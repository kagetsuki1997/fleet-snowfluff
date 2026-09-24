//! A throwaway one-shot HTTP server for provider tests: serves one canned
//! response, in caller-chosen byte chunks, and captures the request it
//! received. Lets a provider's real `reqwest` path -- request building,
//! header handling, and the streaming line-buffering across arbitrary
//! chunk boundaries -- be tested end to end without a network or an API
//! key. Dependency-free on purpose (`std::net`, one thread).

use std::{
    io::{Read, Write},
    net::TcpListener,
    thread::JoinHandle,
    time::Duration,
};

/// What the server saw.
pub struct CapturedRequest {
    pub request_line: String,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl CapturedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Starts the server and returns `(base_url, handle)`. The handle joins
/// to the captured request once the response has been fully written.
/// `chunks` are written one at a time with a short pause between them,
/// so the client sees them as separate reads.
pub fn serve_once(status: u16, chunks: Vec<Vec<u8>>) -> (String, JoinHandle<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a client connects");
        let captured = read_request(&mut stream);

        let reason = if status == 200 { "OK" } else { "Error" };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: text/event-stream\r\nConnection: \
             close\r\n\r\n"
        )
        .unwrap();
        for chunk in chunks {
            stream.write_all(&chunk).unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_millis(15));
        }
        captured
    });
    (base_url, handle)
}

fn read_request(stream: &mut std::net::TcpStream) -> CapturedRequest {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    // Read up to the blank line ending the headers.
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            break;
        }
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    let length = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).ok();
    CapturedRequest { request_line, headers, body: String::from_utf8_lossy(&body).into_owned() }
}

/// Splits `text` into chunks of `size` bytes (deliberately ignoring line
/// and character boundaries) so a test exercises the client's buffering.
pub fn chunked(text: &str, size: usize) -> Vec<Vec<u8>> {
    text.as_bytes().chunks(size).map(<[u8]>::to_vec).collect()
}
