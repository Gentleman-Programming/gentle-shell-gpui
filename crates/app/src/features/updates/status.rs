//! Gentle Shell's own update status (`get_update_status`), for the
//! Configuration screen when the active agent tab's Shell advertises it:
//! Gentle Shell's release (as the same offer the npm path makes), Pi, and
//! Pi's packages. The npm registry path stays the answer for every other
//! Shell.

use gentle_core::version::offer_for;
use pi_rpc::desktop_rpc::{
    PackageKind, PackageUpdate, PackageUpdateState, ShellChannel, ShellUpdate, UpdateState,
    UpdateStatus,
};

use super::UpdateOffer;

/// The "Gentle Shell X is available" offer from Gentle Shell's answer: only
/// a release (or bundled) install with a newer stable release, never a
/// skipped version, and never when gentle-ai manages the install (it updates
/// it itself; `managed` is the app's own launcher check, the `channel` the
/// Shell's).
pub fn offer_from_status(
    shell: &ShellUpdate,
    managed: bool,
    skipped: Option<&str>,
) -> Option<UpdateOffer> {
    if managed
        || shell.state != UpdateState::UpdateAvailable
        || !matches!(shell.channel, ShellChannel::Release | ShellChannel::Bundled)
    {
        return None;
    }
    offer_for(shell.current.as_deref()?, shell.latest.as_deref()?, skipped)
}

/// Lines worth showing beside the versions: a newer `main` commit, a newer
/// Pi, and an offline Shell that looked nothing up. Each names the command
/// Gentle Shell gives for it; the app does not run those. Nothing for a
/// managed install: gentle-ai owns its updates.
pub fn status_notes(status: &UpdateStatus, managed: bool) -> Vec<(&'static str, String)> {
    let mut notes = Vec::new();
    if managed || status.gentle_shell.channel == ShellChannel::GentleAiManaged {
        return notes;
    }
    let shell = &status.gentle_shell;
    if shell.channel == ShellChannel::Main
        && shell.state == UpdateState::UpdateAvailable
        && let (Some(latest), Some(command)) = (&shell.latest, &shell.command)
    {
        notes.push((
            "Gentle Shell main",
            format!("newer commit {latest} · run `{command}`"),
        ));
    }
    let pi = &status.pi;
    if pi.state == UpdateState::UpdateAvailable {
        let latest = pi.latest.as_deref().unwrap_or("a newer version");
        let current = pi
            .current
            .as_deref()
            .map(|c| format!(" (you have {c})"))
            .unwrap_or_default();
        notes.push((
            "Pi",
            format!("{latest} is available{current} · run `{}`", pi.command),
        ));
    }
    if status.offline {
        notes.push((
            "Update check",
            "Gentle Shell is offline (PI_OFFLINE): nothing was looked up".to_string(),
        ));
    }
    notes
}

/// Gentle Shell's row for a package configured as `source` (`npm:name@v`,
/// `git:host/path@ref`, `https://host/path.git`, or a local path).
pub fn package_status<'a>(source: &str, items: &'a [PackageUpdate]) -> Option<&'a PackageUpdate> {
    let npm = crate::features::settings::npm_name(source);
    let git = {
        let rest = source.strip_prefix("git:").unwrap_or(source);
        let rest = rest
            .split_once("://")
            .map_or(rest, |(_, after)| after)
            .split('@')
            .next()
            .unwrap_or(rest);
        rest.strip_suffix(".git").unwrap_or(rest)
    };
    let local = std::path::Path::new(source)
        .file_name()
        .and_then(|n| n.to_str());
    items.iter().find(|item| match item.kind {
        PackageKind::Npm => npm == Some(item.name.as_str()),
        PackageKind::Git => git == item.name,
        PackageKind::Local => local == Some(item.name.as_str()),
        PackageKind::Unknown => false,
    })
}

