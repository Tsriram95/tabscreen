//! Wire protocol shared with the Android app. All integers little-endian.
//!
//! Frame: `u8 type, u32 payload_len, payload`.
//! See ../../PROTOCOL.md for the human-readable spec; keep both in sync.

pub const VERSION: u16 = 2;

// client -> server
pub const MSG_HELLO: u8 = 0x01;
pub const MSG_PEN: u8 = 0x02;
pub const MSG_TOUCH: u8 = 0x03;
pub const MSG_PING: u8 = 0x04;
pub const MSG_KEYFRAME_REQUEST: u8 = 0x05;
pub const MSG_DISCOVER: u8 = 0x06;

// server -> client
pub const MSG_STREAM_CONFIG: u8 = 0x81;
pub const MSG_VIDEO: u8 = 0x82;
pub const MSG_AUDIO_CONFIG: u8 = 0x83;
pub const MSG_PONG: u8 = 0x84;
pub const MSG_AUDIO: u8 = 0x85;

pub const MODE_SCREEN: u8 = 0;
pub const MODE_TOUCHPAD: u8 = 1;

pub const AUDIO_NONE: u8 = 0;
pub const AUDIO_TABLET: u8 = 1;
pub const AUDIO_BOTH: u8 = 2;

pub const AUDIO_FORMAT_S16LE: u8 = 1;

pub const CODEC_H264: u8 = 1;
pub const CODEC_HEVC: u8 = 2;
pub const CODEC_AV1: u8 = 3;

pub const VIDEO_FLAG_KEYFRAME: u8 = 1;

pub const PEN_HOVER_ENTER: u8 = 0;
pub const PEN_HOVER_MOVE: u8 = 1;
pub const PEN_HOVER_EXIT: u8 = 2;
pub const PEN_DOWN: u8 = 3;
pub const PEN_MOVE: u8 = 4;
pub const PEN_UP: u8 = 5;
pub const PEN_CANCEL: u8 = 6;

pub const TOOL_PEN: u8 = 1;
pub const TOOL_ERASER: u8 = 2;

pub const BTN_PRIMARY: u8 = 1;
pub const BTN_SECONDARY: u8 = 2;

pub const TOUCH_DOWN: u8 = 0;
pub const TOUCH_MOVE: u8 = 1;
pub const TOUCH_UP: u8 = 2;
pub const TOUCH_CANCEL: u8 = 3;


#[derive(Debug, Clone)]
pub struct Hello {
    pub version: u16,
    pub width: u16,
    pub height: u16,
    pub refresh: u16,
    pub codecs: u8,
    pub preferred_codec: u8,
    pub width_mm: f32,
    pub height_mm: f32,
    /// MODE_SCREEN or MODE_TOUCHPAD
    pub mode: u8,
    /// AUDIO_NONE / AUDIO_TABLET / AUDIO_BOTH
    pub audio: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct PenEvent {
    pub t_ns: u64,
    pub action: u8,
    pub tool: u8,
    pub buttons: u8,
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    pub tilt_x: f32,
    pub tilt_y: f32,
    pub distance: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct TouchEvent {
    pub t_ns: u64,
    pub action: u8,
    pub id: u8,
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    pub major: f32,
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> anyhow::Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            anyhow::bail!("truncated message");
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> anyhow::Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> anyhow::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> anyhow::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn f32(&mut self) -> anyhow::Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

pub fn parse_hello(p: &[u8]) -> anyhow::Result<Hello> {
    let mut r = Reader::new(p);
    Ok(Hello {
        version: r.u16()?,
        width: r.u16()?,
        height: r.u16()?,
        refresh: r.u16()?,
        codecs: r.u8()?,
        preferred_codec: r.u8()?,
        width_mm: r.f32()?,
        height_mm: r.f32()?,
        mode: r.u8()?,
        audio: r.u8()?,
    })
}

pub fn parse_pen_batch(p: &[u8]) -> anyhow::Result<Vec<PenEvent>> {
    let mut r = Reader::new(p);
    let n = r.u16()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let t_ns = r.u64()?;
        let action = r.u8()?;
        let tool = r.u8()?;
        let buttons = r.u8()?;
        let _pad = r.u8()?;
        v.push(PenEvent {
            t_ns,
            action,
            tool,
            buttons,
            x: r.f32()?,
            y: r.f32()?,
            pressure: r.f32()?,
            tilt_x: r.f32()?,
            tilt_y: r.f32()?,
            distance: r.f32()?,
        });
    }
    Ok(v)
}

pub fn parse_touch_batch(p: &[u8]) -> anyhow::Result<Vec<TouchEvent>> {
    let mut r = Reader::new(p);
    let n = r.u16()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let t_ns = r.u64()?;
        let action = r.u8()?;
        let id = r.u8()?;
        let _pad = r.u16()?;
        v.push(TouchEvent {
            t_ns,
            action,
            id,
            x: r.f32()?,
            y: r.f32()?,
            pressure: r.f32()?,
            major: r.f32()?,
        });
    }
    Ok(v)
}

/// Build a framed message.
pub fn frame(ty: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + payload.len());
    v.push(ty);
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

pub fn stream_config(codec: u8, width: u16, height: u16, fps: u16) -> Vec<u8> {
    let mut p = Vec::with_capacity(7);
    p.push(codec);
    p.extend_from_slice(&width.to_le_bytes());
    p.extend_from_slice(&height.to_le_bytes());
    p.extend_from_slice(&fps.to_le_bytes());
    frame(MSG_STREAM_CONFIG, &p)
}

pub fn audio_config(format: u8, rate: u32, channels: u8) -> Vec<u8> {
    let mut p = Vec::with_capacity(6);
    p.push(format);
    p.extend_from_slice(&rate.to_le_bytes());
    p.push(channels);
    frame(MSG_AUDIO_CONFIG, &p)
}

pub fn audio_chunk(pts_ns: u64, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + 8 + data.len());
    v.push(MSG_AUDIO);
    v.extend_from_slice(&((8 + data.len()) as u32).to_le_bytes());
    v.extend_from_slice(&pts_ns.to_le_bytes());
    v.extend_from_slice(data);
    v
}

pub fn video_frame(pts_ns: u64, keyframe: bool, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + 9 + data.len());
    v.push(MSG_VIDEO);
    v.extend_from_slice(&((9 + data.len()) as u32).to_le_bytes());
    v.extend_from_slice(&pts_ns.to_le_bytes());
    v.push(if keyframe { VIDEO_FLAG_KEYFRAME } else { 0 });
    v.extend_from_slice(data);
    v
}
