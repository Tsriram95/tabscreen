//! Virtual input devices via /dev/uinput: a stylus tablet (pressure, tilt,
//! hover, two barrel buttons, eraser) and a multitouch touchscreen. Written
//! against the raw ioctl ABI so libinput/KWin see an ordinary tablet.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;

use crate::protocol::*;

// <linux/input-event-codes.h>
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const REL_WHEEL_HI_RES: u16 = 0x0b;
const REL_HWHEEL_HI_RES: u16 = 0x0c;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_TOOL_PEN: u16 = 0x140;
const BTN_TOOL_RUBBER: u16 = 0x141;
const BTN_TOUCH: u16 = 0x14a;
const BTN_STYLUS: u16 = 0x14b;
const BTN_STYLUS2: u16 = 0x14c;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_PRESSURE: u16 = 0x18;
const ABS_DISTANCE: u16 = 0x19;
const ABS_TILT_X: u16 = 0x1a;
const ABS_TILT_Y: u16 = 0x1b;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_TOUCH_MAJOR: u16 = 0x30;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const ABS_MT_PRESSURE: u16 = 0x3a;

const INPUT_PROP_DIRECT: u16 = 0x01;

// <linux/uinput.h> ioctls (x86_64/aarch64 encoding)
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_ABS_SETUP: libc::c_ulong = 0x401c_5504;
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_RELBIT: libc::c_ulong = 0x4004_5566;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_SET_ABSBIT: libc::c_ulong = 0x4004_5567;
const UI_SET_PROPBIT: libc::c_ulong = 0x4004_556e;
const UI_GET_SYSNAME_64: libc::c_ulong = 0x8040_552c;

const AXIS_MAX: i32 = 65535;
const PRESSURE_MAX: i32 = 4095;
const DISTANCE_MAX: i32 = 1023;
const TILT_MAX: i32 = 90;
const MT_SLOTS: i32 = 10;

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct InputAbsinfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: InputAbsinfo,
}

#[repr(C)]
struct InputEvent {
    time: libc::timeval,
    type_: u16,
    code: u16,
    value: i32,
}

fn ioctl(fd: i32, req: libc::c_ulong, arg: libc::c_ulong) -> Result<()> {
    let r = unsafe { libc::ioctl(fd, req, arg) };
    if r < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("uinput ioctl 0x{req:x}"));
    }
    Ok(())
}

struct Device {
    file: File,
    /// Kernel sysfs name ("inputN"); used to find the evdev node KWin knows the device by.
    sysname: String,
    pending: Vec<InputEvent>,
}

impl Device {
    fn open(name: &str, product: u16) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")
            .context("opening /dev/uinput (see README: udev rule / input group)")?;
        let mut setup = UinputSetup {
            id: InputId { bustype: 0x06 /* BUS_VIRTUAL */, vendor: 0x7ab5, product, version: 1 },
            name: [0; 80],
            ff_effects_max: 0,
        };
        let n = name.as_bytes().len().min(79);
        setup.name[..n].copy_from_slice(&name.as_bytes()[..n]);
        Ok(Self { file, sysname: String::new(), pending: Vec::new() })
            .and_then(|d| {
                ioctl(d.file.as_raw_fd(), UI_DEV_SETUP, &setup as *const _ as libc::c_ulong)?;
                Ok(d)
            })
    }

    fn set_bit(&self, req: libc::c_ulong, bit: u16) -> Result<()> {
        ioctl(self.file.as_raw_fd(), req, bit as libc::c_ulong)
    }

    fn abs(&self, code: u16, min: i32, max: i32, resolution: i32) -> Result<()> {
        self.set_bit(UI_SET_ABSBIT, code)?;
        let s = UinputAbsSetup {
            code,
            absinfo: InputAbsinfo { value: 0, minimum: min, maximum: max, fuzz: 0, flat: 0, resolution },
        };
        ioctl(self.file.as_raw_fd(), UI_ABS_SETUP, &s as *const _ as libc::c_ulong)
    }

    fn create(&mut self) -> Result<()> {
        ioctl(self.file.as_raw_fd(), UI_DEV_CREATE, 0)?;
        let mut buf = [0u8; 64];
        ioctl(self.file.as_raw_fd(), UI_GET_SYSNAME_64, buf.as_mut_ptr() as libc::c_ulong)?;
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        self.sysname = String::from_utf8_lossy(&buf[..end]).into_owned();
        Ok(())
    }

    fn emit(&mut self, type_: u16, code: u16, value: i32) {
        self.pending.push(InputEvent { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, type_, code, value });
    }

    fn sync(&mut self) -> Result<()> {
        self.emit(EV_SYN, SYN_REPORT, 0);
        let bytes = unsafe {
            std::slice::from_raw_parts(
                self.pending.as_ptr() as *const u8,
                self.pending.len() * std::mem::size_of::<InputEvent>(),
            )
        };
        self.file.write_all(bytes)?;
        self.pending.clear();
        Ok(())
    }

    /// The `eventN` node of this device, as KWin names it on D-Bus.
    fn event_node(&self) -> Option<String> {
        let dir = std::fs::read_dir(format!("/sys/devices/virtual/input/{}", self.sysname)).ok()?;
        dir.filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.starts_with("event"))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY, 0);
    }
}

