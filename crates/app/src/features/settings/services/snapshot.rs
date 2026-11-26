//! Read and enrich one configuration snapshot.
//!
//! Runs only on the background executor. Keep this in the same order as the
//! prior SettingsView::reload implementation: read files, resolve launcher,
//! probe, check Gentle Shell update, check Engram update.
//!
//! Where agents run on a Gentle Shell server nothing of that runs: the
//! snapshot is what the server says about itself ([`remote`]), and the
//! screen fills in what its Gentle Shell reports over Desktop RPC.

use std::path::Path;

use gentle_core::backend::Backend;
use pi_rpc::serve::ServerInfo;

use super::super::remote::ServerPlace;
use super::super::snapshot::{ServerFacts, Snapshot, probe, read_snapshot};

pub(in crate::features::settings) fn load(
    cwd: &Path,
    shell_updates: bool,
    server: Option<&ServerPlace>,
) -> Snapshot {
    let mut snapshot = match server {
        Some(place) => remote(place, crate::features::backend::server::info),
        None => local(cwd, shell_updates),
    };
    snapshot.engram_update = crate::features::setup::engram_version()
        .and_then(|v| crate::features::updates::engram::check(&v));
    snapshot
}

/// This computer's (or WSL's, or a command's) Gentle Shell: its homes'
/// files, its launcher and the npm update check.
fn local(cwd: &Path, shell_updates: bool) -> Snapshot {
    // Where agents run in WSL, its homes; never this computer's by mistake
    // while they are still being asked for.
    crate::features::backend::ensure_homes();
    let homes = pi_rpc::resolve_homes();
    let mut snapshot = read_snapshot(&homes.agent, &homes.config, cwd);
    let spec = crate::features::backend::shell_spec(cwd)
        .or_else(|_| crate::features::backend::home_shell_spec())
        .ok();
    if let Some(spec) = &spec {
        probe(spec, &mut snapshot);
    }
    // In a backend the launcher is its own `gentle-shell`, named with where;
    // a Windows one WSL sees through interop isn't (as Setup says).
    let choice = crate::features::backend::for_tab(cwd);
    if choice != Backend::Local {
        let label = choice.label();
        snapshot.launcher = crate::features::backend::gentle_shell_path(&choice).map(|path| {
            if gentle_core::backend::is_windows_file(&path) {
                format!("not found in {label} (only Windows' {path})").into()
            } else {
                format!("{path} ({label})").into()
            }
        });
    }
    // gentle-ai owns updates of installs it manages; a responding Shell
    // supplies update status in preference to the launcher registry check.
    snapshot.managed = spec
        .as_ref()
        .is_some_and(pi_rpc::LaunchSpec::managed_by_gentle_ai);
    snapshot.skipped_release = crate::features::updates::skipped();
    if !snapshot.managed && !shell_updates {
        snapshot.update = crate::features::updates::current_from_versions(&snapshot.versions)
            .and_then(crate::features::updates::check);
    }
    snapshot
}

/// A Gentle Shell server's snapshot: only what `info` (its `GET
/// /api/server`) says, its home folder and version. No home, file, launcher
/// or release check of this computer's is read for it.
pub(in crate::features::settings) fn remote(
    place: &ServerPlace,
    info: impl FnOnce(&Backend) -> Result<ServerInfo, String>,
) -> Snapshot {
    let mut facts = ServerFacts {
        name: place.name.clone(),
        ..ServerFacts::default()
    };
    match info(&place.backend) {
        Ok(info) => {
            facts.home = info.home;
            facts.version = info.version;
        }
        Err(error) => facts.error = Some(error),
    }
    Snapshot {
        versions: facts
            .version
            .iter()
            .map(|version| format!("Gentle Shell {version} on {}", place.name))
            .collect(),
        server: Some(facts),
        ..Snapshot::default()
    }
}

#[cfg(test)]
mod tests {
    use gentle_core::backend::Backend;
    use pi_rpc::serve::ServerInfo;

    use super::super::super::remote::ServerPlace;
    use super::remote;

    fn place() -> ServerPlace {
        ServerPlace::of(&Backend::Server {
            url: "http://studio:7420".into(),
            device: "dev_1".into(),
            name: "studio".into(),
        })
        .unwrap()
    }

    /// The server's snapshot holds only what the server said: no home
    /// folder, package, sign-in or launcher of this computer's.
    #[test]
    fn a_server_snapshot_is_only_what_the_server_says() {
        let s = remote(&place(), |backend| {
            assert!(backend.is_server(), "asks the server, nothing else");
            Ok(ServerInfo {
                server_id: "srv_1".into(),
                version: Some("5.1.0".into()),
                home: Some("/home/ana".into()),
                ..ServerInfo::default()
            })
        });
        let facts = s.server.as_ref().unwrap();
        assert_eq!(facts.name, "studio");
        assert_eq!(facts.home.as_deref(), Some("/home/ana"));
        assert_eq!(s.versions, vec!["Gentle Shell 5.1.0 on studio".to_string()]);
        assert!(s.agent.as_os_str().is_empty() && s.config.as_os_str().is_empty());
        assert!(s.launcher.is_none() && s.packages.is_empty() && s.sign_ins.is_empty());
        assert!(s.update.is_none() && s.skipped_release.is_none() && !s.managed);
    }

    #[test]
    fn a_server_that_does_not_answer_says_why() {
        let s = remote(&place(), |_| Err("Can't reach studio".into()));
        let facts = s.server.unwrap();
        assert_eq!(facts.error.as_deref(), Some("Can't reach studio"));
        assert!(facts.home.is_none() && s.versions.is_empty());
    }
}
