//! Command line: `gentle-shell-gpui [FOLDER] [-p|--prompt TEXT] [--open-url URL]
//! [--diagnostics]`.

use std::path::{Path, PathBuf};

/// `gentle-shell-gpui [FOLDER] [-p|--prompt TEXT]`
pub struct Cli {
    pub folder: Option<PathBuf>,
    pub prompt: Option<String>,
    /// `--open-url URL`: show a page in the running app's browser.
    pub url: Option<String>,
}

pub fn parse_cli() -> Cli {
    let mut cli = Cli {
        folder: None,
        prompt: None,
        url: None,
    };
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "-p" || arg == "--prompt" {
            cli.prompt = args.next().map(|p| p.to_string_lossy().into_owned());
        } else if arg == "--open-url" || arg == "--url" {
            cli.url = Some(
                args.next()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
        } else if arg == crate::shared::diagnostics::FLAG {
            // Read by `diagnostics::enabled`.
        } else if cli.folder.is_none() {
            cli.folder = Some(PathBuf::from(arg));
        }
    }
    cli
}

/// The folder the first tab opens: the CLI argument, else the folder the app
/// was launched in, else home (GUI launches start in `/`).
pub fn default_cwd() -> PathBuf {
    parse_cli()
        .folder
        .filter(|p| p.is_dir())
        .and_then(|p| dunce::canonicalize(p).ok())
        .or_else(|| launch_folder(launch_dir(), pi_rpc::managed_install_root().as_deref()))
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

static LAUNCH_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Remember the folder the app was launched in, before startup moves the
/// app out of it (`main`, on Windows).
pub fn remember_launch_dir() {
    LAUNCH_DIR.get_or_init(|| std::env::current_dir().ok());
}

/// The folder the app was launched in: the one [`remember_launch_dir`] saw,
/// else the current one.
fn launch_dir() -> Option<PathBuf> {
    match LAUNCH_DIR.get() {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().ok(),
    }
}

/// `launch` as the first tab's folder: not the root (where GUI launches
/// start) nor a folder inside gentle-ai's managed install (`managed_root`),
/// where a shortcut may start the app and where a tab would hold the
/// install open.
fn launch_folder(launch: Option<PathBuf>, managed_root: Option<&Path>) -> Option<PathBuf> {
    launch.filter(|p| p.parent().is_some() && !managed_root.is_some_and(|root| within(p, root)))
}

/// `path` is `root` or inside it, ignoring case (Windows paths).
fn within(path: &Path, root: &Path) -> bool {
    let lower = |path: &Path| -> Vec<String> {
        path.components()
            .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    lower(path).starts_with(&lower(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal launch without a folder argument opens the terminal's
    /// folder, as before the app moved off its launch folder at startup.
    #[test]
    fn a_terminal_launch_opens_the_terminals_folder() {
        let terminal = std::env::temp_dir().join("project");
        assert_eq!(
            launch_folder(Some(terminal.clone()), None),
            Some(terminal.clone())
        );
        let root = std::env::temp_dir().join("gentle-shell").join("install");
        assert_eq!(
            launch_folder(Some(terminal.clone()), Some(&root)),
            Some(terminal)
        );
    }

    #[test]
    fn a_gui_launch_at_the_root_opens_home() {
        let root = std::env::temp_dir()
            .ancestors()
            .last()
            .unwrap()
            .to_path_buf();
        assert_eq!(launch_folder(Some(root), None), None);
    }

    /// A shortcut that starts the app inside gentle-ai's managed install
    /// doesn't make that install the first tab's folder.
    #[test]
    fn a_launch_inside_the_managed_install_opens_home() {
        let root = std::env::temp_dir().join("gentle-shell").join("install");
        let programs = root
            .join("home")
            .join("AppData")
            .join("Local")
            .join("Programs")
            .join("Gentle Shell");
        assert_eq!(launch_folder(Some(programs), Some(&root)), None);
        assert_eq!(launch_folder(Some(root.clone()), Some(&root)), None);
    }
}
