//! KWin only offers privileged Wayland protocols (like the screencast one we
//! use to create the virtual output) to executables that a `.desktop` file
//! vouches for via `X-KDE-Wayland-Interfaces`, matched on the canonical `Exec`
//! path. Register this binary that way before touching Wayland.

use anyhow::{Context, Result};
use std::path::PathBuf;

const DESKTOP_ID: &str = "tabscreen-server.desktop";

fn applications_dir() -> PathBuf {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME not set")).join(".local/share"));
    data_home.join("applications")
}

pub fn ensure_registered() -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let dir = applications_dir();
    let path = dir.join(DESKTOP_ID);
    let content = format!(
        "[Desktop Entry]\nType=Application\nName=TabScreen Server\nComment=Second screen server for tablets (registered automatically)\nExec={}\nNoDisplay=true\nX-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1\n",
        exe.display()
    );
    if std::fs::read_to_string(&path).map(|c| c == content).unwrap_or(false) {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    log::info!("registered {} with KWin via {}", exe.display(), path.display());
    // Refresh the KService cache so KWin sees the new entry right away.
    for cmd in ["kbuildsycoca6", "kbuildsycoca5"] {
        if let Ok(st) = std::process::Command::new(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            if st.success() {
                return Ok(());
            }
        }
    }
    log::warn!("could not run kbuildsycoca6; if KWin refuses the virtual output, log out and back in");
    Ok(())
}
