//! Registering Tortuga as the handler for magnet links and .torrent files.
//!
//! On Windows the installer registers both, but Windows 10/11 won't let an
//! app silently take over a type that the user already assigned to another
//! program (that choice lives in a hash-protected `UserChoice` key). So we:
//! - register ProgIds and "Capabilities" so Tortuga appears in Default Apps,
//! - point the classic `magnet` class at us when nobody else claimed it,
//! - offer a button that opens the Default Apps page for Tortuga.

use serde::Serialize;

#[derive(Serialize)]
pub struct HandlerStatus {
    pub supported: bool,
    pub is_default: bool,
}

#[cfg(windows)]
mod imp {
    use super::HandlerStatus;
    use winreg::{RegKey, enums::*};

    const APP: &str = "Tortuga";
    const MAGNET_PROGID: &str = "Tortuga.Magnet";
    const TORRENT_PROGID: &str = "Tortuga.Torrent";

    fn exe() -> String {
        std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn command() -> String {
        format!("\"{}\" \"%1\"", exe())
    }

    fn set_class(
        classes: &RegKey,
        name: &str,
        description: &str,
        url_protocol: bool,
    ) -> std::io::Result<()> {
        let (k, _) = classes.create_subkey(name)?;
        k.set_value("", &description)?;
        if url_protocol {
            k.set_value("URL Protocol", &"")?;
        }
        let (icon, _) = k.create_subkey("DefaultIcon")?;
        icon.set_value("", &format!("\"{}\",0", exe()))?;
        let (cmd, _) = k.create_subkey(r"shell\open\command")?;
        cmd.set_value("", &command())?;
        Ok(())
    }

    fn user_choice(kind: &str, name: &str) -> Option<String> {
        let path = match kind {
            "url" => format!(
                r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\{name}\UserChoice"
            ),
            _ => format!(
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{name}\UserChoice"
            ),
        };
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(path)
            .ok()?
            .get_value::<String, _>("ProgId")
            .ok()
    }

    fn classic_magnet_is_ours(classes: &RegKey) -> bool {
        classes
            .open_subkey(r"magnet\shell\open\command")
            .and_then(|k| k.get_value::<String, _>(""))
            .map(|v| v.to_lowercase().contains(&exe().to_lowercase()))
            .unwrap_or(false)
    }

    /// Non-invasive registration done on every start (paths may change after updates).
    pub fn register(take_magnet_if_free: bool) -> std::io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (classes, _) = hkcu.create_subkey(r"Software\Classes")?;
        set_class(&classes, MAGNET_PROGID, "URL:Magnet link", true)?;
        set_class(&classes, TORRENT_PROGID, "BitTorrent file", false)?;
        let (owp, _) = classes.create_subkey(r".torrent\OpenWithProgids")?;
        owp.set_value(TORRENT_PROGID, &"")?;

        let (caps, _) = hkcu.create_subkey(format!(r"Software\{APP}\Capabilities"))?;
        caps.set_value("ApplicationName", &APP)?;
        caps.set_value("ApplicationDescription", &"Tortuga torrent client")?;
        let (urls, _) = caps.create_subkey("URLAssociations")?;
        urls.set_value("magnet", &MAGNET_PROGID)?;
        let (files, _) = caps.create_subkey("FileAssociations")?;
        files.set_value(".torrent", &TORRENT_PROGID)?;
        let (reg, _) = hkcu.create_subkey(r"Software\RegisteredApplications")?;
        reg.set_value(APP, &format!(r"Software\{APP}\Capabilities"))?;

        // Classic fallback used when there is no UserChoice for magnet.
        let magnet_free = classes
            .open_subkey(r"magnet\shell\open\command")
            .and_then(|k| k.get_value::<String, _>(""))
            .map(|v| v.trim().is_empty())
            .unwrap_or(true);
        if take_magnet_if_free && (magnet_free || classic_magnet_is_ours(&classes)) {
            set_class(&classes, "magnet", "URL:Magnet link", true)?;
        }
        Ok(())
    }

    pub fn status() -> HandlerStatus {
        let classes = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Classes")
            .ok();
        let magnet = match user_choice("url", "magnet") {
            Some(p) => p == MAGNET_PROGID,
            None => classes.as_ref().is_some_and(classic_magnet_is_ours),
        };
        let torrent = match user_choice("file", ".torrent") {
            Some(p) => p == TORRENT_PROGID,
            // The installer's own ProgId also counts.
            None => true,
        };
        HandlerStatus {
            supported: true,
            is_default: magnet && torrent,
        }
    }

    pub fn make_default() -> std::io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (classes, _) = hkcu.create_subkey(r"Software\Classes")?;
        register(false)?;
        set_class(&classes, "magnet", "URL:Magnet link", true)?;
        // Windows requires the user to confirm the choice; open Default Apps on Tortuga.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &format!("ms-settings:defaultapps?registeredAppUser={APP}")])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::HandlerStatus;

    pub fn register(_take_magnet_if_free: bool) -> std::io::Result<()> {
        Ok(())
    }

    pub fn status() -> HandlerStatus {
        HandlerStatus {
            supported: false,
            is_default: true,
        }
    }

    pub fn make_default() -> std::io::Result<()> {
        Ok(())
    }
}

pub use imp::{make_default, register, status};
