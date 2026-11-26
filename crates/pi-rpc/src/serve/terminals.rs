//! Persistent terminals on a Gentle Shell server (`/api/terminals*`), which
//! live in its terminal host (`gentle-term-host`) and outlive this app, and
//! that host's status and install (`/api/term-host`).

use std::sync::atomic::AtomicBool;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use sha2::Digest as _;

use super::{Body, Events, ServeClient, ServeError, malformed, query, read_json};

/// One of this device's terminals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    /// `t_…`.
    pub id: String,
    pub cwd: String,
    pub cols: u16,
    pub rows: u16,
    pub title: String,
    pub alive: bool,
    pub exit_code: Option<i32>,
    /// Bytes the terminal has written so far.
    pub offset: u64,
}

impl Terminal {
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            id: value["id"].as_str()?.to_string(),
            cwd: value["cwd"].as_str().unwrap_or_default().to_string(),
            cols: value["cols"].as_u64().map_or(80, |c| c as u16),
            rows: value["rows"].as_u64().map_or(24, |r| r as u16),
            title: value["title"].as_str().unwrap_or_default().to_string(),
            alive: value["alive"].as_bool().unwrap_or(false),
            exit_code: value["exitCode"]
                .as_i64()
                .and_then(|c| i32::try_from(c).ok()),
            offset: value["offset"].as_u64().unwrap_or(0),
        })
    }
}

/// What a terminal's stream says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    /// First on every connection: with `snapshot` the screen as it is
    /// (reset the emulator, write it, go on); without, the missed bytes
    /// follow. `offset` is where output continues.
    Attached {
        offset: u64,
        snapshot: Option<Vec<u8>>,
    },
    /// Terminal output; `offset` is the end of this chunk (what to resume
    /// from).
    Output {
        offset: u64,
        data: Vec<u8>,
    },
    Title(String),
    Resized {
        cols: u16,
        rows: u16,
    },
    /// The program ended; the stream ends after it.
    Exit(Option<i32>),
    /// This stream fell behind and ends: attach again.
    Overflow,
    /// The terminal host went away (its terminals with it).
    HostGone,
}

/// A terminal's output, as it comes.
pub struct TermStream(Events);

impl TermStream {
    /// The next event; `None` when the stream ended, went silent or `stop`
    /// was set.
    pub fn next(&mut self, stop: &AtomicBool) -> Option<TermEvent> {
        loop {
            let event = self.0.next(stop)?;
            let data: Value = serde_json::from_str(&event.data).unwrap_or_default();
            let bytes = |key: &str| data[key].as_str().and_then(|b| STANDARD.decode(b).ok());
            let offset = data["offset"].as_u64().unwrap_or(0);
            match event.event.as_deref() {
                Some("attached") => {
                    return Some(TermEvent::Attached {
                        offset,
                        snapshot: bytes("snapshot"),
                    });
                }
                Some("output") => {
                    return Some(TermEvent::Output {
                        offset,
                        data: bytes("data").unwrap_or_default(),
                    });
                }
                Some("title") => {
                    return Some(TermEvent::Title(
                        data["title"].as_str().unwrap_or_default().to_string(),
                    ));
                }
                Some("resized") => {
                    return Some(TermEvent::Resized {
                        cols: data["cols"].as_u64().map_or(80, |c| c as u16),
                        rows: data["rows"].as_u64().map_or(24, |r| r as u16),
                    });
                }
                Some("exit") => {
                    return Some(TermEvent::Exit(
                        data["code"].as_i64().and_then(|c| i32::try_from(c).ok()),
                    ));
                }
                Some("overflow") => return Some(TermEvent::Overflow),
                Some("error") => return Some(TermEvent::HostGone),
                _ => {}
            }
        }
    }
}

/// `GET /api/term-host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermHost {
    /// A host binary is there.
    pub available: bool,
    pub running: bool,
    pub version: Option<String>,
    /// Node's `process.platform` and `os.arch()` of the server, for the
    /// build to install.
    pub platform: String,
    pub arch: String,
}

