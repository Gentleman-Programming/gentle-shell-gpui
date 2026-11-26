//! A Gentle Shell server's readiness checks (capability `checks`, `GET
//! /api/checks`): the answer's rows, and what each one says in Setup's
//! checklist.

use serde_json::Value;

/// `major.minor.patch`.
pub type Version = (u64, u64, u64);

/// What the server found for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Ok,
    Missing,
    /// It is there but doesn't run, or is too old; also a state this
    /// client doesn't know.
    Error,
}

/// One row of `GET /api/checks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCheck {
    /// `companion`, `node`, `git` or `engram`.
    pub id: String,
    pub state: CheckState,
    pub version: Option<String>,
    pub detail: Option<String>,
}

/// The rows of a `GET /api/checks` answer; rows without an `id` are left
/// out.
pub fn decode_checks(answer: &Value) -> Vec<ServerCheck> {
    let text = |value: &Value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    answer["checks"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(ServerCheck {
                        id: text(&row["id"])?,
                        state: match row["state"].as_str() {
                            Some("ok") => CheckState::Ok,
                            Some("missing") => CheckState::Missing,
                            _ => CheckState::Error,
                        },
                        version: text(&row["version"]),
                        detail: text(&row["detail"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A row as Setup's checklist shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    Ready(String),
    Missing(String),
    Outdated(String),
    Failed(String),
}

/// `v22.19.0`, `22.19` → numbers; missing parts are 0.
fn parse(version: &str) -> Option<Version> {
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    let mut parts = [0u64; 3];
    let mut found = 0;
    for (i, part) in version.split('.').take(3).enumerate() {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            break;
        }
        parts[i] = digits.parse().ok()?;
        found += 1;
    }
    (found > 0).then_some((parts[0], parts[1], parts[2]))
}

fn show((major, minor, patch): Version) -> String {
    format!("{major}.{minor}.{patch}")
}

/// What `check` (the row `name`, e.g. `Node`) says on the server `server`,
/// the way this computer's own check would: a version older than `min`
/// is outdated whatever the server called it.
pub fn readiness(check: &ServerCheck, name: &str, min: Option<Version>, server: &str) -> Readiness {
    let found = check.version.as_deref().and_then(parse);
    if let (Some(version), Some(min)) = (found, min)
        && version < min
    {
        return Readiness::Outdated(format!(
            "{name} {} on {server} — needs {} or newer",
            show(version),
            show(min)
        ));
    }
    let detail = check.detail.as_deref();
    let version = check.version.as_deref();
    match check.state {
        CheckState::Ok => Readiness::Ready(match (version, detail) {
            // Engram's detail is its own `engram 3.1.0` line.
            (Some(version), Some(detail)) if detail.contains(version) => {
                format!("{detail} on {server}")
            }
            (Some(version), Some(detail)) => format!("{name} {version} · {detail} on {server}"),
            (Some(version), None) => format!("{name} {version} on {server}"),
            (None, Some(detail)) => format!("{detail} on {server}"),
            (None, None) => format!("Ready on {server}"),
        }),
        CheckState::Missing => Readiness::Missing(match detail {
            Some(detail) => format!("On {server}: {detail}"),
            None => format!("Not installed on {server}"),
        }),
        // An error with the version found is one too old (or set up with
        // another version); without one, it doesn't run.
        CheckState::Error => {
            let text = match detail {
                Some(detail) => format!("On {server}: {detail}"),
                None => format!("{name} doesn't run on {server}"),
            };
            if version.is_some() {
                Readiness::Outdated(text)
            } else {
                Readiness::Failed(text)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn row(state: CheckState, version: Option<&str>, detail: Option<&str>) -> ServerCheck {
        ServerCheck {
            id: "node".into(),
            state,
            version: version.map(str::to_string),
            detail: detail.map(str::to_string),
        }
    }

    #[test]
    fn decodes_the_rows_and_drops_what_has_no_id() {
        let answer = json!({"checks": [
            {"id": "node", "state": "ok", "version": "22.19.0", "detail": null},
            {"id": "git", "state": "missing", "version": null, "detail": "git isn't on the PATH"},
            {"id": "engram", "state": "weird", "version": " ", "detail": ""},
            {"state": "ok"},
        ]});
        let rows = decode_checks(&answer);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], row(CheckState::Ok, Some("22.19.0"), None));
        assert_eq!(rows[1].state, CheckState::Missing);
        assert_eq!(rows[1].detail.as_deref(), Some("git isn't on the PATH"));
        assert_eq!(
            (
                rows[2].state,
                rows[2].version.clone(),
                rows[2].detail.clone()
            ),
            (CheckState::Error, None, None)
        );
        assert!(decode_checks(&json!({})).is_empty());
    }

    #[test]
    fn rows_read_as_the_local_checklist_does() {
        let min = Some((22, 19, 0));
        assert_eq!(
            readiness(
                &row(CheckState::Ok, Some("22.19.0"), None),
                "Node",
                min,
                "pc"
            ),
            Readiness::Ready("Node 22.19.0 on pc".into())
        );
        assert_eq!(
            readiness(
                &row(CheckState::Ok, Some("v20.1.0"), None),
                "Node",
                min,
                "pc"
            ),
            Readiness::Outdated("Node 20.1.0 on pc — needs 22.19.0 or newer".into())
        );
        assert_eq!(
            readiness(
                &row(CheckState::Error, Some("20.1.0"), Some("too old")),
                "Node",
                min,
                "pc"
            ),
            Readiness::Outdated("Node 20.1.0 on pc — needs 22.19.0 or newer".into())
        );
        assert_eq!(
            readiness(&row(CheckState::Missing, None, None), "Git", None, "pc"),
            Readiness::Missing("Not installed on pc".into())
        );
        assert_eq!(
            readiness(
                &row(CheckState::Error, None, Some("exit 1")),
                "Engram",
                None,
                "pc"
            ),
            Readiness::Failed("On pc: exit 1".into())
        );
        assert_eq!(
            readiness(
                &row(CheckState::Ok, None, Some("Set up")),
                "Companion",
                None,
                "pc"
            ),
            Readiness::Ready("Set up on pc".into())
        );
        assert_eq!(
            readiness(
                &row(CheckState::Ok, Some("3.1.0"), Some("engram 3.1.0")),
                "Engram",
                Some((3, 0, 0)),
                "pc"
            ),
            Readiness::Ready("engram 3.1.0 on pc".into())
        );
        assert_eq!(
            readiness(
                &row(CheckState::Ok, Some("22.19.0"), Some("npm 10.9.0")),
                "Node",
                min,
                "pc"
            ),
            Readiness::Ready("Node 22.19.0 · npm 10.9.0 on pc".into())
        );
        let stale = "Set up with gentle-pi 4.8.0; 4.9.0 is installed";
        assert_eq!(
            readiness(
                &row(CheckState::Error, Some("1.2.0"), Some(stale)),
                "Companion setup",
                None,
                "pc"
            ),
            Readiness::Outdated(format!("On pc: {stale}"))
        );
    }
}
