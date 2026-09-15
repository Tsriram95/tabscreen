# TabScreen wire protocol (v1)

Single TCP connection, tablet → computer. All integers little-endian, floats IEEE-754 single.

```
frame := u8 type, u32 payload_len, payload[payload_len]
```

## Tablet → server

| type | name   | payload |
|------|--------|---------|
| 0x01 | HELLO  | `u16 version(=1)`, `u16 width_px`, `u16 height_px`, `u16 refresh_hz`, `u8 codecs` (bit0 H.264, bit1 HEVC, bit2 AV1), `u8 preferred_codec` (0 = server default), `f32 width_mm`, `f32 height_mm` |
| 0x02 | PEN    | `u16 count`, then `count` × 36 bytes: `u64 t_ns`, `u8 action`, `u8 tool`, `u8 buttons`, `u8 pad`, `f32 x`, `f32 y`, `f32 pressure`, `f32 tilt_x_deg`, `f32 tilt_y_deg`, `f32 distance` |
| 0x03 | TOUCH  | `u16 count`, then `count` × 28 bytes: `u64 t_ns`, `u8 action`, `u8 pointer_id`, `u16 pad`, `f32 x`, `f32 y`, `f32 pressure`, `f32 major` |
| 0x04 | PING   | `u64 t_ns` (echoed back) |
| 0x06 | DISCOVER | empty — TCP discovery probe; server replies with a DISCOVER frame carrying its UTF-8 hostname, then closes (works when UDP :7742 is firewalled) |
| 0x05 | KEYFRAME_REQUEST | empty — ask the encoder for an IDR with headers (sent when the decoder is (re)created) |

`x`, `y` are normalised to `0..1` of the streamed image. `pressure`, `distance`, `major` are `0..1`.

Pen actions: 0 hover_enter, 1 hover_move, 2 hover_exit, 3 down, 4 move, 5 up, 6 cancel.
Pen tools: 1 pen, 2 eraser. Buttons: bit0 primary barrel button, bit1 secondary.
Touch actions: 0 down, 1 move, 2 up, 3 cancel.

## Server → tablet

| type | name          | payload |
|------|---------------|---------|
| 0x81 | STREAM_CONFIG | `u8 codec` (1 H.264, 2 HEVC, 3 AV1), `u16 width`, `u16 height`, `u16 fps` |
| 0x82 | VIDEO         | `u64 pts_ns`, `u8 flags` (bit0 keyframe), then one access unit (Annex-B for H.264/HEVC with SPS/PPS repeated before every IDR; OBU temporal unit for AV1) |
| 0x84 | PONG          | `u64 t_ns` |

The server sends STREAM_CONFIG once after HELLO, then only keyframes until the client is known to have received one (it simply skips delta frames before the first IDR).

## Discovery (UDP)

Zero-config discovery over UDP port **7742**, independent of the TCP protocol above.

- Client → broadcast `TABSCREEN?` (10 ASCII bytes) to `255.255.255.255:7742` and each interface broadcast address.
- Server → unicast reply: `TABSCREEN!` (10 bytes), `u16 tcp_port` (LE), then the UTF-8 hostname.

Needs no mDNS/Avahi and works over any shared broadcast domain, including a USB-tethering link.

## Transport security

The stream port speaks TLS. The server peeks the first byte: `0x16` (TLS ClientHello) starts a secure
session; anything else is treated as a plaintext discovery probe (kept cheap for subnet scans).

After the TLS handshake, before HELLO:

| type | name           | payload |
|------|----------------|---------|
| 0x07 | AUTH_CHALLENGE | server -> `nonce_s` (16 bytes) |
| 0x08 | AUTH_RESPONSE  | client -> `nonce_c` (16), `mac_c` (32) |
| 0x09 | AUTH_OK        | server -> `mac_s` (32) |
| 0x0a | AUTH_FAIL      | server -> reason (UTF-8) |

`token` = base32-decoded pairing code. `fp` = SHA-256 of the server's TLS leaf certificate (each side derives
it from the negotiated cert). `mac_c = HMAC-SHA256(token, "tabscreen-client" || nonce_s || nonce_c || fp)`;
`mac_s` uses the label `"tabscreen-server"`. Binding `fp` into the MAC defeats a man-in-the-middle: a proxy
with its own certificate cannot produce a valid MAC without the code.
