//! Several machines side by side: this computer (or the WSL distro or
//! command environment it reaches) and the Gentle Shell servers the app
//! paired with. Each workspace keeps the machine it was opened on; the
//! default ([`crate::backend::KEY`], "Agents run on") only decides where a
//! new workspace goes. These are the rules that pick a machine: which one a
//! path belongs to, what a workspace saved before machines were kept runs
//! on, and which machines Configuration and Open folder offer.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::backend::Backend;

/// The workspaces file version from which each workspace saves its own
/// machine; one saved before ran where the default said.
pub const VERSION: u32 = 2;

/// The machine `path` belongs to, among `roots` (each a folder and its
/// machine, in order of preference: the shown workspace first): the one
/// whose folder holds it most closely; between equally close folders (the
/// same path on two machines), the earlier. `None` when no folder holds it.
pub fn owner<'a, M>(path: &Path, roots: &'a [(PathBuf, M)]) -> Option<&'a M> {
    let mut best: Option<(usize, &M)> = None;
    for (root, machine) in roots {
        if !path.starts_with(root) {
            continue;
        }
        let depth = root.components().count();
        if best.is_none_or(|(deepest, _)| depth > deepest) {
            best = Some((depth, machine));
        }
    }
    best.map(|(_, machine)| machine)
}

/// What a workspace that isn't a server's runs on, as saved: its own
/// `backend` (a WSL distro or a command) when it has one; else, saved by
/// `version` older than [`VERSION`], where agents ran then (the `default`,
/// unless that is a server: such a workspace's folder is this computer's);
/// else this computer.
pub fn saved_machine(version: u32, saved: Option<&Value>, default: &Backend) -> Backend {
    let machine = match saved {
        Some(value) => Backend::from_json(value),
        None if version < VERSION => default.clone(),
        None => Backend::Local,
    };
    if machine.is_server() {
        Backend::Local
    } else {
        machine
    }
}

/// Whether `a` and `b` are the same machine: a server by its address (its
/// name and device may be told again), anything else as it is.
pub fn same(a: &Backend, b: &Backend) -> bool {
    match (a, b) {
        (Backend::Server { url: a, .. }, Backend::Server { url: b, .. }) => a == b,
        _ => a == b,
    }
}

/// The machines Configuration's "Settings for" and Open folder offer: this
/// computer; the environment the `active` workspace or the `default` runs
/// in when it is another one here (a WSL distro, a command); then every
/// `paired` server. Each once.
pub fn choices(active: &Backend, default: &Backend, paired: &[Backend]) -> Vec<Backend> {
    let mut all: Vec<Backend> = vec![Backend::Local];
    for machine in [active, default]
        .into_iter()
        .filter(|machine| !machine.is_server())
        .chain(paired)
    {
        if !all.iter().any(|known| same(known, machine)) {
            all.push(machine.clone());
        }
    }
    all
}

/// Which of `choices` ([`choices`]) a screen opens on: the `active`
/// workspace's machine; this computer when that isn't offered (a server
/// this device isn't paired with any more).
pub fn pick(active: &Backend, choices: &[Backend]) -> usize {
    choices
        .iter()
        .position(|machine| same(machine, active))
        .unwrap_or(0)
}

/// What a machine is called in a list of machines: "This Mac" (or "This
/// PC", "This computer"), a server by its name (its address without one),
/// anything else as [`Backend::label`] says.
pub fn name(machine: &Backend) -> String {
    match machine {
        Backend::Local => this_computer().to_string(),
        Backend::Server { name, .. } if !name.trim().is_empty() => name.trim().to_string(),
        Backend::Server { url, .. } => url
            .split_once("://")
            .map_or(url.as_str(), |(_, rest)| rest)
            .to_string(),
        other => other.label(),
    }
}

