//! Gentle Shell servers (`gentle-shell serve`) as where agents run: pairing
//! this device with one (at a typed address, found on the local network for
//! a short pairing code, or wherever on the network or through its relay a
//! connection code's server answers), and the device token pairing returns.
//! The token is a secret, so it stays out of `settings.json` (which names
//! only the server, the device id and the server's name): it lives in
//! `server-tokens.json` in the app's folder, owner-only (`0600`), keyed by
//! the address the server was paired at. Beside it are how the server is
//! reached safely (`fp`, the certificate pinned when it speaks HTTPS, and
//! `relay`, the relay that reaches it from anywhere) and what finds it again
//! when its computer gets another address: its server id, its machine's host
//! name, the addresses it was reached at and the one it answers at now
//! ([`relocate`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use gentle_core::backend::Backend;
use gentle_core::serve::{ConnectionCode, Nearby, ServerPage, ServerSession};
use pi_rpc::serve::{Paired, ServeClient, ServerInfo, Trust};
use serde_json::{Map, Value, json};

/// What a server can't do for its devices: its `gentle-shell serve` is
/// older than the feature (its `GET /api/server` doesn't offer it).
pub const UNAVAILABLE: &str =
    "This Gentle Shell server can't do that yet: upgrade Gentle Shell there.";

/// What only runs on this computer or a backend the app drives itself
/// (Setup, a one-off `gentle-shell` run): a server runs only its sessions.
pub const NOT_ON_SERVER: &str = "Not available on a Gentle Shell server.";

/// What each server said about itself last, by address ([`info`]).
static INFOS: LazyLock<Mutex<HashMap<String, ServerInfo>>> = LazyLock::new(Mutex::default);

/// The server work rooted at `cwd` runs on, with this device's client;
/// `None` when it runs elsewhere (this computer, a WSL distro, a command).
pub fn at(cwd: &Path) -> Option<anyhow::Result<ServeClient>> {
    let choice = super::for_tab(cwd);
    choice.is_server().then(|| client(&choice))
}

/// The home folder of the server work rooted at `cwd` runs on, as it last
/// said (without asking); `None` off a server.
pub fn known_home_of(cwd: &Path) -> Option<String> {
    match super::for_tab(cwd) {
        Backend::Server { url, .. } => known_home(&url),
        _ => None,
    }
}

/// What `choice` (a server) says about itself: the last answer this run,
/// else asked ([`info`]). Blocking the first time.
pub fn known_info(choice: &Backend) -> Option<ServerInfo> {
    let Backend::Server { url, .. } = choice else {
        return None;
    };
    if let Some(info) = INFOS.lock().unwrap().get(url) {
        return Some(info.clone());
    }
    info(choice).ok()
}

/// Whether `choice` (a server) offers `capability` (`files`, `git`,
/// `uploads`, `terminals`, `services`, …), as it last said. Blocking the
/// first time.
pub fn offers(choice: &Backend, capability: &str) -> bool {
    known_info(choice).is_some_and(|info| info.has(capability))
}

/// Whether the server work rooted at `cwd` runs on offers `capability`, as
/// it already said this run, without asking: `None` off a server or before
/// it said. For views that word what's missing while they draw.
pub fn said(cwd: &Path, capability: &str) -> Option<bool> {
    let Backend::Server { url, .. } = super::for_tab(cwd) else {
        return None;
    };
    INFOS
        .lock()
        .unwrap()
        .get(&url)
        .map(|info| info.has(capability))
}

/// Ask `choice` what it offers again next time (a terminal host was
/// installed, the device paired again).
pub fn forget_info(choice: &Backend) {
    if let Backend::Server { url, .. } = choice {
        INFOS.lock().unwrap().remove(url);
    }
}

fn tokens_path() -> Option<PathBuf> {
    crate::shared::data_dir::app().map(|dir| dir.join("server-tokens.json"))
}

