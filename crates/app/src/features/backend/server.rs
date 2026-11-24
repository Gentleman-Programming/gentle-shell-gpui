//! Gentle Shell servers (`gentle-shell serve`) as where agents run: pairing
//! this device with one, and the device token pairing returns. The token is a
//! secret, so it stays out of `settings.json` (which names only the server,
//! the device id and the server's name): it lives in `server-tokens.json` in
//! the app's folder, owner-only (`0600`), keyed by server address.

use std::path::PathBuf;

use gentle_core::backend::Backend;
use pi_rpc::serve::{ServeClient, ServerInfo};
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
    let name = format!(
        "Gentle Shell GPUI on {}",
        sysinfo::System::host_name().unwrap_or_else(|| "this computer".into())
    );
    let paired = ServeClient::new(&url, None)
        .pair(&code, &name)
        .map_err(|e| e.message)?;
    let client = ServeClient::new(&url, Some(paired.token.clone()));
    let info = client.server().map_err(|e| e.message)?;
    let mut tokens = read_tokens();
    tokens.insert(
        url.clone(),
        json!({"device": paired.device, "token": paired.token}),
    );
    write_tokens(&tokens).map_err(|e| format!("Couldn't save the device token: {e}"))?;
    Ok((
        Backend::Server {
            name: host_of(&url),
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
/// Blocking.
pub fn info(choice: &Backend) -> Result<ServerInfo, String> {
    client(choice)
        .map_err(|e| e.to_string())?
        .server()
        .map_err(|e| e.message)
}
