//! Creates a virtual output in KWin through its `zkde_screencast_unstable_v1`
//! protocol and returns the PipeWire node that carries its frames. The output
//! lives exactly as long as the `VirtualOutput` value (and the Wayland
//! connection) does.

use anyhow::{anyhow, Context, Result};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};

#[allow(non_upper_case_globals, non_camel_case_types, dead_code)]
pub mod kde {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/zkde-screencast-unstable-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/zkde-screencast-unstable-v1.xml");
}

use kde::zkde_screencast_stream_unstable_v1::{self as stream, ZkdeScreencastStreamUnstableV1};
use kde::zkde_screencast_unstable_v1::{self as screencast, ZkdeScreencastUnstableV1};

#[derive(Default)]
struct Shared {
    node: Option<u32>,
    failed: Option<String>,
    closed: bool,
}

struct State {
    shared: Arc<Mutex<Shared>>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZkdeScreencastUnstableV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZkdeScreencastUnstableV1,
        _: screencast::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZkdeScreencastStreamUnstableV1, ()> for State {
    fn event(
        st: &mut Self,
        _: &ZkdeScreencastStreamUnstableV1,
        event: stream::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut s = st.shared.lock().unwrap();
        match event {
            stream::Event::Created { node } => {
                log::info!("KWin created PipeWire node {node} for the virtual output");
                s.node = Some(node);
            }
            stream::Event::Failed { error } => {
                log::error!("KWin refused the virtual output: {error}");
                s.failed = Some(error);
            }
            stream::Event::Closed => {
                log::warn!("KWin closed the virtual output stream");
                s.closed = true;
            }
            _ => {}
        }
    }
}

/// Cursor rendering mode for the stream (values from the protocol's `pointer` enum).
#[derive(Clone, Copy, Debug)]
pub enum Pointer {
    Hidden = 1,
    Embedded = 2,
    Metadata = 4,
}

pub struct VirtualOutput {
    pub node: u32,
    pub name: String,
    stream: ZkdeScreencastStreamUnstableV1,
    conn: Connection,
    shared: Arc<Mutex<Shared>>,
    // Dispatch thread keeps consuming events (in particular `closed`).
    _thread: Option<JoinHandle<()>>,
}

impl VirtualOutput {
    pub fn create(name: &str, description: &str, width: i32, height: i32, scale: f64, pointer: Pointer) -> Result<Self> {
        let conn = Connection::connect_to_env().context("connecting to the Wayland compositor (is this a KDE Wayland session?)")?;
        let (globals, mut queue): (_, EventQueue<State>) = registry_queue_init(&conn)?;
        let qh = queue.handle();

        // Bind at most v5 so KWin still emits the (v6-deprecated) `created` event with a node id;
        // pipewiresrc is happier with node ids than object serials.
        let sc: ZkdeScreencastUnstableV1 = globals
            .bind(&qh, 4..=5, ())
            .map_err(|e| anyhow!("zkde_screencast_unstable_v1 v4+ not offered by the compositor ({e}); KWin >= 5.27 on Wayland is required"))?;

        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut state = State { shared: shared.clone() };

        let stream = sc.stream_virtual_output_with_description(
            name.to_string(),
            description.to_string(),
            width,
            height,
            scale,
            pointer as u32,
            &qh,
            (),
        );
        conn.flush()?;

        let deadline = Instant::now() + Duration::from_secs(5);
        let node = loop {
            queue.blocking_dispatch(&mut state).context("wayland dispatch")?;
            let s = shared.lock().unwrap();
            if let Some(n) = s.node {
                break n;
            }
            if let Some(e) = &s.failed {
                return Err(anyhow!("virtual output creation failed: {e}"));
            }
            if s.closed {
                return Err(anyhow!("stream closed before it was created"));
            }
            if Instant::now() > deadline {
                return Err(anyhow!("timed out waiting for KWin to create the virtual output"));
            }
        };

        let thread = std::thread::Builder::new().name("wayland".into()).spawn(move || {
            while queue.blocking_dispatch(&mut state).is_ok() {
                if state.shared.lock().unwrap().closed {
                    break;
                }
            }
        })?;

        Ok(Self {
            node,
            name: name.to_string(),
            stream,
            conn,
            shared,
            _thread: Some(thread),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.shared.lock().unwrap().closed
    }
}

impl Drop for VirtualOutput {
    fn drop(&mut self) {
        self.stream.close();
        let _ = self.conn.flush();
        log::info!("virtual output '{}' released", self.name);
    }
}
