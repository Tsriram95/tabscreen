//! KDE/freedesktop system-tray icon (StatusNotifierItem) with a Disconnect
//! action for the current tablet session. Runs entirely over D-Bus; if no tray
//! host is present (e.g. a headless run) spawning simply fails and the server
//! carries on without one.

use crate::SessionControl;
use ksni::blocking::TrayMethods;
use ksni::menu::StandardItem;
use ksni::{MenuItem, ToolTip, Tray};

struct TabTray {
    control: SessionControl,
}

impl Tray for TabTray {
    fn id(&self) -> String {
        "tabscreen".into()
    }
    fn title(&self) -> String {
        "TabScreen".into()
    }
    fn icon_name(&self) -> String {
        // A generic display icon present in every KDE/Breeze icon theme.
        "video-display".into()
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: "video-display".into(),
            title: "TabScreen".into(),
            description: self.control.status(),
            icon_pixmap: Vec::new(),
        }
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let connected = self.control.is_connected();
        vec![
            StandardItem {
                label: self.control.status(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Disconnect tablet".into(),
                icon_name: "network-disconnect".into(),
                enabled: connected,
                activate: Box::new(|t: &mut Self| t.control.disconnect()),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit TabScreen server".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| {
                    t.control.disconnect();
                    std::process::exit(0);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Start the tray. Returns Ok even when unsupported — the server does not need it.
pub fn spawn(control: SessionControl) -> anyhow::Result<()> {
    let handle = TabTray { control: control.clone() }
        .spawn()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Let status changes re-render the tray (title/tooltip/menu) live.
    control.set_refresh(move || {
        handle.update(|_| {});
    });
    log::info!("system tray icon active (right-click it to disconnect)");
    Ok(())
}
