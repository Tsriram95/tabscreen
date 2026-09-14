mod audio;
mod kde;
mod protocol;
mod trust;
mod uinput;
mod video;
mod wayland;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use protocol::*;
use video::{Codec, Convert, Encoder, Settings};
use wayland::{Pointer, VirtualOutput};

/// Turn an Android tablet into a second screen with stylus input (KDE Plasma Wayland).
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Address to listen on
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,
    /// TCP port
    #[arg(long, default_value_t = 7741)]
    port: u16,
    /// Video codec (used if the tablet supports it, else falls back to h264)
    #[arg(long, value_enum, default_value_t = Codec::H264)]
    codec: Codec,
    /// Target bitrate in kbit/s
    #[arg(long, default_value_t = 20000)]
    bitrate: u32,
    /// Refresh rate of the virtual monitor (default: what the tablet reports, e.g. 120)
    #[arg(long)]
    refresh: Option<u32>,
    /// Virtual output scale factor (KDE "global scale" for that monitor)
    #[arg(long, default_value_t = 1.5)]
    scale: f64,
    /// Override the virtual output width in pixels (default: what the tablet reports)
    #[arg(long)]
    width: Option<i32>,
    /// Override the virtual output height in pixels
    #[arg(long)]
    height: Option<i32>,
    /// Name of the virtual output as shown in KDE display settings
    #[arg(long, default_value = "TabScreen")]
    name: String,
    /// Colour conversion / upload strategy before the VA-API encoder
    #[arg(long, value_enum, default_value_t = Convert::Auto)]
    convert: Convert,
    /// Hide the mouse cursor in the stream
    #[arg(long)]
    no_cursor: bool,
    /// Do not create the virtual touchscreen (pen only)
    #[arg(long)]
    no_touch: bool,
    /// Never stream audio, whatever the tablet asks for
    #[arg(long)]
    no_audio: bool,
    /// Full GStreamer pipeline override ("{node}" is replaced; must end in `appsink name=sink`)
    #[arg(long)]
    gst: Option<String>,
}

/// Everything that goes down the socket, serialised by one writer thread.
enum Outgoing {
    Video(video::EncodedFrame),
    Audio(audio::AudioChunk),
    Raw(Vec<u8>),
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    if let Err(e) = trust::ensure_registered() {
        log::warn!("could not register with KWin: {e:#}");
    }
    let listener = TcpListener::bind((args.bind.as_str(), args.port))
        .with_context(|| format!("binding {}:{}", args.bind, args.port))?;
    log::info!("listening on {}:{} — connect from the tablet app", args.bind, args.port);
    for conn in listener.incoming() {
        match conn {
            Ok(sock) => {
                let peer = sock.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                log::info!("client connected from {peer}");
                if let Err(e) = handle_client(sock, &args) {
                    log::error!("session with {peer} ended: {e:#}");
                } else {
                    log::info!("session with {peer} ended");
                }
            }
            Err(e) => log::warn!("accept failed: {e}"),
        }
    }
    Ok(())
}

fn read_frame(sock: &mut TcpStream) -> Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 5];
    sock.read_exact(&mut hdr)?;
    let len = u32::from_le_bytes(hdr[1..5].try_into().unwrap()) as usize;
    if len > 1 << 20 {
        bail!("oversized frame ({len} bytes)");
    }
    let mut payload = vec![0u8; len];
    sock.read_exact(&mut payload)?;
    Ok((hdr[0], payload))
}

/// Resources of the "second screen" mode; dropped in reverse order on exit.
struct Screen {
    encoder: Encoder,
    pen: uinput::Pen,
    touch: Option<uinput::Touch>,
    // Dropped last so the encoder is already stopped when the PipeWire stream goes away.
    _vo: VirtualOutput,
}

