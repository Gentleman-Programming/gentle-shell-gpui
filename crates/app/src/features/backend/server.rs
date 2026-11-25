//! Gentle Shell servers (`gentle-shell serve`) as where agents run: pairing
//! this device with one (at a typed address, found on the local network for
//! a short pairing code, or wherever on the network a connection code's
//! server answers), and the device token pairing returns. The token is a
//! secret, so it stays out of `settings.json` (which names only the server,
//! the device id and the server's name): it lives in `server-tokens.json` in
//! the app's folder, owner-only (`0600`), keyed by server address.

use std::path::PathBuf;

use gentle_core::backend::Backend;
use gentle_core::serve::{ConnectionCode, Nearby, ServerPage, ServerSession};
use pi_rpc::serve::{Paired, ServeClient, ServerInfo};
use serde_json::{Map, Value, json};

/// What a server tab can't do yet: the server has no terminals, files or
/// git for its clients (Gentle Shell's serve phase 1).
pub const UNAVAILABLE: &str = "Not available on a Gentle Shell server yet.";

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

/// The device token for the server at `url`.
fn token(url: &str) -> Option<String> {
    read_tokens()
        .get(url)
        .and_then(|entry| entry["token"].as_str())
        .map(str::to_string)
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

/// A client of `choice` with its token; an error in words when this device
/// isn't paired with it (any more).
pub fn client(choice: &Backend) -> anyhow::Result<ServeClient> {
    let Backend::Server { url, .. } = choice else {
        anyhow::bail!("{} isn't a Gentle Shell server.", choice.label());
    };
    let token = token(url).ok_or_else(|| {
        anyhow::anyhow!(
            "This computer isn't paired with the Gentle Shell server at {url}. Pair it in \
             Configuration › General › Where agents run."
        )
    })?;
    Ok(ServeClient::new(url, Some(token)))
}

/// Pair this device with the server at `url` (typed) using `code`: the
/// backend to save, with what the server says about itself. The token is
/// saved here. Blocking.
pub fn pair(url: &str, code: &str) -> Result<(Backend, ServerInfo), String> {
    let url = gentle_core::serve::normalize_url(url)?;
    let code = gentle_core::serve::normalize_code(code)?;
    let paired = ServeClient::new(&url, None)
        .pair(&code, &device_name())
        .map_err(|e| e.message)?;
    let name = host_of(&url);
    paired_with(url, name, paired)
}

/// Pair this device with the server a connection code names, at `url`
/// (where `pi_rpc::serve::locate` found it on the network), named as the
/// code names it. Blocking.
pub fn pair_code(code: &ConnectionCode, url: &str) -> Result<(Backend, ServerInfo), String> {
    let paired = pi_rpc::serve::pair_code(code, url, &device_name())?;
    paired_with(url.to_string(), code.name.clone(), paired)
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
    paired_with(server.url(), server.name.clone(), paired)
}

/// How this device names itself to a server.
fn device_name() -> String {
    format!(
        "Gentle Shell GPUI on {}",
        sysinfo::System::host_name().unwrap_or_else(|| "this computer".into())
    )
}

/// Save the token a pairing with the server at `url` returned: the backend
/// to save, named `name`, with what the server says about itself.
fn paired_with(url: String, name: String, paired: Paired) -> Result<(Backend, ServerInfo), String> {
    let client = ServeClient::new(&url, Some(paired.token.clone()));
    let info = client.server().map_err(|e| e.message)?;
    let mut tokens = read_tokens();
    let mut entry = json!({"device": paired.device, "token": paired.token});
    if let Some(home) = &info.home {
        entry["home"] = home.clone().into();
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
    let rest = url.trim_start_matches("http://");
    match rest.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.to_string(),
        _ => rest.to_string(),
    }
}

/// What `choice` (a server) says about itself, with this device's token.
/// Its home folder is kept beside the token for [`known_home`]. Blocking.
pub fn info(choice: &Backend) -> Result<ServerInfo, String> {
    let info = client(choice)
        .map_err(|e| e.to_string())?
        .server()
        .map_err(|e| e.message)?;
    if let (Backend::Server { url, .. }, Some(home)) = (choice, &info.home)
        && known_home(url).as_ref() != Some(home)
    {
        let mut tokens = read_tokens();
        if let Some(entry) = tokens.get_mut(url).and_then(Value::as_object_mut) {
            entry.insert("home".into(), home.clone().into());
            if let Err(err) = write_tokens(&tokens) {
                eprintln!("gentle-shell-gpui: could not save the server's home folder: {err}");
            }
        }
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
/// single-use link each time). An error in words when it can't. Blocking.
pub fn page_url(session: &ServerSession, url: &str) -> Result<String, String> {
    match gentle_core::serve::server_page(url) {
        ServerPage::Elsewhere => Ok(url.to_string()),
        ServerPage::Unreachable(why) => Err(why),
        ServerPage::Loopback { port, path } => {
            let token = token(&session.url).ok_or_else(|| {
                format!(
                    "This computer isn't paired with the Gentle Shell server at {} any more.",
                    session.url
                )
            })?;
            ServeClient::new(&session.url, Some(token))
                .forward(&session.id, port, &path)
                .map(|forward| forward.url)
                .map_err(|e| e.message)
        }
    }
}

/// Gentle Tower running on `choice` (a server), through a forward: the
/// address to open. Blocking.
pub fn tower(choice: &Backend) -> Result<String, String> {
    client(choice)
        .map_err(|e| e.to_string())?
        .tower()
        .map(|forward| forward.url)
        .map_err(|e| e.message)
}
