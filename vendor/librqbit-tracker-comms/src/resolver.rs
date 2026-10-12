//! Tortuga patch: a pluggable hostname resolver for UDP trackers, so the app
//! can fall back to DNS-over-HTTPS when the system DNS filters tracker names.

use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc};

use parking_lot::RwLock;

pub type ResolveFuture = Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>;
pub type Resolver = Arc<dyn Fn(String, u16) -> ResolveFuture + Send + Sync>;

static RESOLVER: RwLock<Option<Resolver>> = RwLock::new(None);

/// Replace the system resolver used for UDP tracker hostnames.
pub fn set_resolver(resolver: Option<Resolver>) {
    *RESOLVER.write() = resolver;
}

pub(crate) async fn lookup(name: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
    let custom = RESOLVER.read().clone();
    match custom {
        Some(r) => r(name.to_owned(), port).await,
        None => Ok(tokio::net::lookup_host((name, port)).await?.collect()),
    }
}