fn scale(v: f32, max: i32) -> i32 {
    ((v.clamp(0.0, 1.0)) * max as f32).round() as i32
}

pub struct Pen {
    dev: Device,
    in_proximity: bool,
    tool: u16,
    buttons: u8,
}

impl Pen {
    pub fn new(width_mm: f32, height_mm: f32) -> Result<Self> {
        let mut dev = Device::open("TabScreen Pen", 0x0001)?;
        let f = dev.file.as_raw_fd();
        for ev in [EV_KEY, EV_ABS] {
            ioctl(f, UI_SET_EVBIT, ev as libc::c_ulong)?;
        }
        for key in [BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, BTN_STYLUS, BTN_STYLUS2] {
            dev.set_bit(UI_SET_KEYBIT, key)?;
        }
        dev.set_bit(UI_SET_PROPBIT, INPUT_PROP_DIRECT)?;
        let res_x = (AXIS_MAX as f32 / width_mm.max(1.0)).round() as i32;
        let res_y = (AXIS_MAX as f32 / height_mm.max(1.0)).round() as i32;
        dev.abs(ABS_X, 0, AXIS_MAX, res_x)?;
        dev.abs(ABS_Y, 0, AXIS_MAX, res_y)?;
        dev.abs(ABS_PRESSURE, 0, PRESSURE_MAX, 0)?;
        dev.abs(ABS_DISTANCE, 0, DISTANCE_MAX, 0)?;
        dev.abs(ABS_TILT_X, -TILT_MAX, TILT_MAX, 0)?;
        dev.abs(ABS_TILT_Y, -TILT_MAX, TILT_MAX, 0)?;
        dev.create()?;
        log::info!("created uinput tablet '{}' ({})", "TabScreen Pen", dev.event_node().unwrap_or_default());
        Ok(Self { dev, in_proximity: false, tool: BTN_TOOL_PEN, buttons: 0 })
    }

    pub fn event_node(&self) -> Option<String> {
        self.dev.event_node()
    }

