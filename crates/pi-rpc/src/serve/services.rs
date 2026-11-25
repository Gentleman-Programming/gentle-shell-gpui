//! Pages the server machine runs on its own (Engram, Gentle Tower): what
//! `GET /api/services` lists, and server-level forwards to them
//! (`POST /api/forwards`), not tied to a session.

use std::time::Duration;

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

    /// Visit `forward`'s link once for its cookie, reached as this client
    /// reaches its server (the forward's listener has the same certificate;
    /// through a relay, the relay is asked for the forward's port). Blocking.
    pub fn open_page(&self, forward: &Forward, timeout: Duration) -> Result<ForwardedPage, String> {
        let (origin, path) = split_link(&forward.url).ok_or("The forward's link has no host.")?;
        let base = self.url();
        // Through a relay the link names the relay's host (the one this
        // client used) and the forward's port: it goes to the relay instead.
        let (origin, link) = match base.strip_prefix(gentle_core::serve::RELAY_PREFIX) {
            Some(relay) => {
                let port = origin
                    .rsplit_once(':')
                    .and_then(|(_, port)| port.parse::<u16>().ok())
                    .ok_or("The forward's link has no port.")?;
                (format!("https://{relay}"), self.trust().link(Some(port)))
            }
            None => (origin, self.trust().link(None)),
        };
        let agent = super::linked_agent(&link, |c| c.timeout_global(Some(timeout)));
        let response = agent.get(format!("{origin}{path}")).call().map_err(|e| {
            if link.changed() {
                gentle_core::serve::CERTIFICATE_CHANGED.to_string()
            } else {
                format!("Couldn't reach the forwarded page: {e}")
            }
        })?;
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
        Ok(ForwardedPage {
            agent,
            origin,
            cookie,
        })
    }
}

/// A page behind a forward, read by the app's own HTTP client (Engram's
/// API): the first visit trades the link's single-use ticket for the
/// forward's cookie (`303` + `Set-Cookie: gs_fwd_<id>=…`), which every later
/// request carries ([`ServeClient::open_page`]).
#[derive(Clone)]
pub struct ForwardedPage {
    agent: ureq::Agent,
    /// `scheme://host:port` requests go to.
    origin: String,
    /// `gs_fwd_<id>=<secret>`.
    cookie: String,
}

impl ForwardedPage {
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

/// A forward's link as `scheme://host:port` and the rest (path and query).
fn split_link(url: &str) -> Option<(String, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, path) = rest.split_at(end);
    (!host.is_empty()).then(|| (format!("{scheme}://{host}"), path))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_forward_link_names_its_origin_and_path() {
        let split = |url| super::split_link(url).map(|(o, p)| (o, p.to_string()));
        assert_eq!(
            split("http://10.0.0.2:8801/x?gs_ticket=t"),
            Some(("http://10.0.0.2:8801".into(), "/x?gs_ticket=t".into()))
        );
        assert_eq!(
            split("https://h:1?gs_ticket=t"),
            Some(("https://h:1".into(), "?gs_ticket=t".into()))
        );
        assert_eq!(split("ftp://h/"), None);
        assert_eq!(split("https:///x"), None);
    }
}
