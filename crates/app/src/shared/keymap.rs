//! The user's keymap file on disk: `<data>/gentle-shell-gpui/keymap.json`.
//! The app owns it and only creates it when the user runs "Open keymap file".
//! Parsing and validation live in `gentle_core::keymap`; the file accepts
//! comments and a comma before a closing bracket (JSONC).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gentle_core::keymap::{self, Keymap};
use gentle_core::shortcuts::{self, Binding};

use super::data_dir;

/// The keymap the app binds with no user file, with the leader it was bound
/// with, captured every time it binds (`app::bind_keys`). The template's
/// reference is generated from it, so the file can't list keys the app
/// doesn't bind.
static DEFAULTS: Mutex<Option<(String, Vec<Binding>)>> = Mutex::new(None);

pub fn path() -> Option<PathBuf> {
    data_dir::app().map(|dir| dir.join("keymap.json"))
}

/// The file's keymap. A missing file is the defaults without an error; an
/// unreadable one is the defaults plus one error.
pub fn load() -> Keymap {
    let Some(path) = path() else {
        return Keymap::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => keymap::parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Keymap::default(),
        Err(e) => {
            let mut k = Keymap::default();
            k.errors.push(keymap::Issue {
                index: None,
                message: format!("couldn't read {}: {e}", path.display()),
            });
            k
        }
    }
}

/// Remember the defaults the app just bound, and the leader they use.
pub fn remember_defaults(leader: &str, bindings: Vec<Binding>) {
    if let Ok(mut slot) = DEFAULTS.lock() {
        *slot = Some((leader.to_string(), bindings));
    }
}

/// The defaults as they were bound, without touching them (tests: the
/// template's reference is these).
#[cfg(test)]
pub fn default_keymap() -> Vec<Binding> {
    DEFAULTS
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .map(|(_, bindings)| bindings)
        .unwrap_or_default()
}

/// The defaults with their leader bindings spelling `leader`, so a template
/// created after the leader changed still matches its own `"leader"`.
fn defaults_for(leader: &str) -> Vec<Binding> {
    let snapshot = DEFAULTS.lock().ok().and_then(|slot| slot.clone());
    let Some((bound, bindings)) = snapshot else {
        return Vec::new();
    };
    with_leader(&bound, bindings, leader)
}

fn with_leader(bound: &str, mut bindings: Vec<Binding>, leader: &str) -> Vec<Binding> {
    for binding in &mut bindings {
        if binding.keys.len() > 1 && shortcuts::same_keys(&binding.keys[0], bound) {
            binding.keys[0] = leader.to_string();
        }
    }
    bindings
}

/// The file's path, created from the documented template when missing (with
/// `leader`), and upgraded when it is still the empty file older versions
/// wrote. Any other content is never touched.
pub fn ensure(leader: &str) -> Result<PathBuf, String> {
    let path = path().ok_or("no data folder on this system")?;
    create_or_upgrade(&path, leader, &defaults_for(leader))?;
    Ok(path)
}

fn create_or_upgrade(path: &Path, leader: &str, defaults: &[Binding]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    }
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return create(path, &keymap::template(leader, defaults));
        }
        Err(e) => return Err(format!("couldn't read {}: {e}", path.display())),
    };
    // The old template held only a leader and an empty `bindings`: everything
    // the app knew, and nothing the user added. Keep the leader; anything else
    // — an entry, a comment, another key — is the user's and stays as it is.
    if keymap::bare_template_leader(&existing).is_none() {
        return Ok(());
    }
    let leader = keymap::parse(&existing).leader;
    std::fs::write(path, keymap::template(&leader, defaults))
        .map_err(|e| format!("couldn't write {}: {e}", path.display()))
}

fn create(path: &Path, text: &str) -> Result<(), String> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => file
            .write_all(text.as_bytes())
            .map_err(|e| format!("couldn't write {}: {e}", path.display())),
        // Someone created it while we looked; never overwrite that.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!("couldn't create {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::{create_or_upgrade, with_leader};
    use gentle_core::keymap;
    use gentle_core::shortcuts::Binding;

    /// A scratch folder of this process, outside the repository.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-keymap-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn defaults() -> Vec<Binding> {
        vec![
            Binding {
                keys: vec!["cmd-t".into()],
                action: "gentle::NewTab".into(),
                context: None,
            },
            Binding {
                keys: vec!["ctrl-a".into(), "h".into()],
                action: "gentle::FocusPaneLeft".into(),
                context: None,
            },
        ]
    }

    #[test]
    fn creates_the_documented_template_but_never_overwrites() {
        let dir = scratch("create");
        let file = dir.join("keymap.json");
        create_or_upgrade(&file, "ctrl-a", &defaults()).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("// "), "{text}");
        assert!(text.contains("\"leader\": \"ctrl-a\""));
        assert!(text.contains("cmd-t") && text.contains("gentle::NewTab"));
        assert!(text.contains("ctrl-a h"));
        let parsed = keymap::parse(&text);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert_eq!(parsed.leader, "ctrl-a");
        assert!(parsed.bindings.is_empty());

        // An edited file is never rewritten.
        std::fs::write(&file, "mine").unwrap();
        create_or_upgrade(&file, "ctrl-a", &defaults()).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn upgrades_only_the_old_empty_template_and_keeps_its_leader() {
        let dir = scratch("upgrade");
        let file = dir.join("keymap.json");
        std::fs::write(
            &file,
            "{\n  \"leader\": \"ctrl-b\",\n  \"bindings\": []\n}\n",
        )
        .unwrap();
        create_or_upgrade(&file, "ctrl-space", &defaults()).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("// "), "{text}");
        assert!(text.contains("\"leader\": \"ctrl-b\""), "{text}");
        assert!(text.contains("// "));
        let parsed = keymap::parse(&text);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert_eq!(parsed.leader, "ctrl-b");

        // Any other content — even the same JSON with a comment — is the
        // user's file: every byte stays.
        for mine in [
            "{\"leader\": \"ctrl-b\", \"bindings\": [{\"keys\": \"cmd-l\", \"action\": null}]}",
            "{\"leader\": \"ctrl-b\", \"bindings\": [], \"version\": 2}",
            "// mine\n{\"leader\": \"ctrl-b\", \"bindings\": []}",
            "mine",
        ] {
            std::fs::write(&file, mine).unwrap();
            create_or_upgrade(&file, "ctrl-space", &defaults()).unwrap();
            assert_eq!(std::fs::read_to_string(&file).unwrap(), mine, "{mine:?}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_reference_follows_the_template_leader() {
        let rewritten = with_leader("ctrl-a", defaults(), "ctrl-space");
        let keys: Vec<String> = rewritten.iter().map(|b| b.keys.join(" ")).collect();
        assert_eq!(keys, ["cmd-t", "ctrl-space h"]);
        // Same leader: nothing moves.
        assert_eq!(with_leader("ctrl-a", defaults(), "ctrl-a"), defaults());
        // Only the leader's own keystroke is rewritten, not any sequence.
        assert_eq!(defaults()[0].keys, ["cmd-t"]);
    }
}
