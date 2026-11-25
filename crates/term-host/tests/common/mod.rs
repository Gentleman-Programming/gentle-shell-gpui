//! A protocol client for the host's tests, over any transport.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::Value;

pub const BIN: &str = env!("CARGO_BIN_EXE_gentle-term-host");

/// A connection to the host: a Unix socket or TCP.
pub trait Conn: Read + Write + Send {
    fn set_timeout(&self, timeout: Duration);
    fn duplicate(&self) -> Box<dyn Conn>;
}

impl Conn for std::net::TcpStream {
    fn set_timeout(&self, timeout: Duration) {
        self.set_read_timeout(Some(timeout)).unwrap();
    }
    fn duplicate(&self) -> Box<dyn Conn> {
        Box::new(self.try_clone().unwrap())
    }
}

#[cfg(unix)]
impl Conn for std::os::unix::net::UnixStream {
    fn set_timeout(&self, timeout: Duration) {
        self.set_read_timeout(Some(timeout)).unwrap();
    }
    fn duplicate(&self) -> Box<dyn Conn> {
        Box::new(self.try_clone().unwrap())
    }
}

pub struct Client {
    pub stream: Box<dyn Conn>,
    reader: BufReader<Box<dyn Conn>>,
    next_id: u64,
    events: VecDeque<Value>,
}

impl Client {
    pub fn new(stream: impl Conn + 'static) -> Self {
        Self {
            reader: BufReader::new(stream.duplicate()),
            stream: Box::new(stream),
            next_id: 0,
            events: VecDeque::new(),
        }
    }

    /// The next line, `None` on a timeout or the end of the connection.
    pub fn read_line(&mut self, timeout: Duration) -> Option<Value> {
        self.stream.set_timeout(timeout);
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(serde_json::from_str(&line).unwrap()),
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => None,
            Err(err) if err.kind() == ErrorKind::ConnectionReset => None,
            Err(err) => panic!("{err}"),
        }
    }

    /// Whether the host has hung up (within `timeout`).
    pub fn closed(&mut self, timeout: Duration) -> bool {
        self.stream.set_timeout(timeout);
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => true,
            Ok(_) => panic!("more from the host: {line}"),
            Err(err) => err.kind() == ErrorKind::ConnectionReset,
        }
    }

    pub fn send(&mut self, request: &Value) {
        let mut line = serde_json::to_vec(request).unwrap();
        line.push(b'\n');
        self.stream.write_all(&line).unwrap();
    }

    /// The whole reply to `request`; events that come first are kept.
    pub fn call(&mut self, mut request: Value) -> Value {
        self.next_id += 1;
        request["id"] = self.next_id.into();
        self.send(&request);
        loop {
            let message = self.read_line(Duration::from_secs(10)).expect("no reply");
            if message.get("ev").is_some() {
                self.events.push_back(message);
            } else {
                assert_eq!(message["id"], self.next_id);
                return message;
            }
        }
    }

    /// The reply's fields, asserting it succeeded.
    pub fn ok(&mut self, request: Value) -> Value {
        let reply = self.call(request.clone());
        assert_eq!(reply["ok"], true, "{request} → {reply}");
        reply
    }

    pub fn error(&mut self, request: Value) -> String {
        let reply = self.call(request);
        assert_eq!(reply["ok"], false, "{reply}");
        reply["error"]["code"].as_str().unwrap().to_string()
    }

    pub fn event(&mut self, timeout: Duration) -> Option<Value> {
        self.events.pop_front().or_else(|| self.read_line(timeout))
    }

    /// Events until `done` says so; panics after 10 s.
    pub fn events_until(&mut self, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out after {seen:?}");
            if let Some(event) = self.event(left) {
                let stop = done(&event);
                seen.push(event);
                if stop {
                    return seen;
                }
            }
        }
    }
}

/// The text of the `output` events among `events`.
pub fn output(events: &[Value]) -> String {
    let bytes: Vec<u8> = events
        .iter()
        .filter(|e| e["ev"] == "output")
        .flat_map(|e| BASE64.decode(e["data"].as_str().unwrap()).unwrap())
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
