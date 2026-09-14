//! Capture the virtual output's PipeWire stream and hardware-encode it with
//! GStreamer (VA-API). Encoded access units are handed to a callback.

use anyhow::{anyhow, Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::sync::Arc;
use std::time::Duration;

use crate::protocol::{CODEC_AV1, CODEC_H264, CODEC_HEVC};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub fn wire(self) -> u8 {
        match self {
            Codec::H264 => CODEC_H264,
            Codec::Hevc => CODEC_HEVC,
            Codec::Av1 => CODEC_AV1,
        }
    }
    pub fn from_wire(v: u8) -> Option<Self> {
        match v {
            CODEC_H264 => Some(Codec::H264),
            CODEC_HEVC => Some(Codec::Hevc),
            CODEC_AV1 => Some(Codec::Av1),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Convert {
    /// Try the VA-API post-processor (zero-copy from DMA-BUF), fall back to CPU.
    Auto,
    Va,
    Cpu,
}

pub struct EncodedFrame {
    pub pts_ns: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

pub struct Settings {
    pub codec: Codec,
    pub bitrate_kbps: u32,
    pub fps: u32,
    pub convert: Convert,
    /// Full pipeline override; must contain `{node}` and end in an appsink named `sink`.
    pub pipeline_override: Option<String>,
}

pub struct Encoder {
    pipeline: gst::Pipeline,
}

fn encoder_chain(codec: Codec, s: &Settings) -> String {
    let gop = s.fps.max(1) * 2;
    let common = format!(
        "rate-control=cbr bitrate={} target-usage=6 key-int-max={gop} b-frames=0 ref-frames=1",
        s.bitrate_kbps
    );
    match codec {
        Codec::H264 => format!(
            "vah264enc {common} ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au,profile=main"
        ),
        Codec::Hevc => format!(
            "vah265enc {common} ! h265parse config-interval=-1 ! video/x-h265,stream-format=byte-stream,alignment=au"
        ),
        Codec::Av1 => format!("vaav1enc {common} ! av1parse ! video/x-av1,stream-format=obu-stream,alignment=tu"),
    }
}

fn pipeline_desc(node: u32, s: &Settings, convert: Convert) -> String {
    if let Some(p) = &s.pipeline_override {
        return p.replace("{node}", &node.to_string());
    }
    let src = format!(
        "pipewiresrc path={node} do-timestamp=true keepalive-time=1000 ! queue max-size-buffers=3 leaky=downstream"
    );
    let conv = match convert {
        Convert::Va => "vapostproc ! video/x-raw(memory:VAMemory),format=NV12".to_string(),
        _ => "videoconvert n-threads=4 ! video/x-raw,format=NV12".to_string(),
    };
    let enc = encoder_chain(s.codec, s);
    format!("{src} ! {conv} ! {enc} ! appsink name=sink sync=false max-buffers=2 drop=true")
}

impl Encoder {
    /// `on_frame` returns false to stop the pipeline.
    pub fn start(node: u32, settings: &Settings, on_frame: impl Fn(EncodedFrame) -> bool + Send + Sync + 'static) -> Result<Self> {
        let on_frame: Arc<dyn Fn(EncodedFrame) -> bool + Send + Sync> = Arc::new(on_frame);
        gst::init()?;
        let attempts: Vec<Convert> = match settings.convert {
            Convert::Auto => vec![Convert::Va, Convert::Cpu],
            c => vec![c],
        };
        let mut last_err = anyhow!("no conversion strategy");
        for conv in attempts {
            match Self::try_start(node, settings, conv, on_frame.clone()) {
                Ok(enc) => return Ok(enc),
                Err(e) => {
                    log::warn!("pipeline with {conv:?} conversion failed: {e:#}");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    fn try_start(node: u32, settings: &Settings, conv: Convert, on_frame: Arc<dyn Fn(EncodedFrame) -> bool + Send + Sync>) -> Result<Self> {
        let desc = pipeline_desc(node, settings, conv);
        log::info!("gst pipeline: {desc}");
        let pipeline = gst::parse::launch(&desc)
            .context("building GStreamer pipeline (missing plugin? see README)")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let sink = pipeline
            .by_name("sink")
            .ok_or_else(|| anyhow!("pipeline has no element named 'sink'"))?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| anyhow!("'sink' is not an appsink"))?;

        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let keyframe = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
                    let pts_ns = buffer.pts().map(|p| p.nseconds()).unwrap_or(0);
                    if !on_frame(EncodedFrame { pts_ns, keyframe, data: map.as_slice().to_vec() }) {
                        return Err(gst::FlowError::Eos);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline.set_state(gst::State::Playing).context("setting pipeline to PLAYING")?;

        // Wait for the pipeline to actually reach PLAYING (or fail), so a
        // missing VA-API path surfaces here instead of as a silent black stream.
        let bus = pipeline.bus().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let (res, _, _) = pipeline.state(gst::ClockTime::from_mseconds(100));
            if let Some(msg) = bus.timed_pop_filtered(gst::ClockTime::from_mseconds(0), &[gst::MessageType::Error]) {
                if let gst::MessageView::Error(e) = msg.view() {
                    let _ = pipeline.set_state(gst::State::Null);
                    return Err(anyhow!("{} ({})", e.error(), e.debug().unwrap_or_default()));
                }
            }
            match res {
                Ok(gst::StateChangeSuccess::Success) => break,
                Ok(_) if std::time::Instant::now() < deadline => continue,
                Ok(_) => {
                    let _ = pipeline.set_state(gst::State::Null);
                    return Err(anyhow!("pipeline did not reach PLAYING in time"));
                }
                Err(e) => {
                    let _ = pipeline.set_state(gst::State::Null);
                    return Err(anyhow!("state change failed: {e}"));
                }
            }
        }

        // Surface later runtime errors in the log.
        let weak = pipeline.downgrade();
        bus.set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Error(e) => log::error!("gst error: {} ({})", e.error(), e.debug().unwrap_or_default()),
                gst::MessageView::Warning(w) => log::warn!("gst warning: {}", w.error()),
                gst::MessageView::Eos(_) => {
                    log::warn!("gst EOS");
                    if let Some(p) = weak.upgrade() {
                        let _ = p.set_state(gst::State::Null);
                    }
                }
                _ => {}
            }
            gst::BusSyncReply::Drop
        });

        log::info!("encoder running with {conv:?} conversion");
        Ok(Self { pipeline })
    }

    /// Ask the encoder for an IDR frame with headers (new client attached).
    pub fn request_keyframe(&self) {
        let ev = gstreamer_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
        self.pipeline.send_event(ev);
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
