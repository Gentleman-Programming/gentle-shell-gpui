//! Engram's HTTP API: where it listens (this computer's, or a server's
//! through a forward), blocking GETs and the queries the browser needs.
//! Runs on the background executor.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use gentle_core::engram::{Observation, encode, parse_observations};
use pi_rpc::serve::{ForwardedPage, ServeClient, Service};

use crate::features::backend::{self, server};

/// How long one Engram request may take.
const TIMEOUT: Duration = Duration::from_secs(8);

/// `ENGRAM_URL`, else `ENGRAM_PORT` on loopback, else Engram's default port.
fn base_url() -> String {
    if let Ok(url) = std::env::var("ENGRAM_URL")
        && !url.trim().is_empty()
    {
        return url.trim_end_matches('/').to_string();
    }
    let port = std::env::var("ENGRAM_PORT")
        .ok()
        .filter(|p| p.parse::<u16>().is_ok())
        .unwrap_or_else(|| "7437".into());
    format!("http://127.0.0.1:{port}")
}

/// `GET path` on the Engram that serves `cwd`: the server's, through a
/// forward, when `cwd` is on a server; else this computer's.
fn get(cwd: &Path, path: &str) -> Result<serde_json::Value, String> {
    let (status, body) = match server::at(cwd) {
        None => local_get(path)?,
        Some(client) => server_get(&client.map_err(|e| e.to_string())?, cwd, path)?,
    };
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|_| format!("Engram answered {status} without JSON"))?;
    if !(200..300).contains(&status) {
        return Err(json["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("Engram answered {status}")));
    }
    Ok(json)
}

/// `GET path` on this computer's Engram: its status and body.
fn local_get(path: &str) -> Result<(u16, String), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let mut request = agent.get(format!("{}{path}", base_url()));
    if let Ok(token) = std::env::var("ENGRAM_HTTP_TOKEN")
        && !token.is_empty()
    {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let mut response = request.call().map_err(|_| {
        "Engram is not running. Gentle Shell starts it with the first session.".to_string()
    })?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok((status, body))
}

/// The forwarded Engram page of each server this run, by server address.
static PAGES: LazyLock<Mutex<HashMap<String, ForwardedPage>>> = LazyLock::new(Mutex::default);

/// `GET path` on the server's Engram through its forwarded page (opened
/// once per server); a page the server stopped honouring (`401`, gone) is
/// opened again once.
fn server_get(client: &ServeClient, cwd: &Path, path: &str) -> Result<(u16, String), String> {
    let address = client.url();
    let cached = PAGES.lock().unwrap().get(&address).cloned();
    if let Some(page) = cached {
        match page.get(path) {
            Ok((401, _)) | Err(_) => {
                PAGES.lock().unwrap().remove(&address);
            }
            Ok(answer) => return Ok(answer),
        }
    }
    let page = open_page(client, cwd)?;
    let answer = page.get(path)?;
    PAGES.lock().unwrap().insert(address, page);
    Ok(answer)
}

/// Forward the server's Engram (`GET /api/services`, `POST /api/forwards`)
/// and visit it for its cookie; why not, when the server can't.
fn open_page(client: &ServeClient, cwd: &Path) -> Result<ForwardedPage, String> {
    if !server::offers(&backend::for_tab(cwd), "services") {
        return Err(server::UNAVAILABLE.to_string());
    }
    let services = client.services().map_err(|e| e.message)?;
    let port = engram_port(&services)?;
    let forward = client.server_forward(port, "/").map_err(|e| e.message)?;
    ForwardedPage::open(&forward, TIMEOUT)
}

/// The port of the server's Engram when a forward can reach it, else why
/// the memory screen can't show it.
fn engram_port(services: &[Service]) -> Result<u16, String> {
    let engram = services
        .iter()
        .find(|s| s.id == "engram")
        .ok_or("The server doesn't run Engram, so it has no memories to show.")?;
    if !engram.available {
        return Err(
            "Engram is not running on the server. Gentle Shell starts it there with the first session."
                .into(),
        );
    }
    if !engram.forwardable || engram.via.as_deref() != Some("forwards") {
        return Err(
            "This Gentle Shell server can't forward its Engram yet: upgrade Gentle Shell there."
                .into(),
        );
    }
    Ok(engram.port)
}

/// The project Engram assigns to `cwd` (`GET /project/current`).
pub(super) fn current_project(cwd: &Path) -> Option<String> {
    let json = get(
        cwd,
        &format!("/project/current?cwd={}", encode(&cwd.to_string_lossy())),
    )
    .ok()?;
    json["project"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// The latest (or matching `query`) memories of `project`, else of every
/// project, from the Engram serving `cwd`.
pub(super) fn load(
    cwd: &Path,
    project: Option<&str>,
    query: &str,
) -> Result<Vec<Observation>, String> {
    let scope = match project {
        Some(p) => format!("project={}", encode(p)),
        None => "all_projects=true".into(),
    };
    let path = if query.trim().is_empty() {
        format!("/observations/recent?{scope}&limit=60")
    } else {
        format!("/search?q={}&{scope}&limit=40", encode(query.trim()))
    };
    match get(cwd, &path) {
        Ok(value) => Ok(parse_observations(value)),
        // A project Engram has never seen simply has no memories yet.
        Err(error) if project.is_some() && error.contains("not found") => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// One observation (`GET /observations/{id}`), for a memory the session
/// status points at that the loaded page doesn't hold.
pub(super) fn observation(cwd: &Path, id: i64) -> Option<Observation> {
    serde_json::from_value(get(cwd, &format!("/observations/{id}")).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use pi_rpc::serve::Service;

    fn engram(available: bool, forwardable: bool, via: Option<&str>) -> Service {
        Service {
            id: "engram".into(),
            name: "Engram".into(),
            port: 7437,
            available,
            forwardable,
            via: via.map(str::to_string),
        }
    }

    #[test]
    fn only_a_running_forwardable_engram_is_used() {
        assert_eq!(
            super::engram_port(&[engram(true, true, Some("forwards"))]),
            Ok(7437)
        );
        assert!(super::engram_port(&[]).is_err());
        assert!(super::engram_port(&[engram(false, true, Some("forwards"))]).is_err());
        assert!(super::engram_port(&[engram(true, false, None)]).is_err());
        assert!(super::engram_port(&[engram(true, true, Some("tower"))]).is_err());
    }
}
