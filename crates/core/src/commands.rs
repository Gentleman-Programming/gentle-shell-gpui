//! Gentle Shell slash commands whose UI only exists in the terminal: over RPC
//! `ctx.ui.custom()` returns nothing, so they either show nothing or warn
//! "requires TUI mode". The app routes the ones it has a native screen for and
//! hides the rest from the palette and the composer's `/` menu.

/// A native screen a slash command opens instead of reaching the backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// Usage, Subscription tab.
    Usage,
    /// Usage, Local stats tab.
    Stats,
    /// The session's Agents window, on its Agents tab.
    Agents,
    /// The session's Agents window, on its Jobs tab (background jobs).
    Jobs,
    Profiles,
    Palette,
    /// Files sidebar › Changes tab.
    Changes,
}

/// The native screen for a typed or picked slash command. `text` is what
/// follows the `/` (a leading `/` is tolerated): the name plus any arguments.
/// Arguments matter only for `gentle:profiles`, whose subcommands (`export`,
/// `import`, …) work over RPC and so are not routed.
pub fn route(text: &str) -> Option<Screen> {
    let text = text.trim().trim_start_matches('/');
    let (name, args) = match text.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (text, ""),
    };
    Some(match name {
        "gentle:usage" => Screen::Usage,
        "gentle:stats" => Screen::Stats,
        "gentle:agents" => Screen::Agents,
        "gentle:jobs" => Screen::Jobs,
        "gentle:profiles" if args.is_empty() => Screen::Profiles,
        "gentle:commands" => Screen::Palette,
        "gentle:changes" => Screen::Changes,
        _ => return None,
    })
}

/// Commands that do nothing useful over RPC and have no native screen yet,
/// plus `gentle-ui:event`, the app's channel to its generative UI extension:
/// hidden from the palette and the composer's `/` menu. `name` may carry a
/// leading `/`; arguments are ignored.
pub fn hidden(name: &str) -> bool {
    let name = name.trim().trim_start_matches('/');
    let name = name.split_whitespace().next().unwrap_or_default();
    matches!(
        name,
        "gentle:customize"
            | "gentle:guide"
            | "gentle:voice"
            | "gentle:music"
            | "gentle:panel"
            | "history"
            | "gentle-ui:event"
    )
}

/// One row of the composer's `/` menu and the palette's command list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    /// Without the leading `/`.
    pub name: String,
    pub description: String,
    /// A Pi built-in that shows its result or picker only in Gentle Shell's
    /// terminal (the session feed's `terminal` flag): listed, never sent.
    pub terminal: bool,
}

/// The commands to offer, in menu order: the built-ins the app runs itself,
/// then the Pi built-ins Gentle Shell's session feed lists (the app's own
/// win by name), then the backend's `get_commands` list without the hidden
/// ones, as before. `pi` is empty on a Shell without the feed.
pub fn menu(
    app: &[(&str, &str)],
    pi: &[SlashCommand],
    backend: &[(String, String)],
) -> Vec<SlashCommand> {
    let own = |name: &str| app.iter().any(|(own, _)| *own == name);
    app.iter()
        .map(|(name, description)| SlashCommand {
            name: (*name).to_string(),
            description: (*description).to_string(),
            terminal: false,
        })
        .chain(pi.iter().filter(|command| !own(&command.name)).cloned())
        .chain(
            backend
                .iter()
                .filter(|(name, _)| !hidden(name))
                .map(|(name, description)| SlashCommand {
                    name: name.clone(),
                    description: description.clone(),
                    terminal: false,
                }),
        )
        .collect()
}

/// What submitting a Pi built-in the feed lists does in the app, once the
/// app's own built-ins had their turn. Pi runs none of them over RPC: a
/// `prompt` would hand the text to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiBuiltin {
    /// `/new`: the app's new session (RPC `new_session`).
    NewSession,
    /// `/clone`: RPC `clone`, a copy of the session at its current position.
    Clone,
    /// Anything else: shown as unavailable here instead of being sent.
    Elsewhere { terminal: bool },
}

