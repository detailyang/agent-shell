use crate::protocol::{Request, Response};

pub const MAX_HANDSHAKE_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

pub fn request_frame(req: &Request) -> Result<Vec<u8>, String> {
    let data = serde_json::to_vec(req).map_err(|e| format!("serialize: {}", e))?;
    let mut frame = Vec::with_capacity(4 + data.len());
    frame.extend_from_slice(&(data.len() as u32).to_be_bytes());
    frame.extend_from_slice(&data);
    Ok(frame)
}

pub fn decode_response(buf: &[u8]) -> Result<Response, String> {
    serde_json::from_slice(buf).map_err(|e| format!("parse handshake: {}", e))
}

/// Remove DSR (Device Status Report, ESC[6n) sequences from a byte stream.
/// These cause the client terminal to emit CPR responses that, in writable
/// attach mode, get forwarded to the PTY and misinterpreted as keystrokes.
pub fn strip_dsr(data: &[u8]) -> Vec<u8> {
    const DSR: &[u8] = b"\x1b[6n";
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if i + DSR.len() <= data.len() && &data[i..i + DSR.len()] == DSR {
            i += DSR.len();
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

pub fn initial_output(resp: &Response) -> Vec<u8> {
    resp.output
        .as_ref()
        .and_then(|s| base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s).ok())
        .map(|bytes| strip_dsr(&bytes))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_dsr_removes_query() {
        assert_eq!(strip_dsr(b"a\x1b[6nb"), b"ab");
    }

    #[test]
    fn strip_dsr_keeps_other_sequences() {
        assert_eq!(strip_dsr(b"\x1b[31mred"), b"\x1b[31mred");
    }
}
