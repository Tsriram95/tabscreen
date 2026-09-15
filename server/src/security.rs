//! Transport security: TLS for encryption plus a pairing-code challenge so only
//! a paired tablet can connect. The pairing code is the HMAC key; the challenge
//! also mixes in the server certificate fingerprint, so a man-in-the-middle that
//! presents its own certificate cannot complete the handshake without the code.

use anyhow::{anyhow, bail, Context, Result};
use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;

use crate::protocol::*;

type HmacSha256 = Hmac<Sha256>;

const TOKEN_BYTES: usize = 10; // 80-bit pairing code -> 16 base32 chars
const NONCE_LEN: usize = 16;

fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".config"));
    base.join("tabscreen")
}

/// Server credentials: TLS certificate + the pairing token, persisted so the
/// tablet only has to pair once.
pub struct Credentials {
    pub tls_config: Arc<ServerConfig>,
    pub cert_fingerprint: [u8; 32],
    token: Vec<u8>,
    pub pairing_code: String,
}

impl Credentials {
    pub fn load_or_create() -> Result<Self> {
        let dir = config_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let cert_path = dir.join("tls_cert.der");
        let key_path = dir.join("tls_key.der");

        let (cert_der, key_der) = if cert_path.exists() && key_path.exists() {
            (std::fs::read(&cert_path)?, std::fs::read(&key_path)?)
        } else {
            let c = rcgen::generate_simple_self_signed(vec!["tabscreen".to_string()])
                .context("generating self-signed certificate")?;
            let cert_der = c.cert.der().to_vec();
            let key_der = c.key_pair.serialize_der();
            std::fs::write(&cert_path, &cert_der)?;
            write_private(&key_path, &key_der)?;
            (cert_der, key_der)
        };

        let cert_fingerprint: [u8; 32] = Sha256::digest(&cert_der).into();

        let certs = vec![CertificateDer::from(cert_der)];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));
        let tls_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("building TLS config")?;

        // Pairing token: persisted base32 code.
        let token_path = dir.join("pair_token");
        let pairing_code = if let Ok(s) = std::fs::read_to_string(&token_path) {
            s.trim().to_string()
        } else {
            let mut raw = [0u8; TOKEN_BYTES];
            rand::thread_rng().fill_bytes(&mut raw);
            let code = BASE32_NOPAD.encode(&raw);
            write_private(&token_path, code.as_bytes())?;
            code
        };
        let token = BASE32_NOPAD
            .decode(pairing_code.as_bytes())
            .map_err(|_| anyhow!("corrupt pairing token; delete {}", token_path.display()))?;

        Ok(Self { tls_config: Arc::new(tls_config), cert_fingerprint, token, pairing_code })
    }

    /// Human-friendly grouping of the pairing code, e.g. ABCD-EFGH-IJKL-MNOP.
    pub fn pairing_code_grouped(&self) -> String {
        self.pairing_code
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("-")
    }

    fn mac(&self, label: &[u8], nonce_s: &[u8], nonce_c: &[u8]) -> [u8; 32] {
        let mut m = HmacSha256::new_from_slice(&self.token).expect("hmac key");
        m.update(label);
        m.update(nonce_s);
        m.update(nonce_c);
        m.update(&self.cert_fingerprint);
        m.finalize().into_bytes().into()
    }
}

fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    std::fs::write(path, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Run the server side of the pairing handshake over an established TLS stream.
/// Returns Ok(true) when authenticated, Ok(false) when the peer only wanted a
/// discovery reply (already answered here), Err on failure/mismatch.
pub fn server_handshake<S: Read + Write>(stream: &mut S, creds: &Credentials) -> Result<bool> {
    let mut nonce_s = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_s);
    write_frame(stream, MSG_AUTH_CHALLENGE, &nonce_s)?;

    let (ty, payload) = read_frame(stream)?;
    if ty == MSG_DISCOVER {
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "linux".to_string());
        write_frame(stream, MSG_DISCOVER, hostname.as_bytes())?;
        return Ok(false);
    }
    if ty != MSG_AUTH_RESPONSE {
        bail!("expected auth response, got 0x{ty:02x}");
    }
    if payload.len() != NONCE_LEN + 32 {
        bail!("malformed auth response");
    }
    let nonce_c = &payload[..NONCE_LEN];
    let mac_c = &payload[NONCE_LEN..];
    let expect_c = creds.mac(b"tabscreen-client", &nonce_s, nonce_c);
    // Constant-time compare.
    if !bool::from(subtle_eq(mac_c, &expect_c)) {
        write_frame(stream, MSG_AUTH_FAIL, b"pairing code mismatch")?;
        bail!("client failed pairing (wrong or missing code)");
    }
    let mac_s = creds.mac(b"tabscreen-server", &nonce_s, nonce_c);
    write_frame(stream, MSG_AUTH_OK, &mac_s)?;
    Ok(true)
}

/// Minimal constant-time byte-slice comparison.
fn subtle_eq(a: &[u8], b: &[u8]) -> subtle_bool {
    if a.len() != b.len() {
        return subtle_bool(0);
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    subtle_bool(if diff == 0 { 1 } else { 0 })
}

#[allow(non_camel_case_types)]
struct subtle_bool(u8);
impl From<subtle_bool> for bool {
    fn from(b: subtle_bool) -> bool {
        b.0 == 1
    }
}

fn read_frame<S: Read>(s: &mut S) -> Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 5];
    s.read_exact(&mut hdr)?;
    let len = u32::from_le_bytes(hdr[1..5].try_into().unwrap()) as usize;
    if len > 1 << 16 {
        bail!("oversized handshake frame");
    }
    let mut p = vec![0u8; len];
    s.read_exact(&mut p)?;
    Ok((hdr[0], p))
}

fn write_frame<S: Write>(s: &mut S, ty: u8, payload: &[u8]) -> Result<()> {
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(ty);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    s.write_all(&buf)?;
    s.flush()?;
    Ok(())
}
