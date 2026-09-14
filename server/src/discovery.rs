//! Zero-config discovery over UDP broadcast, so the tablet can find this
//! computer without the user typing an IP. The server answers discovery
//! datagrams on UDP :7742 with its TCP port and hostname. Works on any
//! network the two share, including a USB-tethering link — no Avahi/mDNS
//! daemon required.

use anyhow::{Context, Result};
use std::net::UdpSocket;

pub const DISCOVERY_PORT: u16 = 7742;
const REQUEST: &[u8] = b"TABSCREEN?";
const RESPONSE_MAGIC: &[u8] = b"TABSCREEN!";

/// Spawn a background responder. It lives for the process's lifetime.
pub fn spawn(tcp_port: u16) -> Result<()> {
    let sock = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT))
        .with_context(|| format!("binding UDP :{DISCOVERY_PORT} for discovery"))?;
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "linux".to_string());
    log::info!("discovery responder on UDP :{DISCOVERY_PORT} (host '{hostname}')");

    std::thread::Builder::new().name("discovery".into()).spawn(move || {
        let mut buf = [0u8; 64];
        loop {
            match sock.recv_from(&mut buf) {
                Ok((n, from)) if &buf[..n] == REQUEST => {
                    // magic, u16 tcp port (LE), hostname bytes
                    let mut reply = Vec::with_capacity(RESPONSE_MAGIC.len() + 2 + hostname.len());
                    reply.extend_from_slice(RESPONSE_MAGIC);
                    reply.extend_from_slice(&tcp_port.to_le_bytes());
                    reply.extend_from_slice(hostname.as_bytes());
                    if let Err(e) = sock.send_to(&reply, from) {
                        log::debug!("discovery reply to {from} failed: {e}");
                    } else {
                        log::debug!("answered discovery from {from}");
                    }
                }
                Ok(_) => {} // stray packet
                Err(e) => {
                    log::warn!("discovery socket error: {e}");
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
    })?;
    Ok(())
}