/// How many configured packages Pi's check says have a newer version.
pub fn packages_with_updates(status: &UpdateStatus) -> usize {
    status
        .packages
        .items
        .iter()
        .filter(|item| item.state == PackageUpdateState::UpdateAvailable)
        .count()
}

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{DesktopRecord, SCHEMA, UpdateStatus, decode_payload};

    use super::{offer_from_status, package_status, packages_with_updates, status_notes};

    fn status(data: serde_json::Value) -> UpdateStatus {
        let line = serde_json::json!({
            "schema": SCHEMA, "kind": "response", "requestId": "desktop-71", "ok": true, "data": data,
        })
        .to_string();
        match decode_payload(&line).unwrap() {
            DesktopRecord::Response(response) => response.update_status().unwrap(),
            other => panic!("expected a response, got {other:?}"),
        }
    }

    /// The spec's example, with `channel` and Pi's state adjustable.
    fn example(channel: &str, latest: &str, pi_state: &str) -> UpdateStatus {
        status(serde_json::json!({
            "refreshed": true,
            "offline": false,
            "gentleShell": { "current": "4.8.0", "latest": latest, "channel": channel, "state": "update_available", "command": "gentle-shell upgrade", "checkedAt": "2026-11-17T12:00:00.000Z" },
            "pi": { "current": "1.5.2", "latest": null, "state": pi_state, "command": "gentle-shell update", "checkedAt": null },
            "packages": {
                "available": true,
                "items": [
                    { "name": "pi-web-access", "kind": "npm", "scope": "user", "current": "1.4.0", "latest": null, "pinned": false, "state": "update_available" },
                    { "name": "github.com/user/repo", "kind": "git", "scope": "project", "current": null, "latest": null, "pinned": true, "state": "skipped" }
                ],
                "truncated": false,
                "command": "gentle-shell update --extensions",
                "checkedAt": "2026-11-17T12:00:00.000Z"
            }
        }))
    }

    #[test]
    fn a_release_update_is_offered_like_the_npm_path() {
        let status = example("release", "4.9.0", "unknown");
        let offer = offer_from_status(&status.gentle_shell, false, None).unwrap();
        assert_eq!(
            (offer.current.as_str(), offer.latest.as_str()),
            ("4.8.0", "4.9.0")
        );
        assert!(offer.notes_url.ends_with("/v4.9.0"));
        // Skipped, managed by gentle-ai (either way), or another channel: none.
        assert_eq!(
            offer_from_status(&status.gentle_shell, false, Some("4.9.0")),
            None
        );
        assert_eq!(offer_from_status(&status.gentle_shell, true, None), None);
        for channel in ["gentle-ai-managed", "main", "unknown"] {
            let other = example(channel, "4.9.0", "unknown");
            assert_eq!(
                offer_from_status(&other.gentle_shell, false, None),
                None,
                "{channel}"
            );
        }
        assert!(
            offer_from_status(
                &example("bundled", "4.9.0", "unknown").gentle_shell,
                false,
                None
            )
            .is_some()
        );
    }

    #[test]
    fn notes_name_the_commands_and_stay_silent_when_managed() {
        let main = example("main", "0336985abcde", "update_available");
        let notes = status_notes(&main, false);
        assert_eq!(notes.len(), 2);
        assert!(notes[0].1.contains("0336985abcde"));
        assert!(notes[0].1.contains("gentle-shell upgrade"));
        assert!(notes[1].1.contains("gentle-shell update"));
        assert!(status_notes(&main, true).is_empty());
        assert!(
            status_notes(
                &example("gentle-ai-managed", "4.9.0", "update_available"),
                false
            )
            .is_empty()
        );
        assert!(status_notes(&example("release", "4.9.0", "unknown"), false).is_empty());
    }

    #[test]
    fn packages_match_their_configured_sources() {
        let status = example("release", "4.9.0", "unknown");
        let items = &status.packages.items;
        assert_eq!(
            package_status("npm:pi-web-access@1.4.0", items)
                .unwrap()
                .name,
            "pi-web-access"
        );
        assert_eq!(
            package_status("git:github.com/user/repo@v2", items)
                .unwrap()
                .name,
            "github.com/user/repo"
        );
        assert_eq!(
            package_status("https://github.com/user/repo.git", items)
                .unwrap()
                .name,
            "github.com/user/repo"
        );
        assert!(package_status("npm:other", items).is_none());
        assert_eq!(packages_with_updates(&status), 1);
    }
}