fn terminal_path(id: &str, rest: &str) -> Result<String, ServeError> {
    if !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Ok(format!("/api/terminals/{id}{rest}"))
    } else {
        Err(ServeError {
            status: Some(404),
            error: Some("not_found".into()),
            message: "No such terminal on the server.".into(),
        })
    }
}

impl ServeClient {
    /// `GET /api/terminals`: this device's terminals.
    pub fn terminals(&self) -> Result<Vec<Terminal>, ServeError> {
        let answer = self.json("GET", "/api/terminals", None)?;
        Ok(answer["terminals"]
            .as_array()
            .ok_or_else(malformed)?
            .iter()
            .filter_map(Terminal::parse)
            .collect())
    }

    /// `POST /api/terminals`: the server user's login shell in `cwd`.
    pub fn open_terminal(&self, cwd: &str, cols: u16, rows: u16) -> Result<Terminal, ServeError> {
        let answer = self.json(
            "POST",
            "/api/terminals",
            Some(&json!({"cwd": cwd, "cols": cols.clamp(1, 1000), "rows": rows.clamp(1, 500)})),
        )?;
        Terminal::parse(&answer["terminal"]).ok_or_else(malformed)
    }

    /// `POST /api/terminals/:t/input`: bytes as typed.
    pub fn terminal_input(&self, id: &str, data: &[u8]) -> Result<(), ServeError> {
        self.json(
            "POST",
            &terminal_path(id, "/input")?,
            Some(&json!({"data": STANDARD.encode(data)})),
        )
        .map(drop)
    }

    /// `POST /api/terminals/:t/resize`.
    pub fn terminal_resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), ServeError> {
        self.json(
            "POST",
            &terminal_path(id, "/resize")?,
            Some(&json!({"cols": cols.clamp(1, 1000), "rows": rows.clamp(1, 500)})),
        )
        .map(drop)
    }

    /// `DELETE /api/terminals/:t`: close it (or forget an exited one).
    pub fn close_terminal(&self, id: &str) -> Result<(), ServeError> {
        self.call(&self.agent, "DELETE", &terminal_path(id, "")?, None)
            .map(drop)
    }

    /// `GET /api/terminals/:t/stream`, resuming after `from` (the last
    /// output offset seen) when given.
    pub fn terminal_stream(&self, id: &str, from: Option<u64>) -> Result<TermStream, ServeError> {
        let mut path = terminal_path(id, "/stream")?;
        if let Some(from) = from {
            path.push_str(&query(&[("from", &from.to_string())]));
        }
        let (_, response) = self.send(&self.stream, "GET", &path, None, &[])?;
        Ok(TermStream(Events::new(response)))
    }

    /// `GET /api/term-host`.
    pub fn term_host(&self) -> Result<TermHost, ServeError> {
        let answer = self.json("GET", "/api/term-host", None)?;
        Ok(TermHost {
            available: answer["available"].as_bool().unwrap_or(false),
            running: answer["running"].as_bool().unwrap_or(false),
            version: answer["version"].as_str().map(str::to_string),
            platform: answer["platform"].as_str().unwrap_or_default().to_string(),
            arch: answer["arch"].as_str().unwrap_or_default().to_string(),
        })
    }

    /// `PUT /api/term-host`: `binary` as the server's terminal host (needs
    /// `admin`); the server checks its sha256. Where it went.
    pub fn install_term_host(&self, binary: &[u8]) -> Result<String, ServeError> {
        let sha = sha2::Sha256::digest(binary)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let (status, mut response) = self.send(
            &self.long,
            "PUT",
            "/api/term-host",
            Some(Body::Raw(binary)),
            &[("x-sha256", &sha)],
        )?;
        let answer = read_json(&mut response).map_err(|message| ServeError {
            status: Some(status),
            error: None,
            message,
        })?;
        Ok(answer["path"].as_str().unwrap_or_default().to_string())
    }
}
