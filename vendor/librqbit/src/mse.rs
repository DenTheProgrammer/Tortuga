//! BitTorrent Message Stream Encryption (MSE/PE), the connecting side.
//!
//! An encrypted connection looks like random bytes from the first packet on,
//! so DPI can't match the plaintext "\x13BitTorrent protocol" header. Some
//! VPN servers silently drop every connection with that header.
//! Spec: <https://wiki.vuze.com/w/Message_Stream_Encryption>.

use std::{
    io::IoSliceMut,
    pin::Pin,
    task::{Context, Poll, ready},
};

use anyhow::{Context as _, bail};
use librqbit_core::hash_id::Id20;
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use sha1w::ISha1;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::{
    type_aliases::{BoxAsyncReadVectored, BoxAsyncWrite},
    vectored_traits::AsyncReadVectored,
};

/// Whether outgoing peer connections are encrypted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncryptionMode {
    /// Encrypt; reconnect in plaintext if the peer doesn't support encryption.
    #[default]
    Prefer,
    /// Encrypted connections only.
    Require,
    /// Plaintext only.
    Disable,
}

const PRIME_HEX: &[u8] = b"FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74\
020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576\
625E7EC6F44C42E9A63A36210000000000090563";
const KEY_LEN: usize = 96;
const VC: [u8; 8] = [0; 8];
const CRYPTO_RC4: u32 = 0x02;
const MAX_PAD: usize = 512;

#[derive(Clone)]
pub(crate) struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    fn new(key: &[u8]) -> Self {
        let mut s = [0u8; 256];
        for (i, v) in s.iter_mut().enumerate() {
            *v = i as u8;
        }
        let mut j = 0u8;
        for i in 0..256 {
            j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
            s.swap(i, j as usize);
        }
        Self { s, i: 0, j: 0 }
    }

    /// RC4 as MSE uses it: the first 1024 bytes of keystream are discarded.
    fn for_mse(key: &[u8]) -> Self {
        let mut rc4 = Self::new(key);
        rc4.apply(&mut [0u8; 1024]);
        rc4
    }

    fn apply(&mut self, data: &mut [u8]) {
        let (mut i, mut j) = (self.i, self.j);
        let s = &mut self.s;
        for b in data {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            *b ^= s[s[i as usize].wrapping_add(s[j as usize]) as usize];
        }
        self.i = i;
        self.j = j;
    }
}

fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = sha1w::Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finish()
}

fn key_bytes(n: &BigUint) -> [u8; KEY_LEN] {
    let b = n.to_bytes_be();
    let mut out = [0u8; KEY_LEN];
    out[KEY_LEN - b.len()..].copy_from_slice(&b);
    out
}

fn random_pad() -> Vec<u8> {
    let mut len = [0u8; 1];
    rand::fill(&mut len);
    let mut pad = vec![0u8; len[0] as usize % 64];
    rand::fill(&mut pad[..]);
    pad
}

struct DhKeys {
    public: [u8; KEY_LEN],
    private: BigUint,
}

impl DhKeys {
    fn generate() -> Self {
        let mut x = [0u8; 20];
        rand::fill(&mut x);
        let private = BigUint::from_bytes_be(&x);
        let prime = BigUint::parse_bytes(PRIME_HEX, 16).unwrap();
        let public = key_bytes(&BigUint::from(2u8).modpow(&private, &prime));
        Self { public, private }
    }

    fn shared_secret(&self, their_public: &[u8]) -> [u8; KEY_LEN] {
        let prime = BigUint::parse_bytes(PRIME_HEX, 16).unwrap();
        key_bytes(&BigUint::from_bytes_be(their_public).modpow(&self.private, &prime))
    }
}

async fn read_more(read: &mut BoxAsyncReadVectored, buf: &mut Vec<u8>) -> anyhow::Result<()> {
    let mut tmp = [0u8; 1024];
    let n = read.read(&mut tmp).await?;
    if n == 0 {
        bail!("peer closed the connection");
    }
    buf.extend_from_slice(&tmp[..n]);
    Ok(())
}