fn start_screen(hello: &Hello, args: &Args, tx: &mpsc::Sender<Outgoing>) -> Result<Screen> {
    let codec = {
        let supported = |c: Codec| hello.codecs & (1 << (c.wire() - 1)) != 0;
        let wanted = Codec::from_wire(hello.preferred_codec).unwrap_or(args.codec);
        if supported(wanted) {
            wanted
        } else if supported(args.codec) {
            args.codec
        } else {
            Codec::H264
        }
    };
    let width = args.width.unwrap_or(hello.width as i32);
    let height = args.height.unwrap_or(hello.height as i32);
    if width < 64 || height < 64 {
        bail!("implausible display size {width}x{height}");
    }

    // 1. Virtual monitor (KWin adds it to the desktop immediately).
    let pointer = if args.no_cursor { Pointer::Hidden } else { Pointer::Embedded };
    let vo = VirtualOutput::create(&args.name, "Tablet second screen", width, height, args.scale, pointer)?;
    let output_name = kde::resolve_output_name(&vo.name);
    if let Err(e) = kde::set_output_scale(&output_name, args.scale) {
        log::warn!("{e:#}");
    }
    let refresh = args.refresh.unwrap_or(hello.refresh as u32).clamp(30, 240);
    if let Err(e) = kde::set_output_mode(&output_name, width, height, refresh) {
        log::warn!("{e:#} — the monitor stays at 60 Hz");
    }

    // 2. Capture + encode.
    let settings = Settings {
        codec,
        bitrate_kbps: args.bitrate,
        fps: refresh,
        convert: args.convert,
        pipeline_override: args.gst.clone(),
    };
    let vtx = tx.clone();
    let encoder = Encoder::start(vo.node, &settings, move |f| vtx.send(Outgoing::Video(f)).is_ok())?;

    // 3. Input devices, mapped onto the new monitor.
    let pen = uinput::Pen::new(hello.width_mm, hello.height_mm)?;
    let touch = if args.no_touch { None } else { Some(uinput::Touch::new_touchscreen(hello.width_mm, hello.height_mm)?) };
    for node in [pen.event_node(), touch.as_ref().and_then(|t| t.event_node())].into_iter().flatten() {
        if let Err(e) = kde::map_device_to_output(&node, &output_name) {
            log::warn!("{e:#} — map it manually in System Settings > Tablet / Touchscreen");
        }
    }

    tx.send(Outgoing::Raw(stream_config(codec.wire(), width as u16, height as u16, refresh as u16)))?;
    encoder.request_keyframe();
    Ok(Screen { encoder, pen, touch, _vo: vo })
}

fn start_touchpad(_hello: &Hello) -> Result<uinput::Touchpad> {
    // A relative pointer: the server turns finger positions into motion, scroll
    // and taps itself, so it does not depend on libinput's touchpad heuristics.
    uinput::Touchpad::new()
}

