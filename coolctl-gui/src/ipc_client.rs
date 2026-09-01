//! Blocking Unix-socket client for coolctld, matching the framing
//! `coolctld/src/ipc.rs` implements server-side: plain requests are one JSON
//! object; `set_media` is a JSON header line followed by raw frame bytes.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use coolctl_core::{Request, Response, SOCKET_PATH};

pub fn send_request(request: &Request) -> Result<Response, String> {
    let mut stream = UnixStream::connect(SOCKET_PATH).map_err(|e| format!("failed to connect to {SOCKET_PATH}: {e}"))?;

    let body = serde_json::to_vec(request).map_err(|e| format!("failed to encode request: {e}"))?;
    stream.write_all(&body).map_err(|e| format!("write failed: {e}"))?;
    stream.shutdown(std::net::Shutdown::Write).ok();

    let mut response_bytes = Vec::new();
    stream.read_to_end(&mut response_bytes).map_err(|e| format!("read failed: {e}"))?;
    serde_json::from_slice(&response_bytes).map_err(|e| format!("failed to decode response: {e}"))
}

/// `set_media`'s special framing: a JSON header line, then the raw
/// concatenated RGB565 frame bytes directly on the same connection.
pub fn send_media(frame_count: u32, delays_ms: Vec<u64>, raw_frames: &[u8]) -> Result<Response, String> {
    let mut stream = UnixStream::connect(SOCKET_PATH).map_err(|e| format!("failed to connect to {SOCKET_PATH}: {e}"))?;

    let header = Request::SetMedia { frame_count, delays_ms };
    let mut header_json = serde_json::to_vec(&header).map_err(|e| format!("failed to encode request: {e}"))?;
    header_json.push(b'\n');

    stream.write_all(&header_json).map_err(|e| format!("write failed: {e}"))?;
    stream.write_all(raw_frames).map_err(|e| format!("write failed: {e}"))?;
    stream.shutdown(std::net::Shutdown::Write).ok();

    let mut response_bytes = Vec::new();
    stream.read_to_end(&mut response_bytes).map_err(|e| format!("read failed: {e}"))?;
    serde_json::from_slice(&response_bytes).map_err(|e| format!("failed to decode response: {e}"))
}
