//! Files, git and uploads on a Gentle Shell server (`/api/fs/*`,
//! `POST /api/git`, `POST /api/uploads`): every path is an absolute path
//! there, as the server sees it.

use std::sync::atomic::AtomicBool;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::{BODY_LIMIT, Body, Events, ServeClient, ServeError, malformed, query, read_json};

/// The most one `GET /api/fs/read` answers.
pub const READ_CHUNK: u64 = 8 * 1024 * 1024;

/// The largest body `PUT /api/fs/write` takes: the server refuses more
/// (`413 too_large`).
pub const WRITE_LIMIT: u64 = 64 * 1024 * 1024;

/// What an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    /// A socket, FIFO or device.
    Other,
}

impl EntryKind {
    fn parse(value: &Value) -> Option<Self> {
        Some(match value.as_str()? {
            "file" => Self::File,
            "dir" => Self::Dir,
            "symlink" => Self::Symlink,
            _ => Self::Other,
        })
    }
}

/// One entry of a folder on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Milliseconds since the epoch.
    pub mtime: u64,
    /// What a symlink points to; `None` when it dangles (or isn't a link).
    pub link_kind: Option<EntryKind>,
}

impl Entry {
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            name: value["name"].as_str().unwrap_or_default().to_string(),
            kind: EntryKind::parse(&value["kind"])?,
            size: value["size"].as_u64().unwrap_or(0),
            mtime: value["mtime"].as_f64().map_or(0, |m| m as u64),
            link_kind: EntryKind::parse(&value["linkKind"]),
        })
    }

    /// A folder, or a link to one (listed like a folder).
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
            || (self.kind == EntryKind::Symlink && self.link_kind == Some(EntryKind::Dir))
    }
}

/// `GET /api/fs/list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub path: String,
    /// Sorted by name (byte order), hidden ones included.
    pub entries: Vec<Entry>,
    /// More than 5000 entries: these are some of them.
    pub truncated: bool,
}

/// `GET /api/fs/stat`: the entry itself (a link is reported as one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub path: String,
    pub entry: Entry,
    /// Permission bits.
    pub mode: u32,
    /// The content's tag, for [`ServeClient::fs_write`]'s guard.
    pub etag: Option<String>,
    pub link_target: Option<String>,
    /// The path with every link resolved; `None` when a link dangles.
    pub real_path: Option<String>,
}

/// Bytes of a file on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChunk {
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
    /// The whole file's size.
    pub size: u64,
    /// Milliseconds since the epoch.
    pub mtime: u64,
    /// Bytes remain after this chunk.
    pub truncated: bool,
}

/// `PUT /api/fs/write`'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: String,
    pub size: u64,
    pub etag: Option<String>,
    pub created: bool,
}

/// One name `GET /api/fs/search` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub path: String,
    pub name: String,
    pub kind: EntryKind,
}

/// A change batch of `GET /api/fs/watch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// The watch is in place.
    Ready,
    /// These paths changed (stat them); `overflow` means too many to name:
    /// look again at everything.
    Changes { paths: Vec<String>, overflow: bool },
    /// The watch failed (the stream ends).
    Error(String),
}

/// A folder's changes, as they happen.
pub struct WatchStream(Events);

impl WatchStream {
    /// The next batch; `None` when the stream ended or `stop` was set.
    pub fn next(&mut self, stop: &AtomicBool) -> Option<WatchEvent> {
        loop {
            let event = self.0.next(stop)?;
            let data: Value = serde_json::from_str(&event.data).unwrap_or_default();
            match event.event.as_deref() {
                Some("ready") => return Some(WatchEvent::Ready),
                Some("changes") => {
                    return Some(WatchEvent::Changes {
                        paths: data["paths"]
                            .as_array()
                            .map(|paths| {
                                paths
                                    .iter()
                                    .filter_map(|p| p.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        overflow: data["overflow"].as_bool().unwrap_or(false),
                    });
                }
                Some("error") => {
                    return Some(WatchEvent::Error(
                        data["message"]
                            .as_str()
                            .unwrap_or("The watch failed")
                            .to_string(),
                    ));
                }
                _ => {}
            }
        }
    }
}

/// `POST /api/git`'s answer: git's own, whatever its exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// git's exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// stdout was cut at 32 MiB (git was stopped).
    pub truncated: bool,
    pub timed_out: bool,
}

impl GitOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.truncated && !self.timed_out
    }
}

/// `POST /api/uploads`' answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uploaded {
    /// Where the file is on the server: what the prompt names.
    pub path: String,
    pub name: String,
    pub size: u64,
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

