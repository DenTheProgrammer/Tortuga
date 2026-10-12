//! Finding installed video players and opening stream URLs in them.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct PlayerInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub path: Option<PathBuf>,
}

struct Known {
    id: &'static str,
    name: &'static str,
    /// Executable names to look up in PATH.
    exe: &'static [&'static str],
    /// Paths relative to Program Files / Program Files (x86) / LocalAppData.
    windows: &'static [&'static str],
    /// (registry key under HKCU/HKLM, value name) holding the exe or install dir.
    #[cfg_attr(not(windows), allow(dead_code))]
    registry: &'static [(&'static str, &'static str)],
    macos: &'static [&'static str],
    /// Argument used to set the window/media title.
    title_arg: Option<&'static str>,
}

const KNOWN: &[Known] = &[
    Known {
        id: "vlc",
        name: "VLC",
        exe: &["vlc"],
        windows: &[r"VideoLAN\VLC\vlc.exe"],
        registry: &[(r"SOFTWARE\VideoLAN\VLC", "")],
        macos: &["/Applications/VLC.app/Contents/MacOS/VLC"],
        title_arg: Some("--meta-title="),
    },
    Known {
        id: "mpv",
        name: "mpv",
        exe: &["mpv"],
        windows: &[r"mpv\mpv.exe", r"Programs\mpv\mpv.exe"],
        registry: &[],
        macos: &[
            "/Applications/mpv.app/Contents/MacOS/mpv",
            "/opt/homebrew/bin/mpv",
            "/usr/local/bin/mpv",
        ],
        title_arg: Some("--force-media-title="),
    },
    Known {
        id: "mpc-hc",
        name: "MPC-HC",
        exe: &["mpc-hc64", "mpc-hc"],
        windows: &[
            r"MPC-HC\mpc-hc64.exe",
            r"MPC-HC\mpc-hc.exe",
            r"K-Lite Codec Pack\MPC-HC64\mpc-hc64.exe",
            r"K-Lite Codec Pack\MPC-HC\mpc-hc.exe",
        ],
        registry: &[(r"Software\MPC-HC\MPC-HC", "ExePath")],
        macos: &[],
        title_arg: None,
    },
    Known {
        id: "mpc-be",
        name: "MPC-BE",
        exe: &["mpc-be64", "mpc-be"],
        windows: &[r"MPC-BE x64\mpc-be64.exe", r"MPC-BE\mpc-be.exe"],
        registry: &[(r"Software\MPC-BE", "ExePath")],
        macos: &[],
        title_arg: None,
    },
    Known {
        id: "potplayer",
        name: "PotPlayer",
        exe: &["PotPlayerMini64", "PotPlayerMini"],
        windows: &[
            r"DAUM\PotPlayer\PotPlayerMini64.exe",
            r"DAUM\PotPlayer\PotPlayerMini.exe",
        ],
        registry: &[
            (r"Software\DAUM\PotPlayer64", "ProgramPath"),
            (r"Software\DAUM\PotPlayer", "ProgramPath"),
        ],
        macos: &[],
        title_arg: None,
    },
    Known {
        id: "iina",
        name: "IINA",
        exe: &["iina"],
        windows: &[],
        registry: &[],
        macos: &["/Applications/IINA.app/Contents/MacOS/iina-cli"],
        title_arg: None,
    },
];

fn in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        vec![".exe".into()]
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path).find_map(|dir| {
        exts.iter()
            .map(|e| dir.join(format!("{name}{e}")))
            .find(|p| p.is_file())
    })
}

#[cfg(windows)]
fn from_registry(key: &str, value: &str, exe_names: &[&str]) -> Option<PathBuf> {
    use winreg::{RegKey, enums::*};
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let Ok(k) = RegKey::predef(root).open_subkey(key) else {
            continue;
        };
        let Ok(v) = k.get_value::<String, _>(value) else {
            continue;
        };
        let p = PathBuf::from(v.trim_matches('"'));
        if p.is_file() {
            return Some(p);
        }
        if p.is_dir() {
            for exe in exe_names {
                let c = p.join(format!("{exe}.exe"));
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn app_paths(exe: &str) -> Option<PathBuf> {
    from_registry(
        &format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}.exe"),
        "",
        &[],
    )
}

fn find(k: &Known) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        for (key, value) in k.registry {
            if let Some(p) = from_registry(key, value, k.exe) {
                return Some(p);
            }
        }
        for exe in k.exe {
            if let Some(p) = app_paths(exe) {
                return Some(p);
            }
        }
        let roots: Vec<PathBuf> = [
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "LOCALAPPDATA",
        ]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect();
        for root in &roots {
            for rel in k.windows {
                let p = root.join(rel);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            for exe in k.exe {
                let p = Path::new(&home).join(format!(r"scoop\apps\{}\current\{exe}.exe", k.id));
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    for p in k.macos {
        if Path::new(p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    k.exe.iter().find_map(|e| in_path(e))
}

pub fn detect() -> Vec<PlayerInfo> {
    KNOWN
        .iter()
        .filter(|k| {
            // Only list players that make sense on this OS.
            if cfg!(windows) {
                !k.windows.is_empty()
            } else if cfg!(target_os = "macos") {
                !k.macos.is_empty() || k.id == "vlc" || k.id == "mpv"
            } else {
                matches!(k.id, "vlc" | "mpv")
            }
        })
        .map(|k| PlayerInfo {
            id: k.id,
            name: k.name,
            path: find(k),
        })
        .collect()
}

fn spawn(exe: &Path, args: &[String]) -> anyhow::Result<()> {
    let mut cmd = Command::new(exe);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("can't start {}: {e}", exe.display()))?;
    Ok(())
}

/// Write a one-entry playlist so the OS opens it with the default media player
/// (opening the http URL directly would launch the browser instead).
fn playlist(url: &str, title: &str) -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir().join("tortuga");
    std::fs::create_dir_all(&dir)?;
    let safe: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    let path = dir.join(format!("{safe}.m3u"));
    std::fs::write(&path, format!("#EXTM3U\n#EXTINF:-1,{title}\n{url}\n"))?;
    Ok(path)
}

pub fn open(url: &str, title: &str, player: &str, custom_path: Option<&str>) -> anyhow::Result<()> {
    match player {
        "system" | "builtin" => {
            let pl = playlist(url, title)?;
            open_with_default(&pl)
        }
        "custom" => {
            let exe = custom_path
                .filter(|p| !p.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("player path is not set"))?;
            spawn(Path::new(exe), &[url.to_owned()])
        }
        id => {
            let k = KNOWN
                .iter()
                .find(|k| k.id == id)
                .ok_or_else(|| anyhow::anyhow!("unknown player {id}"))?;
            let exe = custom_path
                .map(PathBuf::from)
                .filter(|p| p.is_file())
                .or_else(|| find(k))
                .ok_or_else(|| anyhow::anyhow!("{} is not installed", k.name))?;
            let mut args = vec![url.to_owned()];
            if let Some(t) = k.title_arg {
                args.push(format!("{t}{title}"));
            }
            #[cfg(target_os = "macos")]
            if k.id == "iina" {
                return spawn(&exe, &[url.to_owned()]);
            }
            spawn(&exe, &args)
        }
    }
}

fn open_with_default(path: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        // `start` resolves file associations; the empty title argument is required.
        let mut cmd = Command::new("cmd");
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.args(["/C", "start", ""])
            .arg(path)
            .creation_flags(CREATE_NO_WINDOW);
        cmd.spawn()?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn()?;
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
}
