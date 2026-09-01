//! Unix socket IPC server. Every request is a single JSON header line (`\n`- or
//! EOF-terminated), with one exception: `set_media` is followed by its raw
//! RGB565 frame bytes directly on the same connection (see `Request::SetMedia`'s
//! doc comment in coolctl-core for why). One `Response` is written back per
//! connection, then it closes — same overall shape as the validated Python
//! reference's protocol.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use coolctl_core::{Request, Response, PANEL_HEIGHT, PANEL_WIDTH, SOCKET_PATH};

use crate::config;
use crate::media_store;
use crate::state::{DisplayState, MediaSequence, Mode, SharedState};
use crate::usb::Lm360;

const FRAME_BYTES: usize = (PANEL_WIDTH * PANEL_HEIGHT * 2) as usize;

pub fn spawn(state: SharedState, device: Arc<Mutex<Lm360>>) {
    std::thread::spawn(move || run(state, device));
}

fn run(state: SharedState, device: Arc<Mutex<Lm360>>) {
    let _ = std::fs::remove_file(SOCKET_PATH);

    let listener = match UnixListener::bind(SOCKET_PATH) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ipc: failed to bind {SOCKET_PATH}: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::set_permissions(SOCKET_PATH, std::fs::Permissions::from_mode(0o666)) {
        eprintln!("ipc: failed to chmod socket: {e}");
    }
    println!("ipc: listening on {SOCKET_PATH}");

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => handle_client(stream, &state, &device),
            Err(e) => eprintln!("ipc: accept error: {e}"),
        }
    }
}

fn handle_client(stream: UnixStream, state: &SharedState, device: &Arc<Mutex<Lm360>>) {
    let write_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ipc: failed to clone stream: {e}");
            return;
        }
    };
    let mut reader = BufReader::new(stream);

    let mut header_line = String::new();
    if let Err(e) = reader.read_line(&mut header_line) {
        eprintln!("ipc: read error: {e}");
        return;
    }

    let request = match serde_json::from_str::<Request>(header_line.trim_end()) {
        Ok(r) => r,
        Err(e) => {
            send_response(write_stream, Response::Error { message: format!("bad request: {e}") });
            return;
        }
    };

    let response = match request {
        Request::SetMedia { frame_count, delays_ms } => {
            let expected_len = FRAME_BYTES * frame_count as usize;
            let mut raw_frames = vec![0u8; expected_len];
            match reader.read_exact(&mut raw_frames) {
                Ok(()) => handle_set_media(frame_count, delays_ms, raw_frames, state),
                Err(e) => Response::Error { message: format!("failed to read media payload: {e}") },
            }
        }
        other => handle_request(other, state, device),
    };

    send_response(write_stream, response);
}

fn send_response(mut stream: UnixStream, response: Response) {
    if let Ok(bytes) = serde_json::to_vec(&response) {
        let _ = stream.write_all(&bytes);
    }
}

fn handle_set_media(frame_count: u32, delays_ms: Vec<u64>, raw_frames: Vec<u8>, state: &SharedState) -> Response {
    if delays_ms.len() != frame_count as usize {
        return Response::Error { message: "delays_ms length doesn't match frame_count".to_string() };
    }

    if let Err(e) = media_store::save(frame_count, &delays_ms, &raw_frames) {
        return Response::Error { message: format!("failed to persist media: {e}") };
    }

    let frames: Vec<Vec<u8>> = raw_frames.chunks_exact(FRAME_BYTES).map(|c| c.to_vec()).collect();
    let sequence = Arc::new(MediaSequence { frames, delays_ms });

    let mut s = state.lock().unwrap();
    s.media = Some(sequence);
    s.media_frame_index = 0;
    s.media_frame_started_at = Instant::now();
    s.mode = Mode::Media;
    config::save(&s);
    Response::Ok
}

fn handle_request(req: Request, state: &SharedState, device: &Arc<Mutex<Lm360>>) -> Response {
    match req {
        Request::Solid { color } => {
            let mut s = state.lock().unwrap();
            s.mode = Mode::Solid;
            s.solid_color = color;
            config::save(&s);
            Response::Ok
        }
        Request::Monitor => {
            let mut s = state.lock().unwrap();
            s.mode = Mode::Monitor;
            config::save(&s);
            Response::Ok
        }
        Request::SetOrientation { orientation } => {
            let mut s = state.lock().unwrap();
            s.orientation = orientation;
            config::save(&s);
            Response::Ok
        }
        Request::BrightnessUp => device_result(device.lock().unwrap().brightness_up()),
        Request::BrightnessDown => device_result(device.lock().unwrap().brightness_down()),
        Request::ZenToggle => device_result(device.lock().unwrap().zen_mode_toggle()),
        Request::GetStatus => {
            let s = state.lock().unwrap();
            Response::Status { mode: mode_name(&s).to_string(), orientation: s.orientation }
        }
        Request::SetMedia { .. } => unreachable!("handled before dispatch in handle_client"),
    }
}

fn mode_name(state: &DisplayState) -> &'static str {
    match state.mode {
        Mode::Monitor => "monitor",
        Mode::Solid => "solid",
        Mode::Media => "media",
    }
}

fn device_result(result: rusb::Result<()>) -> Response {
    match result {
        Ok(()) => Response::Ok,
        Err(e) => Response::Error { message: e.to_string() },
    }
}