pub fn pi_builtin(command: &SlashCommand) -> PiBuiltin {
    match (command.name.as_str(), command.terminal) {
        ("new", false) => PiBuiltin::NewSession,
        ("clone", false) => PiBuiltin::Clone,
        (_, terminal) => PiBuiltin::Elsewhere { terminal },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pi(name: &str, terminal: bool) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: format!("{name} (Pi)"),
            terminal,
        }
    }

    #[test]
    fn the_menu_puts_the_apps_built_ins_first_and_wins_by_name() {
        let app = [("compact", "app compact"), ("copy", "app copy")];
        let feed = [pi("new", false), pi("compact", true), pi("model", true)];
        let backend = vec![
            ("gentle:doctor".to_string(), "Doctor".to_string()),
            ("history".to_string(), "hidden".to_string()),
        ];
        let menu = menu(&app, &feed, &backend);
        let rows: Vec<(&str, &str, bool)> = menu
            .iter()
            .map(|c| (c.name.as_str(), c.description.as_str(), c.terminal))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("compact", "app compact", false),
                ("copy", "app copy", false),
                ("new", "new (Pi)", false),
                ("model", "model (Pi)", true),
                ("gentle:doctor", "Doctor", false),
            ]
        );
    }

    #[test]
    fn without_the_feed_the_menu_is_the_apps_then_the_backends() {
        let backend = vec![("skill:x".to_string(), "X".to_string())];
        let names: Vec<String> = menu(&[("copy", "c")], &[], &backend)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["copy", "skill:x"]);
    }

    #[test]
    fn only_new_and_clone_run_from_the_app() {
        assert_eq!(pi_builtin(&pi("new", false)), PiBuiltin::NewSession);
        assert_eq!(pi_builtin(&pi("clone", false)), PiBuiltin::Clone);
        assert_eq!(
            pi_builtin(&pi("model", true)),
            PiBuiltin::Elsewhere { terminal: true }
        );
        // A terminal `new` (should the feed ever say so) is not run here.
        assert_eq!(
            pi_builtin(&pi("new", true)),
            PiBuiltin::Elsewhere { terminal: true }
        );
        assert_eq!(
            pi_builtin(&pi("future", false)),
            PiBuiltin::Elsewhere { terminal: false }
        );
    }

    #[test]
    fn routes_terminal_only_commands_to_native_screens() {
        assert_eq!(route("gentle:usage"), Some(Screen::Usage));
        assert_eq!(route("gentle:stats"), Some(Screen::Stats));
        assert_eq!(route("gentle:agents"), Some(Screen::Agents));
        assert_eq!(route("gentle:jobs"), Some(Screen::Jobs));
        assert_eq!(route("gentle:profiles"), Some(Screen::Profiles));
        assert_eq!(route("gentle:commands"), Some(Screen::Palette));
        assert_eq!(route("gentle:changes"), Some(Screen::Changes));
    }

    #[test]
    fn tolerates_leading_slash_and_whitespace() {
        assert_eq!(route("/gentle:usage"), Some(Screen::Usage));
        assert_eq!(route("  /gentle:changes  "), Some(Screen::Changes));
        assert_eq!(route("/gentle:profiles   "), Some(Screen::Profiles));
        assert!(hidden("/gentle:guide"));
        assert!(hidden(" history "));
    }

    #[test]
    fn profiles_with_arguments_reach_the_backend() {
        assert_eq!(route("gentle:profiles export work"), None);
        assert_eq!(route("/gentle:profiles clear"), None);
        assert_eq!(route("gentle:profiles\texport"), None);
    }

    #[test]
    fn other_routes_ignore_arguments() {
        assert_eq!(route("gentle:usage now"), Some(Screen::Usage));
        assert_eq!(route("gentle:jobs list"), Some(Screen::Jobs));
    }

    #[test]
    fn everything_else_is_not_routed() {
        for text in [
            "",
            "/",
            "gentle:doctor",
            "gentle:usages",
            "usage",
            "compact",
            "skill:x",
        ] {
            assert_eq!(route(text), None, "{text}");
        }
    }

    #[test]
    fn hides_dead_commands_only() {
        for name in [
            "gentle:customize",
            "gentle:guide",
            "gentle:voice",
            "gentle:music",
            "gentle:music next",
            "gentle:panel",
            "history",
            "gentle-ui:event",
            "/gentle-ui:event {\"id\":\"ui-1\"}",
        ] {
            assert!(hidden(name), "{name}");
        }
        for name in [
            "gentle:usage",
            "gentle:doctor",
            "gentle:history",
            "historyx",
            "gentle-ui",
            "",
        ] {
            assert!(!hidden(name), "{name}");
        }
    }

    #[test]
    fn routed_and_hidden_never_overlap() {
        for name in [
            "gentle:usage",
            "gentle:stats",
            "gentle:agents",
            "gentle:jobs",
            "gentle:profiles",
            "gentle:commands",
            "gentle:changes",
        ] {
            assert!(!hidden(name), "{name}");
        }
        for name in [
            "gentle:customize",
            "gentle:guide",
            "gentle:voice",
            "gentle:music",
            "gentle:panel",
            "history",
        ] {
            assert_eq!(route(name), None, "{name}");
        }
    }
}