    pub fn handle(&mut self, e: &PenEvent) -> Result<()> {
        let tool = if e.tool == TOOL_ERASER { BTN_TOOL_RUBBER } else { BTN_TOOL_PEN };
        // Tool change mid-proximity: leave with the old tool first.
        if self.in_proximity && tool != self.tool {
            self.dev.emit(EV_KEY, self.tool, 0);
            self.dev.sync()?;
            self.in_proximity = false;
        }
        self.tool = tool;

        let d = &mut self.dev;
        match e.action {
            PEN_HOVER_ENTER | PEN_HOVER_MOVE | PEN_DOWN | PEN_MOVE | PEN_UP => {
                if !self.in_proximity {
                    d.emit(EV_KEY, tool, 1);
                    self.in_proximity = true;
                }
                d.emit(EV_ABS, ABS_X, scale(e.x, AXIS_MAX));
                d.emit(EV_ABS, ABS_Y, scale(e.y, AXIS_MAX));
                d.emit(EV_ABS, ABS_TILT_X, e.tilt_x.clamp(-90.0, 90.0).round() as i32);
                d.emit(EV_ABS, ABS_TILT_Y, e.tilt_y.clamp(-90.0, 90.0).round() as i32);
                let touching = matches!(e.action, PEN_DOWN | PEN_MOVE);
                if touching {
                    d.emit(EV_ABS, ABS_PRESSURE, scale(e.pressure, PRESSURE_MAX).max(1));
                    d.emit(EV_ABS, ABS_DISTANCE, 0);
                } else {
                    d.emit(EV_ABS, ABS_PRESSURE, 0);
                    d.emit(EV_ABS, ABS_DISTANCE, scale(e.distance, DISTANCE_MAX).max(1));
                }
                match e.action {
                    PEN_DOWN => d.emit(EV_KEY, BTN_TOUCH, 1),
                    PEN_UP => d.emit(EV_KEY, BTN_TOUCH, 0),
                    _ => {}
                }
                if e.buttons != self.buttons {
                    d.emit(EV_KEY, BTN_STYLUS, (e.buttons & BTN_PRIMARY != 0) as i32);
                    d.emit(EV_KEY, BTN_STYLUS2, (e.buttons & BTN_SECONDARY != 0) as i32);
                    self.buttons = e.buttons;
                }
                d.sync()?;
            }
            PEN_HOVER_EXIT | PEN_CANCEL => {
                if self.in_proximity {
                    d.emit(EV_KEY, BTN_TOUCH, 0);
                    d.emit(EV_ABS, ABS_PRESSURE, 0);
                    if self.buttons != 0 {
                        d.emit(EV_KEY, BTN_STYLUS, 0);
                        d.emit(EV_KEY, BTN_STYLUS2, 0);
                        self.buttons = 0;
                    }
                    d.emit(EV_KEY, tool, 0);
                    d.sync()?;
                    self.in_proximity = false;
                }
            }
            other => log::warn!("unknown pen action {other}"),
        }
        Ok(())
    }
}

/// Absolute multitouch touchscreen, mapped onto the virtual monitor.
pub struct Touch {
    dev: Device,
    /// slot -> client pointer id
    slots: [Option<u8>; MT_SLOTS as usize],
    next_tracking_id: i32,
}

impl Touch {
    pub fn new_touchscreen(width_mm: f32, height_mm: f32) -> Result<Self> {
        let mut dev = Device::open("TabScreen Touch", 0x0002)?;
        let f = dev.file.as_raw_fd();
        for ev in [EV_KEY, EV_ABS] {
            ioctl(f, UI_SET_EVBIT, ev as libc::c_ulong)?;
        }
        dev.set_bit(UI_SET_KEYBIT, BTN_TOUCH)?;
        dev.set_bit(UI_SET_PROPBIT, INPUT_PROP_DIRECT)?;
        let res_x = (AXIS_MAX as f32 / width_mm.max(1.0)).round() as i32;
        let res_y = (AXIS_MAX as f32 / height_mm.max(1.0)).round() as i32;
        dev.abs(ABS_X, 0, AXIS_MAX, res_x)?;
        dev.abs(ABS_Y, 0, AXIS_MAX, res_y)?;
        dev.abs(ABS_MT_SLOT, 0, MT_SLOTS - 1, 0)?;
        dev.abs(ABS_MT_TRACKING_ID, 0, 65535, 0)?;
        dev.abs(ABS_MT_POSITION_X, 0, AXIS_MAX, res_x)?;
        dev.abs(ABS_MT_POSITION_Y, 0, AXIS_MAX, res_y)?;
        dev.abs(ABS_MT_PRESSURE, 0, PRESSURE_MAX, 0)?;
        dev.abs(ABS_MT_TOUCH_MAJOR, 0, AXIS_MAX, 0)?;
        dev.create()?;
        log::info!("created uinput touchscreen ({})", dev.event_node().unwrap_or_default());
        Ok(Self { dev, slots: [None; MT_SLOTS as usize], next_tracking_id: 1 })
    }

    pub fn event_node(&self) -> Option<String> {
        self.dev.event_node()
    }

    fn slot_for(&mut self, id: u8, allocate: bool) -> Option<usize> {
        if let Some(i) = self.slots.iter().position(|s| *s == Some(id)) {
            return Some(i);
        }
        if allocate {
            let i = self.slots.iter().position(|s| s.is_none())?;
            self.slots[i] = Some(id);
            return Some(i);
        }
        None
    }

    fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    pub fn handle(&mut self, e: &TouchEvent) -> Result<()> {
        match e.action {
            TOUCH_DOWN | TOUCH_MOVE => {
                let was_empty = self.active_count() == 0;
                let Some(slot) = self.slot_for(e.id, e.action == TOUCH_DOWN) else { return Ok(()) };
                let d = &mut self.dev;
                d.emit(EV_ABS, ABS_MT_SLOT, slot as i32);
                if e.action == TOUCH_DOWN {
                    let tid = self.next_tracking_id;
                    self.next_tracking_id = (self.next_tracking_id + 1) & 0xffff;
                    d.emit(EV_ABS, ABS_MT_TRACKING_ID, tid);
                }
                let x = scale(e.x, AXIS_MAX);
                let y = scale(e.y, AXIS_MAX);
                d.emit(EV_ABS, ABS_MT_POSITION_X, x);
                d.emit(EV_ABS, ABS_MT_POSITION_Y, y);
                d.emit(EV_ABS, ABS_MT_PRESSURE, scale(e.pressure, PRESSURE_MAX).max(1));
                d.emit(EV_ABS, ABS_MT_TOUCH_MAJOR, scale(e.major, AXIS_MAX));
                if slot == 0 || was_empty {
                    d.emit(EV_ABS, ABS_X, x);
                    d.emit(EV_ABS, ABS_Y, y);
                }
                if was_empty {
                    d.emit(EV_KEY, BTN_TOUCH, 1);
                }
                self.dev.sync()?;
            }
            TOUCH_UP | TOUCH_CANCEL => {
                let Some(slot) = self.slot_for(e.id, false) else { return Ok(()) };
                self.slots[slot] = None;
                let none_left = self.active_count() == 0;
                let d = &mut self.dev;
                d.emit(EV_ABS, ABS_MT_SLOT, slot as i32);
                d.emit(EV_ABS, ABS_MT_TRACKING_ID, -1);
                if none_left {
                    d.emit(EV_KEY, BTN_TOUCH, 0);
                }
                self.dev.sync()?;
            }
            other => log::warn!("unknown touch action {other}"),
        }
        Ok(())
    }

    /// Lift every finger (client disconnected mid-gesture).
    pub fn release_all(&mut self) -> Result<()> {
        for slot in 0..MT_SLOTS as usize {
            if self.slots[slot].take().is_some() {
                self.dev.emit(EV_ABS, ABS_MT_SLOT, slot as i32);
                self.dev.emit(EV_ABS, ABS_MT_TRACKING_ID, -1);
            }
        }
        self.dev.emit(EV_KEY, BTN_TOUCH, 0);
        self.dev.sync()
    }
}

/// A relative-pointer "touchpad": the tablet sends absolute finger positions and
/// the server turns them into pointer motion, two-finger scrolling and taps.
/// Implemented as a plain mouse so libinput's absolute-touchpad heuristics (which
/// discard synthetic devices as "jumping") never get involved.
pub struct Touchpad {
    dev: Device,
    fingers: std::collections::HashMap<u8, (f32, f32)>,
    /// Peak simultaneous fingers during the current gesture (for tap button choice).
    peak: usize,
    down_at: std::time::Instant,
    moved: bool,
    scroll_acc_x: f32,
    scroll_acc_y: f32,
    width_px: f32,
    height_px: f32,
}

const POINTER_GAIN: f32 = 1.6; // pad fraction -> screen fraction-ish; KDE accel refines it
const SCROLL_LINES_PER_PAD: f32 = 60.0;
const TAP_MAX_MS: u128 = 220;
const TAP_MAX_MOVE: f32 = 0.03; // fraction of the pad

