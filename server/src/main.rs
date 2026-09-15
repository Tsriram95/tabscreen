mod audio;
mod discovery;
mod kde;
mod protocol;
mod security;
mod trust;
mod tray;
mod uinput;
mod video;
mod wayland;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Shared handle to the current tablet session, used by the tray to show status
/// and to disconnect on demand.
#[derive(Clone, Default)]
pub struct SessionControl {
    inner: Arc<Mutex<SessionInner>>,
}

#[derive(Default)]
struct SessionInner {
    sock: Option<TcpStream>,
    status: String,
    refresh: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl SessionControl {
    fn refresh(&self) {
        let cb = self.inner.lock().unwrap().refresh.clone();
        if let Some(cb) = cb {
            cb();
        }
    }
    fn set_refresh(&self, f: impl Fn() + Send + Sync + 'static) {
        self.inner.lock().unwrap().refresh = Some(Arc::new(f));
    }
    /// A client connected; register its socket (a clone) so it can be shut down.
    fn connected(&self, sock: TcpStream, status: String) {
        {
            let mut g = self.inner.lock().unwrap();
            g.sock = Some(sock);
            g.status = status;
        }
        self.refresh();
    }
    fn set_status(&self, status: String) {
        self.inner.lock().unwrap().status = status;
        self.refresh();
    }
    fn cleared(&self, status: String) {
        {
            let mut g = self.inner.lock().unwrap();
            g.sock = None;
            g.status = status;
        }
        self.refresh();
    }
    /// Force the active session's socket closed (unblocks its read loop).
    fn disconnect(&self) {
        let sock = self.inner.lock().unwrap().sock.take();
        if let Some(sock) = sock {
            let _ = sock.shutdown(Shutdown::Both);
            log::info!("tray requested disconnect");
        }
    }
    fn is_connected(&self) -> bool {
        self.inner.lock().unwrap().sock.is_some()
    }
    fn status(&self) -> String {
        self.inner.lock().unwrap().status.clone()
    }
}

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
    /// Virtual output scale factor: "auto" (from the tablet's DPI) or a number like 1.5, 2
    #[arg(long, default_value = "auto")]
    scale: String,
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
    /// Do not show a system-tray icon
    #[arg(long)]
    no_tray: bool,
    /// Print the pairing code and exit
    #[arg(long)]
    show_pair: bool,
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

    // TLS certificate + pairing code (persisted in ~/.config/tabscreen).
    let creds = Arc::new(security::Credentials::load_or_create().context("loading credentials")?);
    if args.show_pair {
        // Just print the code and exit (for `tabscreen-server --show-pair`).
        println!("{}", creds.pairing_code_grouped());
        return Ok(());
    }

    if let Err(e) = trust::ensure_registered() {
        log::warn!("could not register with KWin: {e:#}");
    }
    let listener = TcpListener::bind((args.bind.as_str(), args.port))
        .with_context(|| format!("binding {}:{}", args.bind, args.port))?;
    if let Err(e) = discovery::spawn(args.port) {
        log::warn!("auto-discovery unavailable: {e:#} — enter the IP manually in the app");
    }

    let control = SessionControl::default();
    control.set_status("Waiting for a tablet…".into());
    if !args.no_tray {
        if let Err(e) = tray::spawn(control.clone(), creds.pairing_code_grouped()) {
            log::warn!("system-tray icon unavailable: {e} — use `systemctl --user stop tabscreen` or --no-tray");
        }
    }

    log::info!("listening on {}:{} — connect from the tablet app", args.bind, args.port);
    log::info!("pairing code (enter once in the app): {}", creds.pairing_code_grouped());
    for conn in listener.incoming() {
        match conn {
            Ok(sock) => {
                let peer = sock.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                let creds = creds.clone();
                if let Err(e) = handle_client(sock, &args, &control, &creds) {
                    log::warn!("session with {peer} ended: {e:#}");
                }
                control.cleared("Waiting for a tablet…".into());
            }
            Err(e) => log::warn!("accept failed: {e}"),
        }
    }
    Ok(())
}

