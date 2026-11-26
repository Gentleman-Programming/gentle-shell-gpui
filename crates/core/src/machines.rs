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

/// What a screen about one machine (Profiles, Usage) says it shows, after
/// its title: "This Mac", "<name> (server)", "WSL · <distro>", a command.
pub fn screen_label(machine: &Backend) -> String {
    match machine {
        Backend::Local => this_computer().to_string(),
        other => other.label(),
    }
}

/// What a screen's project-scoped actions (a repository pin, the session's
/// profile) act on when it shows `shown` and the window's workspace runs on
/// `active` in `folder`: "<folder> on <machine>". They act on that
/// workspace only, so on another machine they are off, and `Err` says why.
pub fn project_target(shown: &Backend, active: &Backend, folder: &Path) -> Result<String, String> {
    if !same(shown, active) {
        return Err(format!(
            "Acts on the open workspace, which runs on {}; switch to it to use this.",
            name(active)
        ));
    }
    let text = folder.to_string_lossy();
    let base = text
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|base| !base.is_empty())
        .unwrap_or(&text);
    Ok(format!("{base} on {}", name(shown)))
}

/// A screen about one machine at a time (Profiles, Usage): the one it
/// `shows`, the `choices` it can switch to, and the machine the window's
/// open workspace runs on (`active`), where its project actions act.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub shows: Backend,
    pub choices: Vec<Backend>,
    pub active: Backend,
}

impl Shown {
    /// Open on `shows` (a machine chosen elsewhere, e.g. Configuration's
    /// "Settings for"), else on the `active` workspace's machine ([`pick`]).
    pub fn new(shows: Option<Backend>, active: Backend, choices: Vec<Backend>) -> Self {
        let shows = shows.unwrap_or_else(|| choices[pick(&active, &choices)].clone());
        Self {
            shows,
            choices,
            active,
        }
    }

    /// The machine the picker's `index` switches to; `None` when it is the
    /// one shown already (or out of range).
    pub fn switch_to(&self, index: usize) -> Option<&Backend> {
        self.choices
            .get(index)
            .filter(|machine| !same(machine, &self.shows))
    }

    /// [`project_target`] for the shown machine.
    pub fn project_target(&self, folder: &Path) -> Result<String, String> {
        project_target(&self.shows, &self.active, folder)
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

    #[test]
    fn a_screen_says_which_machine_it_shows() {
        assert_eq!(screen_label(&Backend::Local), this_computer());
        assert_eq!(
            screen_label(&server("http://pc:8787", "ismael")),
            "ismael (server)"
        );
        assert_eq!(
            screen_label(&Backend::Wsl {
                distro: "Ubuntu".into()
            }),
            "WSL · Ubuntu"
        );
        assert_eq!(screen_label(&orb()), "orb --");
    }

    #[test]
    fn project_actions_act_on_the_open_workspace_only() {
        let pc = server("http://pc:8787", "ismael");
        let folder = Path::new("/home/ismael/gs-remote-test/");
        assert_eq!(
            project_target(&pc, &pc, folder).as_deref(),
            Ok("gs-remote-test on ismael")
        );
        assert_eq!(
            project_target(&Backend::Local, &Backend::Local, Path::new(r"C:\src\app")),
            Ok(format!("app on {}", this_computer()))
        );
        // Another machine than the workspace's: off, and why.
        let off = project_target(&Backend::Local, &pc, folder).unwrap_err();
        assert!(off.contains("ismael"), "{off}");
        assert!(project_target(&pc, &Backend::Local, folder).is_err());
    }

    #[test]
    fn a_screen_opens_on_the_workspace_machine_and_switches_among_the_choices() {
        let pc = server("http://pc:8787", "ismael");
        let folder = Path::new("/home/ismael/gs-remote-test");
        let all = choices(&pc, &Backend::Local, std::slice::from_ref(&pc));
        // Default: the active workspace's machine, project actions on.
        let shown = Shown::new(None, pc.clone(), all.clone());
        assert_eq!(shown.shows, pc);
        assert!(shown.project_target(folder).is_ok());
        // Picking the machine shown changes nothing; This Mac switches.
        assert_eq!(shown.switch_to(1), None);
        assert_eq!(shown.switch_to(0), Some(&Backend::Local));
        assert_eq!(shown.switch_to(9), None);
        // Shown on This Mac (the switch, or Configuration's "Settings for"):
        // project actions are off, they act on the server's workspace.
        let local = Shown::new(Some(Backend::Local), pc.clone(), all.clone());
        assert_eq!(local.shows, Backend::Local);
        assert!(local.project_target(folder).is_err());
        assert_eq!(local.switch_to(1), Some(&pc));
        // A workspace on a server no longer paired opens on this computer.
        let gone = server("http://old:8787", "old");
        assert_eq!(Shown::new(None, gone, all).shows, Backend::Local);
    }
}
