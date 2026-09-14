//! Audio routing to the tablet. "Tablet" mode creates a dedicated PipeWire
//! sink, makes it the default (so the laptop goes silent) and captures it;
//! "both" mode just taps the current default sink's monitor. Raw S16LE PCM is
//! sent — on a LAN/USB link 1.5 Mbit/s is nothing, and it avoids codec latency.

use anyhow::{anyhow, Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::process::Command;

pub const RATE: u32 = 48000;
pub const CHANNELS: u8 = 2;
const SINK_NAME: &str = "tabscreen";

pub struct AudioChunk {
    pub pts_ns: u64,
    pub data: Vec<u8>,
}

fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl").args(args).output().context("running pactl (libpulse missing?)")?;
    if !out.status.success() {
        anyhow::bail!("pactl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Owns the routing change for the session; restores everything on drop.
pub struct AudioRoute {
    target: String,
    module_id: Option<String>,
    previous_default: Option<String>,
}

impl AudioRoute {
    /// Play only on the tablet: new sink becomes the default output.
    pub fn tablet_only() -> Result<Self> {
        let previous_default = pactl(&["get-default-sink"]).ok();
        // A stale sink from a crashed session would collide; drop it first.
        if let Ok(list) = pactl(&["list", "short", "modules"]) {
            for line in list.lines().filter(|l| l.contains(&format!("sink_name={SINK_NAME}"))) {
                if let Some(id) = line.split_whitespace().next() {
                    let _ = pactl(&["unload-module", id]);
                }
            }
        }
        let module_id = pactl(&[
            "load-module",
            "module-null-sink",
            &format!("sink_name={SINK_NAME}"),
            "sink_properties=device.description=TabScreen",
        ])?;
        pactl(&["set-default-sink", SINK_NAME])?;
        log::info!("audio: routed default output to the tablet (was {:?})", previous_default);
        Ok(Self { target: SINK_NAME.to_string(), module_id: Some(module_id), previous_default })
    }

    /// Play on both: capture whatever the default sink is playing.
    pub fn both() -> Result<Self> {
        let target = pactl(&["get-default-sink"])?;
        log::info!("audio: mirroring default sink '{target}' to the tablet");
        Ok(Self { target, module_id: None, previous_default: None })
    }

    pub fn capture_target(&self) -> &str {
        &self.target
    }
}

impl Drop for AudioRoute {
    fn drop(&mut self) {
        if let Some(prev) = &self.previous_default {
            let _ = pactl(&["set-default-sink", prev]);
        }
        if let Some(id) = &self.module_id {
            let _ = pactl(&["unload-module", id]);
        }
        if self.module_id.is_some() {
            log::info!("audio: restored default output");
        }
    }
}

pub struct AudioCapture {
    pipeline: gst::Pipeline,
}

impl AudioCapture {
    pub fn start(target: &str, on_chunk: impl Fn(AudioChunk) -> bool + Send + Sync + 'static) -> Result<Self> {
        gst::init()?;
        let desc = format!(
            "pipewiresrc target-object={target} do-timestamp=true \
             stream-properties=\"props,stream.capture.sink=true,node.latency=480/48000,media.name=TabScreen\" \
             ! audio/x-raw,format=S16LE,rate={RATE},channels={CHANNELS},layout=interleaved \
             ! audioconvert ! audioresample \
             ! appsink name=sink sync=false max-buffers=8 drop=true"
        );
        log::info!("gst audio pipeline: {desc}");
        let pipeline = gst::parse::launch(&desc)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let sink = pipeline
            .by_name("sink")
            .ok_or_else(|| anyhow!("no appsink"))?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| anyhow!("'sink' is not an appsink"))?;
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let pts_ns = buffer.pts().map(|p| p.nseconds()).unwrap_or(0);
                    if !on_chunk(AudioChunk { pts_ns, data: map.as_slice().to_vec() }) {
                        return Err(gst::FlowError::Eos);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline.set_state(gst::State::Playing).context("starting audio capture")?;
        Ok(Self { pipeline })
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