fn read_frame<R: Read>(sock: &mut R) -> Result<(u8, Vec<u8>)> {
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

/// Pick the KDE scale for the virtual monitor. "auto" targets a comfortable
/// ~155 logical DPI from the tablet's real pixel density, so a dense phone
/// screen gets a larger scale (readable UI) and a big tablet a smaller one,
/// rounded to KDE's 0.25 steps and clamped to a sane range.
fn resolve_scale(arg: &str, hello: &Hello, width: i32, height: i32) -> f64 {
    if let Ok(v) = arg.parse::<f64>() {
        return v.clamp(0.5, 4.0);
    }
    if arg != "auto" {
        log::warn!("unrecognised --scale '{arg}', using auto");
    }
    let diag_px = ((width as f64).powi(2) + (height as f64).powi(2)).sqrt();
    let diag_in = (((hello.width_mm as f64).powi(2) + (hello.height_mm as f64).powi(2)).sqrt() / 25.4).max(0.1);
    let dpi = diag_px / diag_in;
    if !dpi.is_finite() || dpi < 50.0 || dpi > 1000.0 {
        return 1.5; // bogus DPI reported; fall back to a middle-of-the-road scale
    }
    let scale = (dpi / 155.0 * 4.0).round() / 4.0;
    let scale = scale.clamp(1.0, 3.0);
    log::info!("auto scale: {dpi:.0} dpi -> {scale}");
    scale
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
    // H.264/HEVC encoders need even dimensions; some devices report odd sizes.
    let width = args.width.unwrap_or(hello.width as i32) & !1;
    let height = args.height.unwrap_or(hello.height as i32) & !1;
    if width < 64 || height < 64 {
        bail!("implausible display size {width}x{height}");
    }

    let scale = resolve_scale(&args.scale, &hello, width, height);

    // 1. Virtual monitor (KWin adds it to the desktop immediately).
    let pointer = if args.no_cursor { Pointer::Hidden } else { Pointer::Embedded };
    let vo = VirtualOutput::create(&args.name, "Tablet second screen", width, height, scale, pointer)?;
    let output_name = kde::resolve_output_name(&vo.name);
    if let Err(e) = kde::set_output_scale(&output_name, scale) {
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

fn handle_client(
    mut sock: TcpStream,
    args: &Args,
    control: &SessionControl,
    creds: &Arc<security::Credentials>,
) -> Result<()> {
    sock.set_nodelay(true)?;

    // Peek the first byte: a TLS ClientHello starts with 0x16. Anything else is a
    // plaintext discovery probe (kept cheap so the app's subnet scan is fast).
    let mut first = [0u8; 1];
    if sock.peek(&mut first)? == 0 {
        return Ok(());
    }
    if first[0] != 0x16 {
        let (ty, _p) = read_frame(&mut sock)?;
        if ty == MSG_DISCOVER {
            let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "linux".to_string());
            let _ = sock.write_all(&frame(MSG_DISCOVER, hostname.as_bytes()));
        }
        let _ = sock.shutdown(Shutdown::Both);
        return Ok(());
    }

    // TLS + pairing handshake.
    let raw = sock.try_clone()?; // for the tray to force-close the session
    sock.set_read_timeout(None)?;
    let conn = rustls::ServerConnection::new(creds.tls_config.clone()).context("TLS setup")?;
    let mut tls = rustls::StreamOwned::new(conn, sock);
    if !security::server_handshake(&mut tls, creds)? {
        return Ok(()); // was a discovery probe over TLS
    }
    log::info!("tablet paired and authenticated");

    let (ty, payload) = read_frame(&mut tls)?;
    if ty != MSG_HELLO {
        bail!("expected HELLO, got message type 0x{ty:02x}");
    }
    let version = payload.get(0..2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0);
    if version != VERSION {
        bail!("protocol version mismatch: tablet {version} vs server {VERSION} — update the app or the server");
    }
    let hello = parse_hello(&payload)?;
    log::info!("tablet: {hello:?}");

    let peer = raw.peer_addr().map(|a| a.ip().to_string()).unwrap_or_else(|_| "tablet".into());
    let mode_label = if hello.mode == MODE_TOUCHPAD { "Touchpad" } else { "Second screen" };
    control.connected(raw.try_clone()?, format!("Connected: {peer} — {mode_label}"));

    let (tx, rx) = mpsc::channel::<Outgoing>();

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

    // TLS can't be read and written from two threads, so this is a single-threaded
    // event loop: it drains outgoing video/audio, then reads input with a short
    // timeout so writes never wait long behind a blocked read.
    tls.sock.set_read_timeout(Some(std::time::Duration::from_millis(3)))?;
    let mut inbuf: Vec<u8> = Vec::with_capacity(1 << 16);
    let mut tmp = [0u8; 1 << 16];
    let mut sent_keyframe = false;
    let (mut frames, mut bytes) = (0u64, 0u64);
    let mut last_report = std::time::Instant::now();

    let result: Result<()> = loop {
        if screen.as_ref().map(|s| s._vo.is_closed()).unwrap_or(false) {
            break Err(anyhow!("KWin removed the virtual output"));
        }

        // 1. Send everything the producers have queued.
        let mut werr = None;
        loop {
            match rx.try_recv() {
                Ok(msg) => {
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
                    bytes += buf.len() as u64;
                    if let Err(e) = tls.write_all(&buf) {
                        werr = Some(e);
                        break;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if let Some(e) = werr {
            break Err(e.into());
        }
        if let Err(e) = tls.flush() {
            break Err(e.into());
        }
        if last_report.elapsed().as_secs() >= 5 {
            let secs = last_report.elapsed().as_secs_f64();
            log::info!("tx: {:.1} fps, {:.1} Mbit/s", frames as f64 / secs, bytes as f64 * 8.0 / 1e6 / secs);
            frames = 0;
            bytes = 0;
            last_report = std::time::Instant::now();
        }

        // 2. Read whatever input is available.
        match tls.read(&mut tmp) {
            Ok(0) => break Ok(()),
            Ok(n) => inbuf.extend_from_slice(&tmp[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => break Err(e.into()),
        }

        // 3. Dispatch any complete frames.
        let mut pos = 0;
        let mut fatal: Option<anyhow::Error> = None;
        loop {
            if inbuf.len() - pos < 5 {
                break;
            }
            let len = u32::from_le_bytes(inbuf[pos + 1..pos + 5].try_into().unwrap()) as usize;
            if len > 1 << 20 {
                fatal = Some(anyhow!("oversized frame ({len} bytes)"));
                break;
            }
            if inbuf.len() - pos < 5 + len {
                break;
            }
            let ty = inbuf[pos];
            let payload = &inbuf[pos + 5..pos + 5 + len];
            if let Err(e) = dispatch(ty, payload, &mut screen, &mut touchpad, &tx) {
                fatal = Some(e);
                pos += 5 + len;
                break;
            }
            pos += 5 + len;
        }
        if pos > 0 {
            inbuf.drain(0..pos);
        }
        if let Some(e) = fatal {
            break Err(e);
        }
    };

    // Teardown: lift fingers/pen, stop producers, release the monitor.
    if let Some(t) = touchpad.as_mut() {
        let _ = t.release_all();
    }
    if let Some(s) = screen.as_mut() {
        if let Some(t) = s.touch.as_mut() {
            let _ = t.release_all();
        }
        let _ = s.pen.handle(&PenEvent { t_ns: 0, action: PEN_CANCEL, tool: TOOL_PEN, buttons: 0, x: 0.0, y: 0.0, pressure: 0.0, tilt_x: 0.0, tilt_y: 0.0, distance: 0.0 });
    }
    let _ = raw.shutdown(Shutdown::Both);
    drop(_audio);
    drop(screen);
    drop(touchpad);

    match result {
        Err(e) if e.downcast_ref::<std::io::Error>().is_some() => {
            log::info!("client disconnected ({e})");
            Ok(())
        }
        r => r,
    }
}

/// Handle one decoded input frame from the tablet.
fn dispatch(
    ty: u8,
    payload: &[u8],
    screen: &mut Option<Screen>,
    touchpad: &mut Option<uinput::Touchpad>,
    tx: &mpsc::Sender<Outgoing>,
) -> Result<()> {
    match ty {
        MSG_PEN => {
            if let Some(s) = screen.as_mut() {
                for e in parse_pen_batch(payload)? {
                    s.pen.handle(&e)?;
                }
            }
        }
        MSG_TOUCH => {
            let events = parse_touch_batch(payload)?;
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
            let _ = tx.send(Outgoing::Raw(frame(MSG_PONG, payload)));
        }
        MSG_KEYFRAME_REQUEST => {
            if let Some(s) = screen.as_ref() {
                s.encoder.request_keyframe();
            }
        }
        other => log::warn!("unknown message type 0x{other:02x} ({} bytes)", payload.len()),
    }
    Ok(())
}