impl ServeClient {
    /// `GET /api/fs/list?path=`.
    pub fn fs_list(&self, path: &str) -> Result<Listing, ServeError> {
        let answer = self.json(
            "GET",
            &format!("/api/fs/list{}", query(&[("path", path)])),
            None,
        )?;
        Ok(Listing {
            path: text(&answer["path"]).unwrap_or_else(|| path.to_string()),
            entries: answer["entries"]
                .as_array()
                .ok_or_else(malformed)?
                .iter()
                .filter_map(Entry::parse)
                .collect(),
            truncated: answer["truncated"].as_bool().unwrap_or(false),
        })
    }

    /// `GET /api/fs/stat?path=`.
    pub fn fs_stat(&self, path: &str) -> Result<Stat, ServeError> {
        let answer = self.json(
            "GET",
            &format!("/api/fs/stat{}", query(&[("path", path)])),
            None,
        )?;
        Ok(Stat {
            path: text(&answer["path"]).unwrap_or_else(|| path.to_string()),
            entry: Entry::parse(&answer).ok_or_else(malformed)?,
            mode: answer["mode"].as_u64().map_or(0, |m| m as u32),
            etag: text(&answer["etag"]),
            link_target: text(&answer["linkTarget"]),
            real_path: text(&answer["realPath"]),
        })
    }

    /// `GET /api/fs/read`: up to `length` bytes (at most [`READ_CHUNK`])
    /// from `offset`.
    pub fn fs_read(
        &self,
        path: &str,
        offset: u64,
        length: Option<u64>,
    ) -> Result<FileChunk, ServeError> {
        let offset_text = offset.to_string();
        let length_text = length.map(|l| l.to_string());
        let mut pairs = vec![("path", path), ("offset", offset_text.as_str())];
        if let Some(length) = &length_text {
            pairs.push(("length", length));
        }
        let (status, mut response) = self.send(
            &self.long,
            "GET",
            &format!("/api/fs/read{}", query(&pairs)),
            None,
            &[],
        )?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let etag = header("etag");
        let size = header("x-file-size").and_then(|s| s.parse().ok());
        let mtime = header("x-file-mtime").and_then(|s| s.parse::<f64>().ok());
        let truncated = header("x-truncated").is_some_and(|t| t == "1");
        let bytes = response
            .body_mut()
            .with_config()
            .limit(READ_CHUNK + 1)
            .read_to_vec()
            .map_err(|e| ServeError {
                status: Some(status),
                error: None,
                message: format!("Couldn't read {path} from the server: {e}"),
            })?;
        Ok(FileChunk {
            size: size.unwrap_or(offset + bytes.len() as u64),
            bytes,
            etag,
            mtime: mtime.map_or(0, |m| m as u64),
            truncated,
        })
    }

    /// The whole file, chunk by chunk, refused past `limit` bytes (`413`):
    /// its bytes and tag.
    pub fn fs_read_all(&self, path: &str, limit: u64) -> Result<FileChunk, ServeError> {
        let mut first = self.fs_read(path, 0, None)?;
        if first.size > limit {
            return Err(ServeError {
                status: Some(413),
                error: Some("too_large".into()),
                message: format!("{path} is too large to open ({} bytes).", first.size),
            });
        }
        while first.truncated {
            let next = self.fs_read(path, first.bytes.len() as u64, None)?;
            if next.bytes.is_empty() {
                break;
            }
            first.bytes.extend_from_slice(&next.bytes);
            first.truncated = next.truncated;
        }
        Ok(first)
    }

    /// `PUT /api/fs/write`: `bytes` become the file, atomically. `if_match`
    /// refuses (`412 changed`) when the file changed since that tag;
    /// `create_only` refuses an existing file (`409 exists`).
    pub fn fs_write(
        &self,
        path: &str,
        bytes: &[u8],
        if_match: Option<&str>,
        create_only: bool,
    ) -> Result<Written, ServeError> {
        let mut pairs = vec![("path", path)];
        if create_only {
            pairs.push(("create_only", "1"));
        }
        let headers: Vec<(&str, &str)> =
            if_match.map(|tag| ("If-Match", tag)).into_iter().collect();
        let (status, mut response) = self.send(
            &self.long,
            "PUT",
            &format!("/api/fs/write{}", query(&pairs)),
            Some(Body::Raw(bytes)),
            &headers,
        )?;
        let answer = read_json(&mut response).map_err(|message| ServeError {
            status: Some(status),
            error: None,
            message,
        })?;
        Ok(Written {
            path: text(&answer["path"]).unwrap_or_else(|| path.to_string()),
            size: answer["size"].as_u64().unwrap_or(bytes.len() as u64),
            etag: text(&answer["etag"]),
            created: answer["created"].as_bool().unwrap_or(status == 201),
        })
    }

