//! A loopback stand-in for an OpenAI-compatible `/v1/embeddings` endpoint,
//! so contract tests can exercise semantic and blended search end to end
//! through the real provider code with no network access.
//!
//! Vectors are derived deterministically from the input text, so the same
//! text always embeds identically. The endpoint can be switched to answer
//! `500`, to prove a failing provider degrades blended search instead of
//! failing it. Hand-rolled on `std` so the test crate needs no HTTP server
//! dependency. Serving threads are detached: the stub lives for the test
//! process.
//!
//! Shared by every integration test binary through `tests/support/mod.rs`;
//! a binary that does not use it would otherwise warn, which is expected
//! rather than genuine dead code.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

const DIMENSION: usize = 16;

/// A running embeddings stub.
pub struct EmbeddingStub {
    addr: SocketAddr,
    healthy: Arc<AtomicBool>,
}

impl EmbeddingStub {
    /// Starts serving on an ephemeral loopback port. The listener is bound
    /// before this returns, so a client may connect immediately.
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let healthy = Arc::new(AtomicBool::new(true));
        let serving = Arc::clone(&healthy);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let healthy = Arc::clone(&serving);
                std::thread::spawn(move || {
                    // A connection error only ends that connection.
                    let _ = serve_connection(stream, &healthy);
                });
            }
        });
        Ok(Self { addr, healthy })
    }

    /// The full embeddings URL to configure as the provider endpoint.
    #[must_use]
    pub fn endpoint(&self) -> String {
        format!("http://{}/v1/embeddings", self.addr)
    }

    /// Makes every later request fail with `500`.
    pub fn start_failing(&self) {
        self.healthy.store(false, Ordering::SeqCst);
    }
}

fn serve_connection(mut stream: TcpStream, healthy: &AtomicBool) -> std::io::Result<()> {
    loop {
        let Some(body) = read_request_body(&mut stream)? else {
            return Ok(());
        };
        if healthy.load(Ordering::SeqCst) {
            let response = embeddings_response(&body);
            write_response(&mut stream, 200, response.to_string().as_bytes())?;
        } else {
            write_response(&mut stream, 500, br#"{"error":"stub is failing"}"#)?;
        }
    }
}

/// Reads one request and returns its body, or `None` when the client closed
/// the connection.
fn read_request_body(stream: &mut TcpStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Ok(Some(buffer[header_end..].to_vec()))
}

fn embeddings_response(body: &[u8]) -> Value {
    let request: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let inputs = request
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            json!({
                "index": index,
                "embedding": vector_for(input.as_str().unwrap_or_default()),
            })
        })
        .collect();
    json!({ "data": data })
}

/// A deterministic vector in `[-1, 1]` per component, from an FNV-1a hash of
/// the text and the component index.
fn vector_for(text: &str) -> Vec<f32> {
    (0..DIMENSION)
        .map(|component| {
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in text.bytes().chain(component.to_le_bytes()) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
            let unit = f32::from(u16::try_from(hash % 65_536).unwrap_or(0)) / 65_535.0;
            unit.mul_add(2.0, -1.0)
        })
        .collect()
}

fn write_response(stream: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
    let reason = if status == 200 { "OK" } else { "Internal Server Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}
