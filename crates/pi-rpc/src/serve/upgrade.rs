//! Upgrading the server's own Gentle Shell (`/api/upgrade`, capability
//! `upgrade`, scope `admin`): one job at a time runs `gentle-shell
//! upgrade` there, its output streams as Server-Sent Events, and on success
//! `serve` restarts itself on the new version (its session keepers and
//! terminal host keep running; clients reconnect).

use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::{Events, ServeClient, ServeError, malformed};

/// What an upgrade's stream says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeEvent {
    /// One line of `gentle-shell upgrade`'s output.
    Line(String),
    /// The upgrade ended; on success the server restarts. `version` is the
    /// version installed, when the server knows it.
    Finished {
        ok: bool,
        error: Option<String>,
        version: Option<String>,
    },
}

impl UpgradeEvent {
    /// One SSE event of the stream; `None` for one it doesn't name.
    fn parse(event: Option<&str>, data: &str) -> Option<Self> {
        let data: Value = serde_json::from_str(data).unwrap_or_default();
        let text = |key: &str| data[key].as_str().map(str::to_string);
        match event? {
            "line" => Some(Self::Line(text("line").unwrap_or_default())),
            "finished" => Some(Self::Finished {
                ok: data["ok"].as_bool().unwrap_or(false),
                error: text("error"),
                version: text("version"),
            }),
            _ => None,
        }
    }
}

/// An upgrade's output, as it comes.
pub struct UpgradeStream(Events);

impl UpgradeStream {
    /// The next event; `None` when the stream ended (the server restarting
    /// ends it too), went silent or `stop` was set.
    pub fn next(&mut self, stop: &AtomicBool) -> Option<UpgradeEvent> {
        loop {
            let event = self.0.next(stop)?;
            if let Some(event) = UpgradeEvent::parse(event.event.as_deref(), &event.data) {
                return Some(event);
            }
        }
    }
}

fn job_path(job: &str) -> Result<String, ServeError> {
    if !job.is_empty()
        && job
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Ok(format!("/api/upgrade/{job}/stream"))
    } else {
        Err(malformed())
    }
}

impl ServeClient {
    /// `POST /api/upgrade { channel? }` (needs `admin`): start upgrading the
    /// server's Gentle Shell, on `channel` (`main`) or its own. The job to
    /// follow. A server already upgrading answers `409 upgrade_running`.
    pub fn upgrade(&self, channel: Option<&str>) -> Result<String, ServeError> {
        let body = match channel {
            Some(channel) => json!({ "channel": channel }),
            None => json!({}),
        };
        let answer = self.json("POST", "/api/upgrade", Some(&body))?;
        answer["jobId"]
            .as_str()
            .filter(|job| !job.is_empty())
            .map(str::to_string)
            .ok_or_else(malformed)
    }

    /// `GET /api/upgrade/:jobId/stream`: the job's output from its start.
    pub fn upgrade_stream(&self, job: &str) -> Result<UpgradeStream, ServeError> {
        let (_, response) = self.send(&self.stream, "GET", &job_path(job)?, None, &[])?;
        Ok(UpgradeStream(Events::new(response)))
    }

    /// The scopes this device's token has (`device.scopes` of
    /// `GET /api/server`): `admin` is what upgrading the server needs.
    pub fn scopes(&self) -> Result<Vec<String>, ServeError> {
        let answer = self.json("GET", "/api/server", None)?;
        Ok(answer["device"]["scopes"]
            .as_array()
            .map(|scopes| {
                scopes
                    .iter()
                    .filter_map(|scope| scope.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::{UpgradeEvent, job_path};

    #[test]
    fn the_stream_says_lines_and_how_it_finished() {
        assert_eq!(
            UpgradeEvent::parse(Some("line"), r#"{"line":"Installing 5.1.0…"}"#),
            Some(UpgradeEvent::Line("Installing 5.1.0…".into()))
        );
        assert_eq!(
            UpgradeEvent::parse(Some("finished"), r#"{"ok":true,"version":"5.1.0"}"#),
            Some(UpgradeEvent::Finished {
                ok: true,
                error: None,
                version: Some("5.1.0".into()),
            })
        );
        assert_eq!(
            UpgradeEvent::parse(Some("finished"), r#"{"ok":false,"error":"npm failed"}"#),
            Some(UpgradeEvent::Finished {
                ok: false,
                error: Some("npm failed".into()),
                version: None,
            })
        );
        // A heartbeat or an event this client doesn't know is skipped.
        assert_eq!(UpgradeEvent::parse(None, "{}"), None);
        assert_eq!(UpgradeEvent::parse(Some("hello"), "{}"), None);
    }

    #[test]
    fn a_job_id_stays_one_path_segment() {
        assert_eq!(job_path("up_1").unwrap(), "/api/upgrade/up_1/stream");
        assert!(job_path("../server").is_err());
        assert!(job_path("").is_err());
    }
}