impl Touchpad {
    pub fn new() -> Result<Self> {
        let mut dev = Device::open("TabScreen Touchpad", 0x0003)?;
        let f = dev.file.as_raw_fd();
        for ev in [EV_KEY, EV_REL] {
            ioctl(f, UI_SET_EVBIT, ev as libc::c_ulong)?;
        }
        for key in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE] {
            dev.set_bit(UI_SET_KEYBIT, key)?;
        }
        for rel in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES] {
            ioctl(f, UI_SET_RELBIT, rel as libc::c_ulong)?;
        }
        dev.create()?;
        log::info!("created uinput touchpad/mouse ({})", dev.event_node().unwrap_or_default());
        Ok(Self {
            dev,
            fingers: std::collections::HashMap::new(),
            peak: 0,
            down_at: std::time::Instant::now(),
            moved: false,
            scroll_acc_x: 0.0,
            scroll_acc_y: 0.0,
            // The pad reports normalised coords; assume a 16:10 surface for gain shaping.
            width_px: 1600.0,
            height_px: 1000.0,
        })
    }

    pub fn event_node(&self) -> Option<String> {
        self.dev.event_node()
    }

    fn click(&mut self, button: u16) -> Result<()> {
        self.dev.emit(EV_KEY, button, 1);
        self.dev.sync()?;
        self.dev.emit(EV_KEY, button, 0);
        self.dev.sync()
    }

    fn emit_scroll(&mut self, dx: f32, dy: f32) {
        // Natural scrolling: dragging content down (fingers move down) scrolls up.
        self.scroll_acc_y += -dy * SCROLL_LINES_PER_PAD;
        self.scroll_acc_x += dx * SCROLL_LINES_PER_PAD;
        let mut any = false;
        // Hi-res wheel is in 1/120 of a detent; also emit whole detents for older clients.
        if self.scroll_acc_y.abs() >= 0.01 {
            self.dev.emit(EV_REL, REL_WHEEL_HI_RES, (self.scroll_acc_y * 120.0) as i32);
            let detents = self.scroll_acc_y.trunc();
            if detents != 0.0 {
                self.dev.emit(EV_REL, REL_WHEEL, detents as i32);
                self.scroll_acc_y -= detents;
            } else {
                self.scroll_acc_y = 0.0;
            }
            any = true;
        }
        if self.scroll_acc_x.abs() >= 0.01 {
            self.dev.emit(EV_REL, REL_HWHEEL_HI_RES, (self.scroll_acc_x * 120.0) as i32);
            let detents = self.scroll_acc_x.trunc();
            if detents != 0.0 {
                self.dev.emit(EV_REL, REL_HWHEEL, detents as i32);
                self.scroll_acc_x -= detents;
            } else {
                self.scroll_acc_x = 0.0;
            }
            any = true;
        }
        if any {
            let _ = self.dev.sync();
        }
    }

    pub fn handle(&mut self, e: &TouchEvent) -> Result<()> {
        match e.action {
            TOUCH_DOWN => {
                if self.fingers.is_empty() {
                    self.down_at = std::time::Instant::now();
                    self.moved = false;
                    self.peak = 0;
                }
                self.fingers.insert(e.id, (e.x, e.y));
                self.peak = self.peak.max(self.fingers.len());
            }
            TOUCH_MOVE => {
                let Some(&(lx, ly)) = self.fingers.get(&e.id) else { return Ok(()) };
                let (dx, dy) = (e.x - lx, e.y - ly);
                self.fingers.insert(e.id, (e.x, e.y));
                if dx.abs() + dy.abs() > TAP_MAX_MOVE {
                    self.moved = true;
                }
                match self.fingers.len() {
                    1 => {
                        // Only the moving finger drives the pointer.
                        let mx = (dx * self.width_px * POINTER_GAIN).round() as i32;
                        let my = (dy * self.height_px * POINTER_GAIN).round() as i32;
                        if mx != 0 || my != 0 {
                            if mx != 0 {
                                self.dev.emit(EV_REL, REL_X, mx);
                            }
                            if my != 0 {
                                self.dev.emit(EV_REL, REL_Y, my);
                            }
                            self.dev.sync()?;
                        }
                    }
                    2 => self.emit_scroll(dx, dy),
                    _ => {} // 3+ fingers: reserved for tap gestures on lift
                }
            }
            TOUCH_UP | TOUCH_CANCEL => {
                self.fingers.remove(&e.id);
                if self.fingers.is_empty() && e.action == TOUCH_UP {
                    let quick = self.down_at.elapsed().as_millis() <= TAP_MAX_MS;
                    if quick && !self.moved {
                        match self.peak {
                            1 => self.click(BTN_LEFT)?,
                            2 => self.click(BTN_RIGHT)?,
                            3 => self.click(BTN_MIDDLE)?,
                            _ => {}
                        }
                    }
                }
            }
            other => log::warn!("unknown touch action {other}"),
        }
        Ok(())
    }

    pub fn release_all(&mut self) -> Result<()> {
        self.fingers.clear();
        for b in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE] {
            self.dev.emit(EV_KEY, b, 0);
        }
        self.dev.sync()
    }
}
