//! Execute typed CLI commands on the live GPUI workspace.

use gentle_core::workspace_commands::{self as protocol, Command, Output, Reply, WorkspaceInfo};

use super::*;

impl Workspace {
    pub(crate) fn workspace_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Reply {
        let message = match command {
            Command::Create { name, root } => {
                self.check_workspace_name(&name, None)?;
                let root = command_root(root)?;
                // A new workspace goes where new ones do.
                let machine = crate::features::backend::default_machine();
                self.create_workspace(Some(name.clone()), root, &machine, window, cx);
                window.activate_window();
                format!("Created workspace '{name}'")
            }
            Command::Switch { name } => {
                let index = self.workspace_index(&name)?;
                self.switch_workspace(index, window, cx);
                window.activate_window();
                format!("Switched to workspace '{}'", self.space().name)
            }
            Command::Rename { name, new_name } => {
                let index = self.workspace_index(&name)?;
                self.check_workspace_name(&new_name, Some(index))?;
                self.rename_workspace(index, new_name.clone(), cx);
                format!("Renamed workspace '{name}' to '{new_name}'")
            }
            Command::SetRoot { name, root } => {
                let index = self.workspace_index(&name)?;
                let root = command_root(root)?;
                if index == self.current {
                    self.set_root(root.clone(), cx);
                } else {
                    // It keeps its machine: the folder is one of its.
                    self.spaces[index].root = root.clone();
                    self.schedule_save(cx);
                    cx.notify();
                }
                format!("Workspace '{name}' root: {}", root.display())
            }
            Command::Remove { name, force } => {
                if !force {
                    return Err(
                        "Removal closes tabs and stops agents; use --force to confirm".into(),
                    );
                }
                if self.spaces.len() < 2 {
                    return Err("Cannot remove the last workspace".into());
                }
                let index = self.workspace_index(&name)?;
                self.delete_workspace(index, window, cx);
                format!("Removed workspace '{name}' (files and conversations kept)")
            }
            Command::List => return protocol::select(self.workspace_infos(), false),
            Command::Current => return protocol::select(self.workspace_infos(), true),
        };
        // Acknowledge only after the mutation and its snapshot have been saved.
        // The existing debounce may save the same (or newer) state later.
        workspaces::save(&self.snapshot(cx))
            .map_err(|error| format!("Change applied in Desktop, but saving failed: {error}"))?;
        Ok(Output::Message(message))
    }

    fn workspace_infos(&self) -> Vec<WorkspaceInfo> {
        self.spaces
            .iter()
            .enumerate()
            .map(|(index, space)| WorkspaceInfo {
                name: space.name.clone(),
                root: space.root.clone(),
                active: self.current == index,
                tabs: space.tabs.len(),
            })
            .collect()
    }

    fn workspace_index(&self, name: &str) -> Result<usize, String> {
        self.spaces
            .iter()
            .position(|space| space.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("Workspace '{name}' not found"))
    }

    fn check_workspace_name(&self, name: &str, except: Option<usize>) -> Result<(), String> {
        if !protocol::valid_name(name) {
            return Err(
                "Workspace name must be non-empty, trimmed and contain no control characters"
                    .into(),
            );
        }
        if self
            .spaces
            .iter()
            .enumerate()
            .any(|(index, space)| Some(index) != except && space.name.eq_ignore_ascii_case(name))
        {
            return Err(format!("Workspace '{name}' already exists"));
        }
        Ok(())
    }
}

/// `root` as a workspace folder: absolute, an existing folder (on the Gentle
/// Shell server agents run on, when they do: there every path is the
/// server's), with every link resolved. Blocking: a server answers over the
/// network.
fn command_root(root: PathBuf) -> Result<PathBuf, String> {
    use crate::features::backend::fs;
    if !root.is_absolute() || !fs::is_dir(&root) {
        return Err(format!(
            "Not an existing absolute directory: {}",
            root.display()
        ));
    }
    fs::canonicalize(&root).map_err(|error| format!("Cannot open {}: {error}", root.display()))
}