/// This computer, as its system calls it.
pub fn this_computer() -> &'static str {
    if cfg!(target_os = "macos") {
        "This Mac"
    } else if cfg!(windows) {
        "This PC"
    } else {
        "This computer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn server(url: &str, name: &str) -> Backend {
        Backend::Server {
            url: url.into(),
            device: "dev_1".into(),
            name: name.into(),
        }
    }

    fn orb() -> Backend {
        Backend::Command {
            prefix: vec!["orb".into(), "--".into()],
        }
    }

    #[test]
    fn a_path_belongs_to_the_workspace_folder_that_holds_it_most_closely() {
        let pc = server("http://pc:8787", "pc");
        let roots = vec![
            (PathBuf::from("/Users/ana/web"), Backend::Local),
            (PathBuf::from("/home/ana/api"), pc.clone()),
            (PathBuf::from("/home/ana"), pc.clone()),
            (PathBuf::from("/Users/ana/web/vendor/lib"), orb()),
        ];
        assert_eq!(
            owner(Path::new("/Users/ana/web/src/a.rs"), &roots),
            Some(&Backend::Local)
        );
        assert_eq!(owner(Path::new("/home/ana/api/src"), &roots), Some(&pc));
        // A server's home holds what its workspaces don't.
        assert_eq!(owner(Path::new("/home/ana/notes"), &roots), Some(&pc));
        assert_eq!(
            owner(Path::new("/Users/ana/web/vendor/lib/x"), &roots),
            Some(&orb())
        );
        // A sibling whose name merely starts the same isn't inside.
        assert_eq!(owner(Path::new("/Users/ana/website"), &roots), None);
        assert_eq!(owner(Path::new("/tmp"), &roots), None);
    }

    #[test]
    fn the_same_folder_on_two_machines_goes_to_the_preferred_one() {
        let (a, b) = (server("http://a:8787", "a"), server("http://b:8787", "b"));
        let roots = vec![
            (PathBuf::from("/srv/app"), b.clone()),
            (PathBuf::from("/srv/app"), a.clone()),
        ];
        assert_eq!(owner(Path::new("/srv/app/x"), &roots), Some(&b));
    }

    #[test]
    fn a_workspace_saved_before_machines_runs_where_the_default_said() {
        let wsl = json!({"kind": "wsl", "distro": "Ubuntu"});
        let pc = server("http://pc:8787", "pc");
        // Before: the default, unless that was a server (the folder is this
        // computer's, its server tabs say so themselves).
        assert_eq!(saved_machine(1, None, &orb()), orb());
        assert_eq!(saved_machine(1, None, &pc), Backend::Local);
        assert_eq!(saved_machine(1, None, &Backend::Local), Backend::Local);
        // Since: its own, never the default.
        assert_eq!(saved_machine(VERSION, None, &orb()), Backend::Local);
        assert_eq!(
            saved_machine(VERSION, Some(&wsl), &orb()),
            Backend::Wsl {
                distro: "Ubuntu".into()
            }
        );
        // A server is never saved here: it goes by the workspace's server.
        assert_eq!(
            saved_machine(VERSION, Some(&pc.to_json().unwrap()), &orb()),
            Backend::Local
        );
    }

    #[test]
    fn screens_offer_this_computer_its_environments_and_every_paired_server() {
        let (a, b) = (server("http://a:8787", "a"), server("http://b:8787", "b"));
        let paired = [a.clone(), b.clone()];
        assert_eq!(
            choices(&Backend::Local, &Backend::Local, &paired),
            vec![Backend::Local, a.clone(), b.clone()]
        );
        // The default's environment is offered, and a server default once.
        assert_eq!(
            choices(&Backend::Local, &orb(), &paired),
            vec![Backend::Local, orb(), a.clone(), b.clone()]
        );
        assert_eq!(
            choices(&a, &a, &paired),
            vec![Backend::Local, a.clone(), b.clone()]
        );
    }

    #[test]
    fn screens_open_on_the_active_workspaces_machine() {
        let (a, b) = (server("http://a:8787", "a"), server("http://b:8787", "b"));
        let list = choices(&b, &Backend::Local, &[a.clone(), b.clone()]);
        assert_eq!(pick(&b, &list), 2);
        // Renamed since: still the same server.
        assert_eq!(pick(&server("http://b:8787", "renamed"), &list), 2);
        assert_eq!(pick(&Backend::Local, &list), 0);
        // A server not paired any more: this computer.
        assert_eq!(pick(&server("http://gone:8787", "gone"), &list), 0);
        let list = choices(&orb(), &Backend::Local, &[a]);
        assert_eq!(pick(&orb(), &list), 1);
    }

    #[test]
    fn machines_are_named_for_a_list() {
        assert_eq!(name(&Backend::Local), this_computer());
        assert_eq!(name(&server("http://pc:8787", " pc ")), "pc");
        assert_eq!(name(&server("https://10.0.0.2:8787", "")), "10.0.0.2:8787");
        assert_eq!(name(&orb()), "orb --");
    }
}