    /// `POST /api/fs/mkdir` (with any missing parent): whether it created it.
    pub fn fs_mkdir(&self, path: &str) -> Result<bool, ServeError> {
        let answer = self.json("POST", "/api/fs/mkdir", Some(&json!({"path": path})))?;
        Ok(answer["created"].as_bool().unwrap_or(false))
    }

    /// `POST /api/fs/rename`: `409 exists` for an existing `to` unless
    /// `overwrite`.
    pub fn fs_rename(&self, from: &str, to: &str, overwrite: bool) -> Result<(), ServeError> {
        self.json(
            "POST",
            "/api/fs/rename",
            Some(&json!({"from": from, "to": to, "overwrite": overwrite})),
        )
        .map(drop)
    }

    /// `POST /api/fs/remove`: a file or link, or a folder (`recursive` for
    /// one with something in it).
    pub fn fs_remove(&self, path: &str, recursive: bool) -> Result<(), ServeError> {
        self.json(
            "POST",
            "/api/fs/remove",
            Some(&json!({"path": path, "recursive": recursive})),
        )
        .map(drop)
    }

    /// `GET /api/fs/search`: names under `root` matching `q`, best first.
    pub fn fs_search(
        &self,
        root: &str,
        q: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, ServeError> {
        let limit = limit.clamp(1, 1000).to_string();
        let answer = self.json(
            "GET",
            &format!(
                "/api/fs/search{}",
                query(&[("root", root), ("q", q), ("limit", &limit)])
            ),
            None,
        )?;
        Ok(answer["results"]
            .as_array()
            .ok_or_else(malformed)?
            .iter()
            .filter_map(|hit| {
                Some(SearchHit {
                    path: text(&hit["path"])?,
                    name: text(&hit["name"]).unwrap_or_default(),
                    kind: EntryKind::parse(&hit["kind"])?,
                })
            })
            .collect())
    }

    /// `GET /api/fs/watch?path=`: what changes under `path`, as it does.
    pub fn fs_watch(&self, path: &str) -> Result<WatchStream, ServeError> {
        let (_, response) = self.send(
            &self.stream,
            "GET",
            &format!("/api/fs/watch{}", query(&[("path", path)])),
            None,
            &[],
        )?;
        Ok(WatchStream(Events::new(response)))
    }

    /// `POST /api/git`: `git args` in `cwd` there, `env` added (only the
    /// names the server allows), `stdin` written to it. The answer whatever
    /// git's exit code.
    pub fn git(
        &self,
        cwd: &str,
        args: &[String],
        env: &[(String, String)],
        stdin: Option<&[u8]>,
    ) -> Result<GitOutput, ServeError> {
        let mut body = json!({"cwd": cwd, "args": args});
        if !env.is_empty() {
            body["env"] = env
                .iter()
                .map(|(k, v)| (k.clone(), Value::from(v.as_str())))
                .collect::<serde_json::Map<_, _>>()
                .into();
        }
        if let Some(stdin) = stdin {
            body["stdin"] = STANDARD.encode(stdin).into();
        }
        let (status, mut response) = self.call(&self.long, "POST", "/api/git", Some(&body))?;
        let answer = read_json(&mut response).map_err(|message| ServeError {
            status: Some(status),
            error: None,
            message,
        })?;
        let bytes = |key: &str| {
            answer[key]
                .as_str()
                .and_then(|b64| STANDARD.decode(b64).ok())
                .unwrap_or_default()
        };
        Ok(GitOutput {
            code: answer["code"].as_i64().and_then(|c| i32::try_from(c).ok()),
            stdout: bytes("stdout"),
            stderr: bytes("stderr"),
            truncated: answer["truncated"].as_bool().unwrap_or(false),
            timed_out: answer["timedOut"].as_bool().unwrap_or(false),
        })
    }

    /// `POST /api/uploads?name=`: `bytes` as a file on the server, for a
    /// prompt to name.
    pub fn upload(&self, name: &str, bytes: &[u8]) -> Result<Uploaded, ServeError> {
        if bytes.len() as u64 > BODY_LIMIT {
            return Err(ServeError {
                status: Some(413),
                error: Some("too_large".into()),
                message: format!("{name} is too large to send to the server."),
            });
        }
        let (status, mut response) = self.send(
            &self.long,
            "POST",
            &format!("/api/uploads{}", query(&[("name", name)])),
            Some(Body::Raw(bytes)),
            &[],
        )?;
        let answer = read_json(&mut response).map_err(|message| ServeError {
            status: Some(status),
            error: None,
            message,
        })?;
        Ok(Uploaded {
            path: text(&answer["path"]).ok_or_else(malformed)?,
            name: text(&answer["name"]).unwrap_or_else(|| name.to_string()),
            size: answer["size"].as_u64().unwrap_or(bytes.len() as u64),
        })
    }
}