fn handle_client(mut sock: TcpStream, args: &Args) -> Result<()> {
    sock.set_nodelay(true)?;
    let (ty, payload) = read_frame(&mut sock)?;
    if ty != MSG_HELLO {
        bail!("expected HELLO, got message type 0x{ty:02x}");
    }
    let version = payload.get(0..2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0);
    if version != VERSION {
        bail!("protocol version mismatch: tablet {version} vs server {VERSION} — update the app or the server");
    }
    let hello = parse_hello(&payload)?;
    log::info!("tablet: {hello:?}");

    let (tx, rx) = mpsc::channel::<Outgoing>();

    // Writer thread: serialises video/audio/control messages onto the socket.
    let mut writer_sock = sock.try_clone()?;
    let write_thread = std::thread::Builder::new().name("net-tx".into()).spawn(move || {
        let mut sent_keyframe = false;
        let (mut frames, mut bytes) = (0u64, 0u64);
        let mut last_report = std::time::Instant::now();
        while let Ok(msg) = rx.recv() {
            let buf = match msg {
                Outgoing::Video(f) => {
                    if !sent_keyframe {
                        if !f.keyframe {
                            continue;
                        }
                        sent_keyframe = true;
                    }
                    frames += 1;
                    video_frame(f.pts_ns, f.keyframe, &f.data)
                }
                Outgoing::Audio(a) => audio_chunk(a.pts_ns, &a.data),
                Outgoing::Raw(v) => v,
            };
            if writer_sock.write_all(&buf).is_err() {
                let _ = writer_sock.shutdown(Shutdown::Both);
                break;
            }
            bytes += buf.len() as u64;
            if last_report.elapsed().as_secs() >= 5 {
                let secs = last_report.elapsed().as_secs_f64();
                log::info!("tx: {:.1} fps, {:.1} Mbit/s", frames as f64 / secs, bytes as f64 * 8.0 / 1e6 / secs);
                frames = 0;
                bytes = 0;
                last_report = std::time::Instant::now();
            }
        }
    })?;

    let mut screen: Option<Screen> = None;
    let mut touchpad: Option<uinput::Touchpad> = None;
    match hello.mode {
        MODE_SCREEN => screen = Some(start_screen(&hello, args, &tx)?),
        MODE_TOUCHPAD => touchpad = Some(start_touchpad(&hello)?),
        other => bail!("unknown mode {other}"),
    }

    // Audio (optional, either mode).
    let mut _audio: Option<(audio::AudioRoute, audio::AudioCapture)> = None;
    if !args.no_audio && hello.audio != AUDIO_NONE {
        let route = if hello.audio == AUDIO_TABLET { audio::AudioRoute::tablet_only() } else { audio::AudioRoute::both() };
        match route {
            Ok(route) => {
                let atx = tx.clone();
                match audio::AudioCapture::start(route.capture_target(), move |c| atx.send(Outgoing::Audio(c)).is_ok()) {
                    Ok(cap) => {
                        tx.send(Outgoing::Raw(audio_config(AUDIO_FORMAT_S16LE, audio::RATE, audio::CHANNELS)))?;
                        _audio = Some((route, cap));
                    }
                    Err(e) => log::warn!("audio capture failed: {e:#}"),
                }
            }
            Err(e) => log::warn!("audio routing failed: {e:#}"),
        }
    }

    // Input loop.
    let tx_ctl = Arc::new(Mutex::new(tx));
    let result = loop {
        if screen.as_ref().map(|s| s._vo.is_closed()).unwrap_or(false) {
            break Err(anyhow!("KWin removed the virtual output"));
        }
        let (ty, payload) = match read_frame(&mut sock) {
            Ok(f) => f,
            Err(e) => break Err(e),
        };
        match ty {
            MSG_PEN => {
                if let Some(s) = screen.as_mut() {
                    for e in parse_pen_batch(&payload)? {
                        s.pen.handle(&e)?;
                    }
                }
            }
            MSG_TOUCH => {
                let events = parse_touch_batch(&payload)?;
                if let Some(tp) = touchpad.as_mut() {
                    for e in &events {
                        tp.handle(e)?;
                    }
                } else if let Some(t) = screen.as_mut().and_then(|s| s.touch.as_mut()) {
                    for e in &events {
                        t.handle(e)?;
                    }
                }
            }
            MSG_PING => {
                let _ = tx_ctl.lock().unwrap().send(Outgoing::Raw(frame(MSG_PONG, &payload)));
            }
            MSG_KEYFRAME_REQUEST => {
                if let Some(s) = screen.as_ref() {
                    s.encoder.request_keyframe();
                }
            }
            other => log::warn!("unknown message type 0x{other:02x} ({} bytes)", payload.len()),
        }
    };

    // Teardown: lift fingers/pen, stop producers (closing the channel ends the writer), release the monitor.
    if let Some(t) = touchpad.as_mut() {
        let _ = t.release_all();
    }
    if let Some(s) = screen.as_mut() {
        if let Some(t) = s.touch.as_mut() {
            let _ = t.release_all();
        }
        let _ = s.pen.handle(&PenEvent { t_ns: 0, action: PEN_CANCEL, tool: TOOL_PEN, buttons: 0, x: 0.0, y: 0.0, pressure: 0.0, tilt_x: 0.0, tilt_y: 0.0, distance: 0.0 });
    }
    let _ = sock.shutdown(Shutdown::Both);
    drop(_audio);
    drop(screen);
    drop(touchpad);
    drop(tx_ctl);
    let _ = write_thread.join();

    match result {
        Err(e) if e.downcast_ref::<std::io::Error>().is_some() => {
            log::info!("client disconnected ({e})");
            Ok(())
        }
        r => r,
    }
}