/// Error from the encryption handshake.
#[derive(Debug, thiserror::Error)]
#[error("{0:#}")]
pub struct HandshakeError(#[from] anyhow::Error);

/// Run the MSE handshake as the connecting side and wrap the streams.
///
/// `initial_payload` (the BitTorrent handshake) is sent inside the encrypted
/// handshake. Returns once the peer's first `expect_reply` payload bytes have
/// arrived, so the caller can read the peer's BitTorrent handshake in one go.
pub(crate) async fn initiate(
    mut read: BoxAsyncReadVectored,
    mut write: BoxAsyncWrite,
    info_hash: Id20,
    initial_payload: &[u8],
    expect_reply: usize,
) -> Result<(BoxAsyncReadVectored, BoxAsyncWrite), HandshakeError> {
    let keys = DhKeys::generate();
    let mut msg = keys.public.to_vec();
    msg.extend(random_pad());
    write.write_all(&msg).await.context("sending public key")?;

    let mut their_public = [0u8; KEY_LEN];
    read.read_exact(&mut their_public)
        .await
        .context("reading public key")?;
    let s = keys.shared_secret(&their_public);
    let skey = &info_hash.0[..];

    let mut enc = Rc4::for_mse(&sha1(&[b"keyA", &s, skey]));
    let mut dec = Rc4::for_mse(&sha1(&[b"keyB", &s, skey]));

    let mut msg = sha1(&[b"req1", &s]).to_vec();
    let req2 = sha1(&[b"req2", skey]);
    let req3 = sha1(&[b"req3", &s]);
    msg.extend(req2.iter().zip(req3).map(|(a, b)| a ^ b));
    let encrypted_from = msg.len();
    msg.extend(VC);
    msg.extend(CRYPTO_RC4.to_be_bytes());
    msg.extend(0u16.to_be_bytes()); // no PadC
    msg.extend(
        u16::try_from(initial_payload.len())
            .context("initial payload too long")?
            .to_be_bytes(),
    );
    msg.extend(initial_payload);
    enc.apply(&mut msg[encrypted_from..]);
    write.write_all(&msg).await.context("sending handshake")?;

    // The peer's reply starts after up to 512 bytes of padding; find it by
    // the encrypted verification constant.
    let mut vc_marker = VC;
    dec.apply(&mut vc_marker);
    let mut buf = Vec::with_capacity(1024);
    let vc_at = loop {
        if let Some(p) = buf.windows(VC.len()).position(|w| w == vc_marker) {
            break p;
        }
        if buf.len() >= MAX_PAD + VC.len() {
            return Err(anyhow::anyhow!("peer doesn't speak MSE").into());
        }
        read_more(&mut read, &mut buf)
            .await
            .context("waiting for the peer's reply")?;
    };
    let mut rest = buf.split_off(vc_at + VC.len());
    dec.apply(&mut rest);

    // crypto_select (4 bytes), len(PadD) (2 bytes), PadD, payload.
    let mut need = 6;
    let mut pad_len = None;
    loop {
        if rest.len() >= need {
            match pad_len {
                None => {
                    let select = u32::from_be_bytes(rest[..4].try_into().unwrap());
                    if select != CRYPTO_RC4 {
                        return Err(anyhow::anyhow!("peer selected crypto {select:#x}").into());
                    }
                    let pad = u16::from_be_bytes(rest[4..6].try_into().unwrap()) as usize;
                    if pad > MAX_PAD {
                        return Err(anyhow::anyhow!("PadD too long: {pad}").into());
                    }
                    pad_len = Some(pad);
                    need = 6 + pad + expect_reply;
                    continue;
                }
                Some(pad) => {
                    rest.drain(..6 + pad);
                    break;
                }
            }
        }
        let old = rest.len();
        read_more(&mut read, &mut rest)
            .await
            .context("reading the peer's reply")?;
        dec.apply(&mut rest[old..]);
    }

    Ok((
        Box::new(EncryptedReader {
            inner: read,
            cipher: dec,
            pending: rest,
            pos: 0,
        }),
        Box::new(EncryptedWriter {
            inner: write,
            cipher: enc,
            scratch: Vec::new(),
        }),
    ))
}

struct EncryptedReader {
    inner: BoxAsyncReadVectored,
    cipher: Rc4,
    /// Already decrypted bytes read during the handshake.
    pending: Vec<u8>,
    pos: usize,
}

impl EncryptedReader {
    fn take_pending(&mut self, out: &mut [u8]) -> usize {
        let n = (self.pending.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        if self.pos == self.pending.len() {
            self.pending = Vec::new();
            self.pos = 0;
        }
        n
    }
}

impl AsyncRead for EncryptedReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.pending.is_empty() {
            let n = this.take_pending(buf.initialize_unfilled());
            buf.advance(n);
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        this.cipher.apply(&mut buf.filled_mut()[before..]);
        Poll::Ready(Ok(()))
    }
}

impl AsyncReadVectored for EncryptedReader {
    fn poll_read_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        vec: &mut [IoSliceMut<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if !this.pending.is_empty() {
            let mut n = 0;
            for slice in vec.iter_mut() {
                if this.pending.is_empty() {
                    break;
                }
                n += this.take_pending(slice);
            }
            return Poll::Ready(Ok(n));
        }
        let n = ready!(Pin::new(&mut this.inner).poll_read_vectored(cx, vec))?;
        let mut left = n;
        for slice in vec.iter_mut() {
            if left == 0 {
                break;
            }
            let k = left.min(slice.len());
            this.cipher.apply(&mut slice[..k]);
            left -= k;
        }
        Poll::Ready(Ok(n))
    }
}

struct EncryptedWriter {
    inner: BoxAsyncWrite,
    cipher: Rc4,
    scratch: Vec<u8>,
}

impl AsyncWrite for EncryptedWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        // RC4 is a stream cipher: only the bytes the socket accepted may
        // advance the keystream, the caller retries the rest.
        let mut next = this.cipher.clone();
        this.scratch.clear();
        this.scratch.extend_from_slice(buf);
        next.apply(&mut this.scratch);
        let n = ready!(Pin::new(&mut this.inner).poll_write(cx, &this.scratch))?;
        if n == buf.len() {
            this.cipher = next;
        } else {
            this.scratch.truncate(n);
            this.cipher.apply(&mut this.scratch);
        }
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectored_traits::{AsyncReadVectoredExt, AsyncReadVectoredIntoCompat};

    #[test]
    fn rc4_known_vector() {
        let mut data = *b"Plaintext";
        Rc4::new(b"Key").apply(&mut data);
        assert_eq!(data, [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
    }

    /// Minimal receiving side of MSE, as other clients implement it.
    async fn respond(
        mut read: impl AsyncRead + Unpin,
        mut write: impl AsyncWrite + Unpin,
        info_hash: Id20,
        reply: &[u8],
    ) -> (Vec<u8>, Rc4, Rc4) {
        let keys = DhKeys::generate();
        let mut ya = [0u8; KEY_LEN];
        read.read_exact(&mut ya).await.unwrap();
        let mut msg = keys.public.to_vec();
        msg.extend([7u8; 33]); // PadB
        write.write_all(&msg).await.unwrap();

        let s = keys.shared_secret(&ya);
        let skey = &info_hash.0[..];
        let req1 = sha1(&[b"req1", &s]);
        let mut buf = Vec::new();
        let at = loop {
            if let Some(p) = buf.windows(20).position(|w| w == req1) {
                break p;
            }
            let mut tmp = [0u8; 256];
            let n = read.read(&mut tmp).await.unwrap();
            buf.extend_from_slice(&tmp[..n]);
        };
        let mut rest = buf.split_off(at + 40);
        let mut dec = Rc4::for_mse(&sha1(&[b"keyA", &s, skey]));
        let mut enc = Rc4::for_mse(&sha1(&[b"keyB", &s, skey]));
        while rest.len() < 14 {
            let mut tmp = [0u8; 256];
            let n = read.read(&mut tmp).await.unwrap();
            rest.extend_from_slice(&tmp[..n]);
        }
        dec.apply(&mut rest[..14]);
        assert_eq!(&rest[..8], &VC);
        assert_eq!(
            u32::from_be_bytes(rest[8..12].try_into().unwrap()),
            CRYPTO_RC4
        );
        let pad_c = u16::from_be_bytes(rest[12..14].try_into().unwrap()) as usize;
        rest.drain(..14);
        while rest.len() < pad_c + 2 {
            let mut tmp = [0u8; 256];
            let n = read.read(&mut tmp).await.unwrap();
            rest.extend_from_slice(&tmp[..n]);
        }
        dec.apply(&mut rest[..pad_c + 2]);
        let ia_len = u16::from_be_bytes(rest[pad_c..pad_c + 2].try_into().unwrap()) as usize;
        rest.drain(..pad_c + 2);
        while rest.len() < ia_len {
            let mut tmp = [0u8; 256];
            let n = read.read(&mut tmp).await.unwrap();
            rest.extend_from_slice(&tmp[..n]);
        }
        dec.apply(&mut rest);
        let ia = rest[..ia_len].to_vec();

        let mut msg = VC.to_vec();
        msg.extend(CRYPTO_RC4.to_be_bytes());
        msg.extend(3u16.to_be_bytes());
        msg.extend([1, 2, 3]);
        msg.extend(reply);
        enc.apply(&mut msg);
        write.write_all(&msg).await.unwrap();
        (ia, enc, dec)
    }

    #[tokio::test]
    async fn handshake_and_streams_roundtrip() {
        let info_hash = Id20::new([42; 20]);
        let (a, b) = tokio::io::duplex(1 << 16);
        let (ar, aw) = tokio::io::split(a);
        let (mut br, mut bw) = tokio::io::split(b);

        let peer = tokio::spawn(async move {
            let (ia, mut enc, mut dec) =
                respond(&mut br, &mut bw, info_hash, b"their handshake").await;
            assert_eq!(ia, b"our handshake");
            let mut got = [0u8; 5];
            br.read_exact(&mut got).await.unwrap();
            dec.apply(&mut got);
            assert_eq!(&got, b"hello");
            let mut msg = *b"world, and more";
            enc.apply(&mut msg);
            bw.write_all(&msg).await.unwrap();
        });

        let (mut r, mut w) = initiate(
            Box::new(ar.into_vectored_compat()),
            Box::new(aw),
            info_hash,
            b"our handshake",
            b"their handshake".len(),
        )
        .await
        .unwrap();

        let mut first = [0u8; 64];
        let n = r.read(&mut first).await.unwrap();
        assert_eq!(&first[..n], b"their handshake");
        w.write_all(b"hello").await.unwrap();

        // Decryption must follow the byte order across vectored slices.
        let mut out = [0u8; 15];
        let mut got = 0;
        while got < out.len() {
            let split = 6usize.saturating_sub(got);
            let (x, y) = out[got..].split_at_mut(split);
            got += r
                .read_vectored(&mut [IoSliceMut::new(x), IoSliceMut::new(y)])
                .await
                .unwrap();
        }
        assert_eq!(&out, b"world, and more");
        peer.await.unwrap();
    }
}
