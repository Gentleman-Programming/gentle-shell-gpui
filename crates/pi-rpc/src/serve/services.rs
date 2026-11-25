//! Pages the server machine runs on its own (Engram, Gentle Tower): what
//! `GET /api/services` lists, and server-level forwards to them
//! (`POST /api/forwards`), not tied to a session.

use serde_json::{Value, json};

use super::{Forward, ServeClient, ServeError, malformed};

/// One of `GET /api/services`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// `engram`, `tower`.
    pub id: String,
    pub name: String,
    /// Its port on the server's loopback.
    pub port: u16,
    /// It answered its health check.
    pub available: bool,
    /// Reachable through a forward; `via` says which (`forwards`: with
    /// [`ServeClient::server_forward`]; `tower`: with
    /// [`ServeClient::tower`]).
    pub forwardable: bool,
    pub via: Option<String>,
}

impl Service {
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            id: value["id"].as_str()?.to_string(),
            name: value["name"].as_str().unwrap_or_default().to_string(),
            port: u16::try_from(value["port"].as_u64()?).ok()?,
            available: value["available"].as_bool().unwrap_or(false),
            forwardable: value["forwardable"].as_bool().unwrap_or(false),
            via: value["via"].as_str().map(str::to_string),
        })
    }
}

impl ServeClient {
    /// `GET /api/services`.
    pub fn services(&self) -> Result<Vec<Service>, ServeError> {
        let answer = self.json("GET", "/api/services", None)?;
        Ok(answer["services"]
            .as_array()
            .ok_or_else(malformed)?
            .iter()
            .filter_map(Service::parse)
            .collect())
    }

    /// `POST /api/forwards { port, path }`: the page at `path` on `port` of
    /// the server's loopback, for this device, belonging to no session. The
    /// server reuses this device's forward of the port, with a fresh ticket.
    pub fn server_forward(&self, port: u16, path: &str) -> Result<Forward, ServeError> {
        let answer = self.json(
            "POST",
            "/api/forwards",
            Some(&json!({"port": port, "path": path})),
        )?;
        Forward::parse(&answer).ok_or_else(malformed)
    }
}

/// A page behind a forward, read by the app's own HTTP client (Engram's
/// API): the first visit trades the link's single-use ticket for the
/// forward's cookie (`303` + `Set-Cookie: gs_fwd_<id>=…`), which every later
/// request carries.
#[derive(Clone)]
pub struct ForwardedPage {
    agent: ureq::Agent,
    /// `http://host:port` of the forward.
    origin: String,
    /// `gs_fwd_<id>=<secret>`.
    cookie: String,
}

impl ForwardedPage {
    /// Visit `forward`'s link once for its cookie. Blocking.
    pub fn open(forward: &Forward, timeout: std::time::Duration) -> Result<Self, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(timeout))
            .build()
            .into();
        let response = agent
            .get(&forward.url)
            .call()
            .map_err(|e| format!("Couldn't reach the forwarded page: {e}"))?;
        let cookie = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|v| v.split(';').next())
            .find(|pair| pair.trim_start().starts_with("gs_fwd_"))
            .map(|pair| pair.trim().to_string())
            .ok_or_else(|| {
                format!(
                    "The forward answered {} without its cookie.",
                    response.status().as_u16()
                )
            })?;
        let origin = forward_origin(&forward.url).ok_or("The forward's link has no host.")?;
        Ok(Self {
            agent,
            origin,
            cookie,
        })
    }

    /// `GET path` (with its query) on the page: its status and body.
    pub fn get(&self, path: &str) -> Result<(u16, String), String> {
        let mut response = self
            .agent
            .get(format!("{}{path}", self.origin))
            .header("Cookie", &self.cookie)
            .call()
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok((status, body))
    }
}

/// `http://host:port` of a forward's link.
fn forward_origin(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| format!("http://{host}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_forward_link_names_its_origin() {
        assert_eq!(
            super::forward_origin("http://10.0.0.2:8801/x?gs_ticket=t").as_deref(),
            Some("http://10.0.0.2:8801")
        );
        assert_eq!(
            super::forward_origin("http://h:1?gs_ticket=t").as_deref(),
            Some("http://h:1")
        );
        assert_eq!(super::forward_origin("https://h/"), None);
    }
}