fn read_tokens() -> Map<String, Value> {
    tokens_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// Replace the tokens file in one step, readable by its owner only.
fn write_tokens(tokens: &Map<String, Value>) -> std::io::Result<()> {
    let path = tokens_path().ok_or_else(|| std::io::Error::other("no data folder"))?;
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!("server-tokens.json.{}.tmp", std::process::id()));
    // Created afresh, so the owner-only mode always applies.
    let _ = std::fs::remove_file(&temp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = (|| {
        use std::io::Write as _;
        let mut file = options.open(&temp)?;
        file.write_all(format!("{}\n", Value::Object(tokens.clone())).as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Forget the token for the server at `url` (unpairing on this side; the
/// server keeps the device until it is revoked there).
pub fn forget(url: &str) -> std::io::Result<()> {
    let mut tokens = read_tokens();
    if tokens.remove(url).is_some() {
        write_tokens(&tokens)?;
    }
    Ok(())
}

/// Where the server paired at `url` answers now: where it was last found,
/// when its address changed since ([`relocate`]); else `url`. The paired
/// address stays the server's name in `settings.json`, workspaces and tabs.
pub fn address(url: &str) -> String {
    read_tokens().get(url).map_or_else(
        || url.to_string(),
        |entry| answers_at(url, entry).to_string(),
    )
}

fn answers_at<'a>(url: &'a str, entry: &'a Value) -> &'a str {
    entry["at"].as_str().unwrap_or(url)
}

/// How the server a token entry is for is reached: the certificate pinned
/// and its relay, as pairing found them.
fn trust_of(entry: &Value) -> Trust {
    let field = |key: &str| entry[key].as_str().map(str::to_string);
    Trust {
        fingerprint: field("fp"),
        relay: field("relay"),
    }
}

/// A client of `choice` with its token; an error in words when this device
/// isn't paired with it (any more).
pub fn client(choice: &Backend) -> anyhow::Result<ServeClient> {
    let Backend::Server { url, .. } = choice else {
        anyhow::bail!("{} isn't a Gentle Shell server.", choice.label());
    };
    client_at(url).ok_or_else(|| {
        anyhow::anyhow!(
            "This computer isn't paired with the Gentle Shell server at {url}. Pair it in \
             Configuration › General › Where agents run."
        )
    })
}

/// A client of the server paired at `url`, at the address it answers at
/// now, that looks for it again when that address stops answering
/// ([`relocate`]); `None` when this device isn't paired with it.
fn client_at(url: &str) -> Option<ServeClient> {
    let tokens = read_tokens();
    let entry = tokens.get(url)?;
    let token = entry["token"].as_str()?.to_string();
    let key = url.to_string();
    Some(
        ServeClient::with_trust(answers_at(url, entry), Some(token), trust_of(entry))
            .relocating(Arc::new(move |failed| relocate(&key, failed))),
    )
}

/// After a search that found nothing, how long before the next one: a
/// server that's down isn't looked for on every reconnect.
const SEARCH_AGAIN: Duration = Duration::from_secs(20);

/// When the last search for each server (by paired address) found nothing.
/// Held while one runs, so requests failing together share one search.
static SEARCHED: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Mutex::default);

/// The server paired at `url` doesn't answer at `failed` any more (its
/// computer got another address on the network, DHCP): look for it on the
/// local network (discovery), at every address it was reached at, its host
/// name and `<host>.local`, and through its relay; the first where
/// `GET /.well-known/gentle-shell` names its server id, over HTTPS with the
/// certificate pinned at pairing when it has one, is where it went (a
/// server answering with its id but another certificate isn't it). That
/// address is saved (`at`, beside the token), so the next launch starts
/// there. `None` when it isn't found, or this pairing predates knowing its
/// id. Blocking: up to about 6 s.
fn relocate(url: &str, failed: &str) -> Option<String> {
    let mut searched = SEARCHED.lock().unwrap_or_else(|e| e.into_inner());
    let tokens = read_tokens();
    let entry = tokens.get(url)?;
    // Another request found it meanwhile.
    if answers_at(url, entry) != failed {
        return Some(answers_at(url, entry).to_string());
    }
    if searched
        .get(url)
        .is_some_and(|when| when.elapsed() < SEARCH_AGAIN)
    {
        return None;
    }
    let server_id = entry["serverId"].as_str().filter(|id| !id.is_empty())?;
    let trust = trust_of(entry);
    let candidates = gentle_core::serve::relocation_urls(
        failed,
        &known_addresses(entry),
        entry["host"].as_str(),
        trust.relay.as_deref(),
    );
    let discovery = std::thread::Builder::new()
        .name("serve-relocate".into())
        .spawn(nearby)
        .ok();
    let found = pi_rpc::serve::find(server_id, &candidates, &trust).or_else(|| {
        // Anyone can answer discovery: what answered with the server's id
        // is asked again, pinned.
        let heard: Vec<String> = discovery?
            .join()
            .ok()?
            .ok()?
            .into_iter()
            .filter(|server| server.server_id == server_id)
            .map(|server| server.url())
            .collect();
        pi_rpc::serve::find(server_id, &heard, &trust)
    });
    let Some(found) = found else {
        searched.insert(url.to_string(), Instant::now());
        return None;
    };
    searched.remove(url);
    let mut tokens = tokens.clone();
    if let Some(entry) = tokens.get_mut(url) {
        moved(entry, url, &found);
        if let Err(err) = write_tokens(&tokens) {
            eprintln!("gentle-shell-gpui: could not save the server's new address: {err}");
        }
    }
    eprintln!("gentle-shell-gpui: the Gentle Shell server {url} answers at {found} now");
    Some(found)
}

/// A paired server's addresses, newest last.
fn known_addresses(entry: &Value) -> Vec<String> {
    entry["addresses"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|url| url.as_str().map(str::to_string))
        .collect()
}

/// The server paired at `url` was found at `found`: requests go there from
/// now on, and it is remembered as one of its addresses.
fn moved(entry: &mut Value, url: &str, found: &str) {
    let mut known = known_addresses(entry);
    gentle_core::serve::remember_address(&mut known, found);
    let Some(entry) = entry.as_object_mut() else {
        return;
    };
    entry.insert("addresses".into(), known.into());
    if found == url {
        entry.remove("at");
    } else {
        entry.insert("at".into(), found.into());
    }
}

/// Pair this device with the server at `url` (typed) using `code`: the
/// backend to save, with what the server says about itself. A server that
/// speaks HTTPS is paired with over HTTPS once it proved it knows the code
/// (`pi_rpc::serve::pair_at`). The token is saved here. Blocking.
pub fn pair(url: &str, code: &str) -> Result<(Backend, ServerInfo), String> {
    let url = gentle_core::serve::normalize_url(url)?;
    let code = gentle_core::serve::normalize_code(code)?;
    let (url, paired) = pi_rpc::serve::pair_at(&url, &code, &device_name())?;
    let name = host_of(&url);
    // Its machine's host name, to look for it by should its address change.
    let host = pi_rpc::serve::identify(&url, &paired.trust).map(|(_, host)| host);
    paired_with(url.clone(), &url, name, host, &[], paired)
}

/// Pair this device with the server a connection code names, at `url`
/// (where `pi_rpc::serve::locate` found it on the network, or its relay),
/// named as the code names it. Through a relay the server is still saved
/// under an address of its own (several servers can share a relay), and
/// answers at the relay until it is found on the network. Blocking.
pub fn pair_code(code: &ConnectionCode, url: &str) -> Result<(Backend, ServerInfo), String> {
    let paired = pi_rpc::serve::pair_code(code, url, &device_name())?;
    let key = match code.urls.first() {
        Some(own) if gentle_core::serve::is_relay(url) => own.clone(),
        _ => url.to_string(),
    };
    paired_with(
        key,
        url,
        code.name.clone(),
        Some(code.name.clone()),
        &code.urls,
        paired,
    )
}

/// The Gentle Shell servers on the local network, each checked: the
/// discovery query goes to 255.255.255.255 and to every network's directed
/// broadcast address on this computer. Blocking (about 1.5 s).
pub fn nearby() -> Result<Vec<Nearby>, String> {
    let networks: Vec<(std::net::IpAddr, u8)> = sysinfo::Networks::new_with_refreshed_list()
        .values()
        .flat_map(|data| data.ip_networks().iter().map(|n| (n.addr, n.prefix)))
        .collect();
    pi_rpc::serve::nearby(&gentle_core::serve::discovery_targets(&networks))
}

/// Pair this device with `server` (found by [`nearby`]) using the short
/// pairing `code`, named as it names itself. Blocking.
pub fn pair_nearby(server: &Nearby, code: &str) -> Result<(Backend, ServerInfo), String> {
    let paired = pi_rpc::serve::pair_nearby(server, code, &device_name())?;
    let url = server.url();
    paired_with(
        url.clone(),
        &url,
        server.name.clone(),
        Some(server.name.clone()),
        &[],
        paired,
    )
}

/// How this device names itself to a server.
fn device_name() -> String {
    format!(
        "Gentle Shell GPUI on {}",
        sysinfo::System::host_name().unwrap_or_else(|| "this computer".into())
    )
}

/// Save the token a pairing with the server at `url` returned: the backend
/// to save, named `name`, with what the server says about itself. Its id,
/// machine `host` name and addresses (`url`, and the `others` a connection
/// code listed) are kept beside the token, to find it should its address
/// change.
fn paired_with(
    url: String,
    answers: &str,
    name: String,
    host: Option<String>,
    others: &[String],
    paired: Paired,
) -> Result<(Backend, ServerInfo), String> {
    let client = ServeClient::with_trust(answers, Some(paired.token.clone()), paired.trust.clone());
    let info = client.server().map_err(|e| e.message)?;
    let mut tokens = read_tokens();
    let mut addresses = others.to_vec();
    gentle_core::serve::remember_address(&mut addresses, &url);
    let mut entry = json!({
        "device": paired.device,
        "token": paired.token,
        "serverId": info.server_id,
        "addresses": addresses,
    });
    if answers != url {
        moved(&mut entry, &url, answers);
    }
    if let Some(fp) = &paired.trust.fingerprint {
        entry["fp"] = fp.clone().into();
    }
    if let Some(relay) = &paired.trust.relay {
        entry["relay"] = relay.clone().into();
    }
    if let Some(home) = &info.home {
        entry["home"] = home.clone().into();
    }
    if let Some(host) = host.filter(|host| !host.trim().is_empty()) {
        entry["host"] = host.into();
    }
    tokens.insert(url.clone(), entry);
    write_tokens(&tokens).map_err(|e| format!("Couldn't save the device token: {e}"))?;
    Ok((
        Backend::Server {
            name,
            url,
            device: paired.device,
        },
        info,
    ))
}

/// The host part of a server address, as its default name.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    match rest.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.to_string(),
        _ => rest.to_string(),
    }
}

/// What `choice` (a server) says about itself, with this device's token.
/// Its home folder and server id are kept beside the token ([`known_home`],
/// [`relocate`]; a pairing older than the id learns it here). Blocking.
pub fn info(choice: &Backend) -> Result<ServerInfo, String> {
    let info = client(choice)
        .map_err(|e| e.to_string())?
        .server()
        .map_err(|e| e.message)?;
    if let Backend::Server { url, .. } = choice {
        let mut tokens = read_tokens();
        if let Some(entry) = tokens.get_mut(url).and_then(Value::as_object_mut) {
            let mut changed = false;
            if let Some(home) = info.home.as_ref().filter(|home| !home.is_empty())
                && entry.get("home").and_then(Value::as_str) != Some(home)
            {
                entry.insert("home".into(), home.clone().into());
                changed = true;
            }
            if !info.server_id.is_empty()
                && entry.get("serverId").and_then(Value::as_str) != Some(&info.server_id)
            {
                entry.insert("serverId".into(), info.server_id.clone().into());
                changed = true;
            }
            if changed && let Err(err) = write_tokens(&tokens) {
                eprintln!("gentle-shell-gpui: could not save what the server said: {err}");
            }
        }
    }
    if let Backend::Server { url, .. } = choice {
        INFOS.lock().unwrap().insert(url.clone(), info.clone());
    }
    Ok(info)
}

/// The home folder of the user `choice` (a server) runs as, a path there:
/// where new workspaces and tabs on it start, and Configuration's control
/// session runs. Kept from the last answer, else asked ([`info`]); `None`
/// when this server doesn't say (an older `gentle-shell serve`) or doesn't
/// answer. Blocking the first time.
pub fn home(choice: &Backend) -> Option<String> {
    let Backend::Server { url, .. } = choice else {
        return None;
    };
    known_home(url).or_else(|| info(choice).ok().and_then(|info| info.home))
}

/// The home folder the server at `url` last said it has (kept beside its
/// token), without asking it.
pub fn known_home(url: &str) -> Option<String> {
    read_tokens()
        .get(url)
        .and_then(|entry| entry["home"].as_str())
        .filter(|home| !home.is_empty())
        .map(str::to_string)
}

/// The address that opens `url`, a page a server session gave, on this
/// device: as it is unless it is on the server's own loopback, where it
/// goes through a forward of `session` (`POST /api/sessions/:id/forwards`;
/// the server reuses this device's forward of the port and mints a new
/// single-use link each time), opened through this computer's proxy for
/// it (`ServeClient::local_page`). An error in words when it can't.
/// Blocking.
pub fn page_url(session: &ServerSession, url: &str) -> Result<String, String> {
    match gentle_core::serve::server_page(url) {
        ServerPage::Elsewhere => Ok(url.to_string()),
        ServerPage::Unreachable(why) => Err(why),
        ServerPage::Loopback { port, path } => {
            let client = client_at(&session.url).ok_or_else(|| {
                format!(
                    "This computer isn't paired with the Gentle Shell server at {} any more.",
                    session.url
                )
            })?;
            let forward = client
                .forward(&session.id, port, &path)
                .map_err(|e| e.message)?;
            client.local_page(&forward)
        }
    }
}

/// Gentle Tower running on `choice` (a server), through a forward and this
/// computer's proxy for it: the address to open. Blocking.
pub fn tower(choice: &Backend) -> Result<String, String> {
    let client = client(choice).map_err(|e| e.to_string())?;
    let forward = client.tower().map_err(|e| e.message)?;
    client.local_page(&forward)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_found_elsewhere_answers_there_and_is_remembered() {
        const PAIRED: &str = "http://192.168.1.20:8787";
        const NOW: &str = "http://192.168.1.31:8787";
        let mut entry = json!({"token": "gss_x", "serverId": "srv_1", "addresses": [PAIRED]});
        assert_eq!(answers_at(PAIRED, &entry), PAIRED);

        moved(&mut entry, PAIRED, NOW);
        assert_eq!(answers_at(PAIRED, &entry), NOW);
        assert_eq!(known_addresses(&entry), [PAIRED, NOW]);
        // The token and the id stay.
        assert_eq!(entry["token"], "gss_x");
        assert_eq!(entry["serverId"], "srv_1");

        // Back at the address it was paired at: nothing to redirect.
        moved(&mut entry, PAIRED, PAIRED);
        assert!(entry.get("at").is_none());
        assert_eq!(answers_at(PAIRED, &entry), PAIRED);
        assert_eq!(known_addresses(&entry), [NOW, PAIRED]);
    }
}
