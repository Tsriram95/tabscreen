# TabScreen

Turn an Android tablet (built for the **Samsung Galaxy Tab S11 Ultra**, works on any Android 11+ tablet)
into a **second monitor with S Pen input**, a **wireless touchpad**, or a **remote speaker** for a Linux
computer running **KDE Plasma on Wayland**.

```
Linux (server, Rust)                                          Tablet (Android app, Kotlin)
────────────────────                                          ────────────────────────────
KWin virtual output ─▶ PipeWire ─▶ VA-API H.264/HEVC/AV1 ───▶ TCP ─▶ MediaCodec ─▶ SurfaceView
PipeWire audio ──────────────────────────────────────────▶ TCP ─▶ AudioTrack
uinput pen + touchscreen, mapped to that output       ◀──────────── S Pen: pressure, tilt,
uinput relative pointer (touchpad mode)               ◀──────────── hover, buttons, eraser; fingers
```

The second screen is a real KWin output: it shows up in *System Settings → Display*, windows move onto it,
and it disappears when the tablet disconnects. No kernel modules, no `xrandr` hacks.

## Features

- **Second screen** at the tablet's native resolution and refresh rate (e.g. 2960×1848 @ 120 Hz), hardware-encoded.
- **S Pen** as a real graphics tablet: pressure, tilt, hover, both barrel buttons, eraser — confined to that monitor.
- **Touchpad mode**: use the tablet as a wireless trackpad (one finger moves, two-finger scroll, tap / two-finger
  tap / three-finger tap = left / right / middle click).
- **Audio routing**: keep sound on the computer, move it to the tablet (computer goes silent), or play on both.
- **USB or Wi-Fi**, selectable video codec (H.264 / HEVC / AV1), adjustable bitrate.

## Quick start

### Computer (one command)

```sh
curl -fsSL https://raw.githubusercontent.com/Tsriram95/tabscreen/main/install.sh | bash
```

This installs dependencies, builds the server, grants `/dev/uinput` access, and enables a per-user
service that listens on port **7741**. (Arch, Debian/Ubuntu and Fedora are auto-detected.)
Log out and back in once after the first install so pen/touch input works (adds you to the `input` group).

From a clone instead:

```sh
git clone https://github.com/Tsriram95/tabscreen && cd tabscreen && ./install.sh
```

### Tablet

Download `tabscreen.apk` from the [Releases page](https://github.com/Tsriram95/tabscreen/releases) and install it
(enable "install from unknown sources"). Open it, pick a **Connection**, then **Use the tablet as**
(Second screen / Touchpad) and **Play computer audio on** (Computer / Tablet / Both), and tap **Connect**.

**Connection** options (no IP typing needed):
- **Wi-Fi (auto-detect)** — tap *Scan*; the app finds computers running the server on the same network by UDP
  broadcast and lists them by hostname. Pick one and connect.
- **USB** — lowest latency, charges the tablet. Two ways:
  - *USB tethering*: turn it on (Settings → Connections → Mobile Hotspot and Tethering), plug in, tap *Scan*.
  - *adb reverse*: with USB debugging on, run `adb reverse tcp:7741 tcp:7741` on the computer; the app uses `127.0.0.1`.
- **Manual IP** — type the address (`ip -4 addr`) and port, for when broadcast is blocked (e.g. some corporate Wi-Fi).

## Running it persistently

The installer sets this up for you. To do it by hand, or to manage it:

```sh
# enable + start now, and on every login
systemctl --user enable --now tabscreen.service

# status / logs / restart / stop
systemctl --user status tabscreen
journalctl --user -u tabscreen -f
systemctl --user restart tabscreen
systemctl --user disable --now tabscreen

# so the service keeps running when you're logged in via display manager but not via ssh:
loginctl enable-linger "$USER"
```

The service must run inside your graphical session (it talks to KWin and PipeWire), which is why it is a
`--user` service tied to `graphical-session.target`, not a system service.

## Building manually

```sh
# server
cd server && cargo build --release && ./target/release/tabscreen-server --help
# app  (needs JDK 17+ and the Android SDK; ANDROID_HOME set)
cd android && ./gradlew assembleRelease   # -> app/build/outputs/apk/release/app-release.apk
```

Server flags: `--codec hevc`, `--bitrate 30000`, `--refresh 60`, `--scale 2`, `--port`, `--no-touch`,
`--no-cursor`, `--no-audio`, `--convert cpu`, `--gst '<pipeline>'`. See `--help`.

## Requirements (computer)

KDE Plasma 6 on Wayland · PipeWire · GStreamer 1.22+ with the pipewire, va and bad plugins · a VA-API driver
(`libva-mesa-driver` for AMD/Intel, `libva-nvidia-driver` for NVIDIA) · Rust · `busctl` · write access to `/dev/uinput`.

## How it hooks into KDE

- On start the server writes `~/.local/share/applications/tabscreen-server.desktop` with
  `X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1` — KWin only offers that privileged protocol to
  executables vouched for by such a desktop file (matched on the canonical `Exec` path).
- KWin names the output `Virtual-<name>`, ignores the scale passed through the protocol, and creates it at
  60 Hz; the server fixes scale and adds/selects a custom mode at the tablet's refresh via `kscreen-doctor`.
- Second-screen touch/pen devices are mapped onto the virtual output through KWin's D-Bus `outputName`.
  The touchpad is a plain relative pointer, so libinput's absolute-touchpad heuristics never interfere.

## Troubleshooting

- **`zkde_screencast_unstable_v1 not offered`** — not a KDE Wayland session (X11 is unsupported).
- **`/dev/uinput: Permission denied`** — udev rule not installed or not in the `input` group (re-login).
- **Black stream / `vah264enc` missing** — check `vainfo` lists an H.264 *encode* profile; try `--convert cpu`;
  on multi-GPU systems the encoder element may be named e.g. `varenderD129h264enc` — use `--gst`.
- **Pen tilt mirrored** — flip `invertTiltX` / `invertTiltY` in `android/.../StreamView.kt`.
- **Laggy over Wi-Fi** — use 5/6 GHz or USB; lower `--bitrate`; try `--codec hevc`.
- **No audio** — pick Tablet or Both in the app; make sure the tablet's media volume is up.

See `PROTOCOL.md` for the wire format. MIT licensed.
