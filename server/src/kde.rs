//! Small shell-outs to KDE tooling (busctl / kscreen-doctor) — cheaper than
//! pulling in D-Bus and KScreen bindings for two calls.

use anyhow::{Context, Result};
use std::process::Command;
use std::time::Duration;

/// Tell KWin to map an input device onto a specific output
/// (what System Settings > Tablet / Touchscreen does).
pub fn map_device_to_output(event_node: &str, output_name: &str) -> Result<()> {
    let path = format!("/org/kde/KWin/InputDevice/{event_node}");
    // KWin registers the device on D-Bus shortly after libinput sees it; retry briefly.
    let mut last = String::new();
    for _ in 0..40 {
        let out = Command::new("busctl")
            .args(["--user", "set-property", "org.kde.KWin", &path, "org.kde.KWin.InputDevice", "outputName", "s", output_name])
            .output()
            .context("running busctl")?;
        if out.status.success() {
            log::info!("mapped {event_node} -> output '{output_name}'");
            return Ok(());
        }
        last = String::from_utf8_lossy(&out.stderr).trim().to_string();
        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("could not map {event_node} to '{output_name}': {last}")
}

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub name: String,
    pub scale: f64,
    pub current_mode: Option<String>,
    pub modes: Vec<String>,
}

/// Outputs KWin currently has (via kscreen-doctor's JSON dump).
pub fn outputs() -> Vec<OutputInfo> {
    let Ok(out) = Command::new("kscreen-doctor").arg("-j").output() else { return vec![] };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return vec![] };
    json["outputs"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    let modes: Vec<(String, String)> = o["modes"]
                        .as_array()?
                        .iter()
                        .map(|m| (m["id"].as_str().unwrap_or("").to_string(), m["name"].as_str().unwrap_or("").to_string()))
                        .collect();
                    let cur = o["currentModeId"].as_str().unwrap_or("");
                    Some(OutputInfo {
                        name: o["name"].as_str()?.to_string(),
                        scale: o["scale"].as_f64().unwrap_or(1.0),
                        current_mode: modes.iter().find(|(id, _)| id == cur).map(|(_, n)| n.clone()),
                        modes: modes.into_iter().map(|(_, n)| n).collect(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve the compositor-side name of the virtual output we asked for.
/// KWin's DRM backend prefixes virtual outputs with "Virtual-".
pub fn resolve_output_name(requested: &str) -> String {
    let prefixed = format!("Virtual-{requested}");
    for _ in 0..20 {
        let outs = outputs();
        if outs.iter().any(|o| o.name == prefixed) {
            return prefixed;
        }
        if outs.iter().any(|o| o.name == requested) {
            return requested.to_string();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    log::warn!("could not find the virtual output in kscreen; assuming '{prefixed}'");
    prefixed
}

fn kscreen(arg: String) -> Result<()> {
    // kscreen-doctor exits 0 even on parse errors, so check its output too.
    let out = Command::new("kscreen-doctor").arg(&arg).output().context("running kscreen-doctor")?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() || text.contains("Unable") || text.contains("rror") {
        anyhow::bail!("kscreen-doctor {arg}: {}", text.trim());
    }
    Ok(())
}

/// KWin creates virtual outputs with a single 60 Hz mode; add a custom mode at
/// the tablet's refresh rate and switch to it so the screencast (and thus the
/// stream) can run at that rate.
pub fn set_output_mode(output_name: &str, width: i32, height: i32, refresh_hz: u32) -> Result<()> {
    let mode = format!("{width}x{height}@{refresh_hz}");
    let info = outputs().into_iter().find(|o| o.name == output_name);
    if info.as_ref().and_then(|o| o.current_mode.clone()).as_deref() == Some(mode.as_str()) {
        return Ok(());
    }
    if !info.map(|o| o.modes.iter().any(|m| m == &mode)).unwrap_or(false) {
        kscreen(format!("output.{output_name}.addCustomMode.{width}.{height}.{}.reduced", refresh_hz * 1000))?;
        std::thread::sleep(Duration::from_millis(300));
    }
    kscreen(format!("output.{output_name}.mode.{mode}"))?;
    log::info!("set mode {mode} on '{output_name}'");
    Ok(())
}

/// KWin's output configuration store overrides the scale passed through the
/// screencast protocol, so set it explicitly (KWin remembers it per output name).
pub fn set_output_scale(output_name: &str, scale: f64) -> Result<()> {
    kscreen(format!("output.{output_name}.scale.{scale}"))?;
    log::info!("set scale {scale} on '{output_name}'");
    Ok(())
}
