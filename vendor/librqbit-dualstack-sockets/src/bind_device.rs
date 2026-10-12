#[cfg(test)]
pub(crate) mod tests;

use crate::Error;
use std::{ffi::CString, num::NonZeroU32, str::FromStr};

#[derive(Debug, Clone)]
pub struct BindDevice {
    #[allow(unused)]
    index: NonZeroU32,
    #[allow(unused)]
    name: CString,
}

impl BindDevice {
    #[cfg(not(windows))]
    pub fn new_from_name(name: &str) -> crate::Result<Self> {
        let name = CString::new(name).map_err(|_| Error::BindDeviceInvalid)?;

        let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
        let index = NonZeroU32::new(index)
            .ok_or_else(|| Error::BindDeviceInvalidError(std::io::Error::last_os_error()))?;
        Ok(Self { index, name })
    }

    /// Tortuga patch: on Windows accept either an interface index ("12")
    /// or an adapter name as reported by `network-interface` (friendly name).
    #[cfg(windows)]
    pub fn new_from_name(name: &str) -> crate::Result<Self> {
        let index = match name.trim().parse::<u32>() {
            Ok(i) => i,
            Err(_) => {
                use network_interface::{NetworkInterface, NetworkInterfaceConfig};
                NetworkInterface::show()
                    .map_err(|_| Error::BindDeviceInvalid)?
                    .into_iter()
                    .find(|nic| nic.name == name)
                    .map(|nic| nic.index)
                    .ok_or(Error::BindDeviceInvalid)?
            }
        };
        let index = NonZeroU32::new(index).ok_or(Error::BindDeviceInvalid)?;
        let name = CString::new(name).map_err(|_| Error::BindDeviceInvalid)?;
        Ok(Self { index, name })
    }

    pub fn index(&self) -> NonZeroU32 {
        self.index
    }

    pub fn name(&self) -> &str {
        // We constructed from a string so this can't fail
        unsafe { std::str::from_utf8_unchecked(self.name.to_bytes()) }
    }

    #[cfg(target_os = "macos")]
    pub fn bind_sref(&self, sref: &socket2::Socket, is_v6: bool) -> crate::Result<()> {
        if is_v6 {
            sref.bind_device_by_index_v6(Some(self.index))
                .map_err(Error::BindDeviceSetDeviceError)
        } else {
            sref.bind_device_by_index_v4(Some(self.index))
                .map_err(Error::BindDeviceSetDeviceError)
        }
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    pub fn bind_sref(&self, sref: &socket2::Socket, _is_v6: bool) -> crate::Result<()> {
        let name = self.name.as_bytes_with_nul();
        sref.bind_device(Some(name))
            .map_err(Error::BindDeviceSetDeviceError)
    }

    /// Tortuga patch: route outgoing traffic of the socket via the given
    /// interface using IP_UNICAST_IF / IPV6_UNICAST_IF.
    #[cfg(windows)]
    pub fn bind_sref(&self, sref: &socket2::Socket, is_v6: bool) -> crate::Result<()> {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{
            IP_UNICAST_IF, IPPROTO_IP, IPPROTO_IPV6, IPV6_UNICAST_IF, SOCKET, SOCKET_ERROR,
            setsockopt,
        };

        let socket = sref.as_raw_socket() as SOCKET;
        let set = |level: i32, opt: i32, value: u32| -> std::io::Result<()> {
            let res = unsafe {
                setsockopt(
                    socket,
                    level,
                    opt,
                    &value as *const u32 as *const u8,
                    std::mem::size_of::<u32>() as i32,
                )
            };
            if res == SOCKET_ERROR {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        };

        let index = self.index.get();
        if is_v6 {
            set(IPPROTO_IPV6, IPV6_UNICAST_IF, index).map_err(Error::BindDeviceSetDeviceError)?;
            // Dual-stack sockets also carry IPv4-mapped traffic.
            let _ = set(IPPROTO_IP, IP_UNICAST_IF, index.to_be());
        } else {
            // IPv4 expects the index in network byte order.
            set(IPPROTO_IP, IP_UNICAST_IF, index.to_be())
                .map_err(Error::BindDeviceSetDeviceError)?;
        }
        Ok(())
    }
}

impl FromStr for BindDevice {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new_from_name(s)
    }
}
