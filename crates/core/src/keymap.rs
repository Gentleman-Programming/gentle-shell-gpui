//! The user's keymap file (`<data>/gentle-shell-gpui/keymap.json`): parsing
//! and validation, without touching GPUI or the disk.
//!
//! The file is forgiving: `//` and `/* */` comments and a comma before a
//! closing bracket are allowed (JSONC), a bad entry is reported with its index
//! and the rest still apply; an unreadable file means the defaults plus one
//! error.

use serde_json::Value;

/// The leader prefix when the file doesn't set a valid one.
pub const DEFAULT_LEADER: &str = "ctrl-space";

/// One `bindings` entry that passed the syntax checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Position in the file's `bindings` array, for later error messages.
    pub index: usize,
    /// A keystroke or a space-separated sequence, e.g. `ctrl-space o`.
    pub keys: String,
    /// Registered action name (`gentle::OpenPalette`); `None` unbinds `keys`.
    pub action: Option<String>,
    /// Key context predicate, e.g. `GentleWorkspace`; `None` is everywhere.
    pub context: Option<String>,
}

/// Something wrong in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// The `bindings` entry it is about; `None` for the file or the leader.
    pub index: Option<usize>,
    pub message: String,
}

impl Issue {
    /// One line for the user, e.g. `keymap.json bindings[2]: …`.
    pub fn describe(&self) -> String {
        match self.index {
            Some(i) => format!("keymap.json bindings[{i}]: {}", self.message),
            None => format!("keymap.json: {}", self.message),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    pub leader: String,
    pub bindings: Vec<Binding>,
    pub errors: Vec<Issue>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap {
            leader: DEFAULT_LEADER.into(),
            bindings: Vec::new(),
            errors: Vec::new(),
        }
    }
}

const MODIFIERS: [&str; 7] = ["cmd", "ctrl", "alt", "shift", "fn", "function", "secondary"];

/// Parse the file's text. Never fails: problems land in `errors`.
pub fn parse(text: &str) -> Keymap {
    let mut keymap = Keymap::default();
    if text.trim().is_empty() {
        return keymap;
    }
    let root: Value = match serde_json::from_str(&strip_jsonc(text)) {
        Ok(v) => v,
        Err(e) => {
            keymap
                .errors
                .push(file_issue(format!("not valid JSON ({e})")));
            return keymap;
        }
    };
    let Some(root) = root.as_object() else {
        keymap.errors.push(file_issue(
            "expected an object with \"leader\" and \"bindings\"".into(),
        ));
        return keymap;
    };
    if let Some(leader) = root.get("leader") {
        match leader_ok(leader) {
            Ok(l) => keymap.leader = l,
            Err(e) => keymap
                .errors
                .push(file_issue(format!("leader: {e}; using {DEFAULT_LEADER}"))),
        }
    }
    match root.get("bindings") {
        None | Some(Value::Null) => {}
        Some(Value::Array(entries)) => {
            for (index, entry) in entries.iter().enumerate() {
                match binding(index, entry) {
                    Ok(b) => keymap.bindings.push(b),
                    Err(message) => keymap.errors.push(Issue {
                        index: Some(index),
                        message,
                    }),
                }
            }
        }
        Some(_) => keymap
            .errors
            .push(file_issue("bindings: expected an array".into())),
    }
    keymap
}

fn file_issue(message: String) -> Issue {
    Issue {
        index: None,
        message,
    }
}

/// The file's text as plain JSON: `//` line comments and `/* */` block
/// comments removed, and a comma before a closing `]` or `}` dropped. Both are
/// replaced with spaces (newlines kept), so the parser's line and column still
/// point at the original place.
fn strip_jsonc(text: &str) -> String {
    drop_trailing_commas(&strip_comments(text))
}

fn strip_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let byte = bytes[i];
        if in_string {
            out.push(byte);
            match byte {
                // A backslash escapes the next byte, `"` included.
                b'\\' => {
                    if let Some(next) = bytes.get(i + 1) {
                        out.push(*next);
                    }
                    i += 2;
                    continue;
                }
                b'"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match byte {
            b'"' => {
                in_string = true;
                out.push(byte);
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    out.push(b' ');
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                out.extend_from_slice(b"  ");
                while i < bytes.len() {
                    if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        i += 2;
                        out.extend_from_slice(b"  ");
                        break;
                    }
                    out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
            _ => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn drop_trailing_commas(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let byte = bytes[i];
        if in_string {
            out.push(byte);
            match byte {
                b'\\' => {
                    if let Some(next) = bytes.get(i + 1) {
                        out.push(*next);
                    }
                    i += 2;
                    continue;
                }
                b'"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match byte {
            b'"' => {
                in_string = true;
                out.push(byte);
            }
            b',' => {
                let mut next = i + 1;
                while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                    next += 1;
                }
                if matches!(bytes.get(next), Some(b']') | Some(b'}')) {
                    out.push(b' ');
                } else {
                    out.push(byte);
                }
            }
            _ => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn leader_ok(value: &Value) -> Result<String, String> {
    let keys = value.as_str().ok_or("expected a string")?;
    validate_keys(keys)?;
    if keys.split_whitespace().count() != 1 {
        return Err("must be a single keystroke".into());
    }
    let keys = keys.trim();
    // A bare key as the prefix would swallow typing everywhere.
    if !MODIFIERS.iter().any(|m| keys.starts_with(&format!("{m}-"))) {
        return Err("needs a modifier (ctrl, alt, cmd…)".into());
    }
    Ok(keys.into())
}

fn binding(index: usize, entry: &Value) -> Result<Binding, String> {
    let entry = entry.as_object().ok_or("expected an object")?;
    let keys = entry
        .get("keys")
        .and_then(Value::as_str)
        .ok_or("\"keys\" must be a string")?;
    validate_keys(keys).map_err(|e| format!("keys {keys:?}: {e}"))?;
    let action = match entry.get("action") {
        Some(Value::Null) => None,
        Some(Value::String(name)) if !name.trim().is_empty() => Some(name.trim().to_string()),
        _ => return Err("\"action\" must be an action name or null".into()),
    };
    let context = match entry.get("context") {
        None | Some(Value::Null) => None,
        Some(Value::String(c)) if !c.trim().is_empty() => Some(c.trim().to_string()),
        Some(_) => return Err("\"context\" must be a string".into()),
    };
    Ok(Binding {
        index,
        keys: keys.split_whitespace().collect::<Vec<_>>().join(" "),
        action,
        context,
    })
}

/// Check a keystroke or space-separated sequence; the error says what's wrong.
pub fn validate_keys(keys: &str) -> Result<(), String> {
    let mut any = false;
    for stroke in keys.split_whitespace() {
        any = true;
        validate_keystroke(stroke)?;
    }
    if any { Ok(()) } else { Err("empty".into()) }
}

/// `modifier-…-key`; the key may be `-` itself (`cmd--`).
fn validate_keystroke(stroke: &str) -> Result<(), String> {
    let mut rest = stroke;
    while let Some((head, tail)) = rest.split_once('-') {
        if tail.is_empty() || !MODIFIERS.contains(&head) {
            break;
        }
        rest = tail;
    }
    if rest.is_empty() || (rest.contains('-') && rest != "-") {
        return Err(format!("{stroke:?} is not modifiers followed by one key"));
    }
    if MODIFIERS.contains(&rest) {
        return Err(format!("{stroke:?} has no key after its modifiers"));
    }
    Ok(())
}

/// The file written by "Open keymap file" when none exists: the leader, an
/// empty `bindings` array and a commented reference — every labelled action
/// with its label, its default keys on this platform and its context.
/// `defaults` is the keymap the app binds with no user file, handed over with
/// its leader keys already spelled like `leader` (see `shared/keymap.rs`).
pub fn template(leader: &str, defaults: &[crate::shortcuts::Binding]) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');
    out.push_str("{\n");
    out.push_str("  // The leader: one keystroke with a modifier. Press it, then one key.\n");
    out.push_str(
        "  // Examples: \"ctrl-space\" (default), \"ctrl-a\", \"ctrl-b\" (tmux), \"cmd-k\".\n",
    );
    out.push_str("  // Every leader binding in the reference below follows this value.\n");
    out.push_str(&format!("  \"leader\": \"{leader}\",\n\n"));
    out.push_str(BINDINGS_HELP);
    out.push_str(&examples(leader, defaults));
    out.push_str("  \"bindings\": []\n}\n\n");
    out.push_str(&reference(leader, defaults));
    out.push_str(CONTEXTS);
    out
}

const HEADER: &str = "\
// Gentle Shell keymap.
//
// Read at startup and again when you run \"Reload keymap\" (Configuration →
// Appearance → Keyboard, or the command palette). Nothing here is required:
// without this file the defaults apply, and the reference at the end of the
// file lists them.
//
// Lines and blocks starting with // and /* */ are comments, and a comma before
// a closing ] or } is allowed. Delete the notes you don't want.
";

const BINDINGS_HELP: &str = concat!(
    "  // Your bindings. An entry wins over a default that uses the same keys.\n",
    "  //   \"keys\":    one keystroke or a space-separated sequence (\"cmd-shift-p\",\n",
    "  //              \"ctrl-space o\"). Modifiers are cmd, ctrl, alt and shift,\n",
    "  //              then the key. The leader is written literally: with\n",
    "  //              \"leader\": \"ctrl-a\", write \"ctrl-a o\".\n",
    "  //   \"action\":  one of the names in the reference below, or null to unbind\n",
    "  //              the keys.\n",
    "  //   \"context\": optional. Leave it out to change a default: a default with\n",
    "  //              no context is matched deepest, so an entry with one on the\n",
    "  //              same keys never wins.\n",
);

/// Four examples written with the real leader and the platform's own composer
/// chord: unbind, add a leader key, a Files key, and a terminal rest.
fn examples(leader: &str, defaults: &[crate::shortcuts::Binding]) -> String {
    let composer = defaults
        .iter()
        .find(|b| b.action == "gentle::FocusComposer")
        .and_then(|b| b.keys.first().cloned())
        .unwrap_or_else(|| "cmd-l".into());
    let mut text = String::from("  //\n  // Examples — delete the leading // of one to start:\n");
    text.push_str(&format!(
        "  //   {{ \"keys\": \"{composer}\", \"action\": null }},\n"
    ));
    text.push_str(&format!(
        "  //   {{ \"keys\": \"{leader} e\", \"action\": \"gentle::FocusFiles\" }},\n"
    ));
    text.push_str(
        "  //   { \"keys\": \"shift-enter\", \"action\": \"file_tree::OpenToSide\", \"context\": \"FileTree\" },\n",
    );
    text.push_str(
        "  //   { \"keys\": \"ctrl-shift-space\", \"action\": \"gentle::FocusComposer\", \"context\": \"Terminal\" }\n",
    );
    text.push_str(
        "  //\n  // In order: unbind a default chord, add a leader key, add a Files key,\n  // and give a terminal pane its own chord (plain ctrl-letter chords there\n  // belong to the shell).\n",
    );
    text
}

/// The commented reference: the cheat sheet's sections, one row per labelled
/// action with its default keys here, and the labelled actions that have none.
fn reference(leader: &str, defaults: &[crate::shortcuts::Binding]) -> String {
    let sections = crate::shortcuts::reference(defaults, leader);
    let unbound = crate::shortcuts::unbound(defaults);
    let label_width = sections
        .iter()
        .flat_map(|section| section.rows.iter())
        .map(|row| row.label.len())
        .chain(unbound.iter().map(|(_, label)| label.len()))
        .max()
        .unwrap_or(0);
    let keys_width = sections
        .iter()
        .flat_map(|section| section.rows.iter())
        .map(|row| row.keys.join(", ").len())
        .max()
        .unwrap_or(0)
        .max(1);

    let mut text = String::from(
        "// Reference: every action you can bind, grouped as in the keyboard cheat\n\
         // sheet (⌘/ or leader ?), with its label, its default keys on this\n\
         // platform and the name \"action\" takes. A row with a context in\n\
         // parentheses only applies there.\n",
    );
    for section in &sections {
        text.push_str(&format!("//\n// {}\n", section.section.title()));
        for row in &section.rows {
            let keys = row.keys.join(", ");
            let context = match &row.context {
                Some(context) => format!("  ({context})"),
                None => String::new(),
            };
            text.push_str(&format!(
                "//   {label:<label_width$}  {keys:<keys_width$}  {action}{context}\n",
                label = row.label,
                action = row.action,
            ));
        }
    }
    if !unbound.is_empty() {
        text.push_str("//\n// No default keys — add one of these in \"bindings\":\n");
        for (action, label) in &unbound {
            text.push_str(&format!(
                "//   {label:<label_width$}  {keys:<keys_width$}  {action}\n",
                keys = "—",
            ));
        }
    }
    text
}

const CONTEXTS: &str = "\
//
// Contexts. A binding without one applies everywhere; with one it applies only
// while the context matches. && and ! combine them.
//   GentleWorkspace   the window; add pane_mode to match pane mode only
//   GentleSession     an agent pane (the conversation)
//   Terminal          a terminal pane
//   FileTree          the Files tree (add Input inside its text fields)
//   GentleChanges     the Git changes list (add Input the same way)
//   GentleFileView    the file and diff viewer
//   GentleCanvas      the canvas
//   GentleTabList     the tab list in the right sidebar
//   Input             a focused text field
//   vim               on while the \"Vim keys\" setting is on
";

/// The leader of a file that is still exactly the template older versions
/// wrote — plain JSON, `{"leader": …, "bindings": []}` and no other keys —
/// which the app upgrades to the documented one. `None` for anything else, so
/// an edited or commented file is never rewritten.
pub fn bare_template_leader(text: &str) -> Option<String> {
    let root: Value = serde_json::from_str(text).ok()?;
    let root = root.as_object()?;
    if root.len() != 2 {
        return None;
    }
    let leader = root.get("leader")?.as_str()?.to_string();
    match root.get("bindings")? {
        Value::Array(entries) if entries.is_empty() => Some(leader),
        _ => None,
    }
}

/// The warning for an entry that names a context but whose keys the app binds
/// without one by default: GPUI ranks a context-less binding deepest, so the
/// entry would never win. `globals` are the default bindings that have no
/// context, one binding string each. `None` when the entry is fine.
pub fn contextual_override_warning(entry: &Binding, globals: &[String]) -> Option<Issue> {
    entry.context.as_ref()?;
    globals
        .iter()
        .any(|g| crate::shortcuts::same_keys(g, &entry.keys))
        .then(|| Issue {
            index: Some(entry.index),
            message: format!(
                "\"{}\" is bound globally by default; omit \"context\" to change it",
                entry.keys
            ),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(index: usize, keys: &str, action: Option<&str>, context: Option<&str>) -> Binding {
        Binding {
            index,
            keys: keys.into(),
            action: action.map(Into::into),
            context: context.map(Into::into),
        }
    }

    #[test]
    fn no_file_content_means_defaults() {
        assert_eq!(parse(""), Keymap::default());
        assert_eq!(Keymap::default().leader, "ctrl-space");
    }

    #[test]
    fn valid_leader_overrides_the_default() {
        let k = parse(r#"{"leader": "ctrl-a"}"#);
        assert_eq!(k.leader, "ctrl-a");
        assert!(k.errors.is_empty());
    }

    #[test]
    fn invalid_leader_falls_back_with_an_error() {
        for bad in [r#""ctrl-a b""#, r#""ctrl-""#, r#""a""#, "3"] {
            let k = parse(&format!(r#"{{"leader": {bad}}}"#));
            assert_eq!(k.leader, DEFAULT_LEADER, "{bad}");
            assert_eq!(k.errors.len(), 1, "{bad}");
            assert_eq!(k.errors[0].index, None);
            assert!(k.errors[0].message.contains("leader"), "{bad}");
        }
    }

    #[test]
    fn valid_binding_keeps_keys_action_and_context() {
        let k = parse(
            r#"{"bindings": [
              {"keys": "cmd-shift-p", "action": "gentle::OpenPalette"},
              {"keys": "ctrl-space o", "action": "gentle::FocusFiles", "context": "GentleWorkspace"}
            ]}"#,
        );
        assert!(k.errors.is_empty());
        assert_eq!(
            k.bindings,
            vec![
                b(0, "cmd-shift-p", Some("gentle::OpenPalette"), None),
                b(
                    1,
                    "ctrl-space o",
                    Some("gentle::FocusFiles"),
                    Some("GentleWorkspace")
                ),
            ]
        );
    }

    #[test]
    fn null_action_unbinds() {
        let k = parse(r#"{"bindings": [{"keys": "cmd-l", "action": null}]}"#);
        assert!(k.errors.is_empty());
        assert_eq!(k.bindings, vec![b(0, "cmd-l", None, None)]);
    }

    #[test]
    fn invalid_entry_is_reported_with_its_index_and_the_rest_kept() {
        let k = parse(
            r#"{"bindings": [
              {"keys": "cmd-p", "action": "gentle::OpenPalette"},
              {"keys": "ctrl-", "action": "gentle::OpenPalette"},
              {"action": "gentle::OpenPalette"},
              {"keys": "cmd-k", "action": 5},
              "nope",
              {"keys": "cmd-j", "action": "gentle::NewTab"}
            ]}"#,
        );
        let indexes: Vec<_> = k.errors.iter().map(|e| e.index).collect();
        assert_eq!(indexes, vec![Some(1), Some(2), Some(3), Some(4)]);
        let kept: Vec<_> = k.bindings.iter().map(|b| b.index).collect();
        assert_eq!(kept, vec![0, 5]);
        assert!(
            k.errors[0]
                .describe()
                .starts_with("keymap.json bindings[1]:")
        );
    }

    #[test]
    fn malformed_json_gives_defaults_and_one_error() {
        let k = parse("{ not json");
        assert_eq!(k.leader, DEFAULT_LEADER);
        assert!(k.bindings.is_empty());
        assert_eq!(k.errors.len(), 1);
        assert_eq!(k.errors[0].index, None);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let k = parse(
            r#"{"version": 2, "bindings": [
              {"keys": "cmd-j", "action": "gentle::NewTab", "note": "x"}
            ]}"#,
        );
        assert!(k.errors.is_empty());
        assert_eq!(k.bindings.len(), 1);
    }

    #[test]
    fn keystroke_syntax() {
        for ok in [
            "a",
            "cmd-shift-p",
            "ctrl-space h",
            "cmd--",
            "shift-;",
            "f2",
            "alt-enter",
        ] {
            assert_eq!(validate_keys(ok), Ok(()), "{ok}");
        }
        for bad in ["", "   ", "ctrl-", "ctrl-shift", "cmd-p-q", "ctrl- p"] {
            assert!(validate_keys(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn template_is_a_valid_file_with_the_leader() {
        let k = parse(&template("ctrl-a", &[]));
        assert_eq!(k.leader, "ctrl-a");
        assert!(k.errors.is_empty() && k.bindings.is_empty());
    }

    fn defaults() -> Vec<crate::shortcuts::Binding> {
        use crate::shortcuts::Binding as Live;
        vec![
            Live {
                keys: vec!["cmd-t".into()],
                action: "gentle::NewTab".into(),
                context: None,
            },
            Live {
                keys: vec!["ctrl-space".into(), "c".into()],
                action: "gentle::NewTab".into(),
                context: None,
            },
            Live {
                keys: vec!["cmd-shift-b".into()],
                action: "gentle::FocusFiles".into(),
                context: None,
            },
            Live {
                keys: vec!["j".into()],
                action: "file_tree::SelectNext".into(),
                context: Some("FileTree && vim && !Input".into()),
            },
        ]
    }

    #[test]
    fn comments_and_trailing_commas_are_tolerated() {
        let k = parse(
            r#"// my keymap
            {
              /* the prefix */
              "leader": "ctrl-a", // one keystroke
              "bindings": [
                {"keys": "cmd-p", "action": "gentle::OpenPalette"}, // palette
                {"keys": "ctrl-a h", "action": "gentle::FocusPaneLeft"},
              ],
            }
            // the end
            "#,
        );
        assert!(k.errors.is_empty(), "{:?}", k.errors);
        assert_eq!(k.leader, "ctrl-a");
        assert_eq!(
            k.bindings,
            vec![
                b(0, "cmd-p", Some("gentle::OpenPalette"), None),
                b(1, "ctrl-a h", Some("gentle::FocusPaneLeft"), None),
            ]
        );
    }

    #[test]
    fn slashes_and_commas_inside_strings_are_not_comments() {
        let k = parse(
            r#"{"leader": "ctrl-/", "bindings": [
              {"keys": "ctrl-/", "action": "gentle::OpenPalette", "url": "https://x//y/*z*/"},
              {"keys": "cmd-space", "action": null, "note": "a,],} and \"//\""},
            ],}"#,
        );
        assert!(k.errors.is_empty(), "{:?}", k.errors);
        assert_eq!(k.leader, "ctrl-/");
        assert_eq!(
            k.bindings,
            vec![
                b(0, "ctrl-/", Some("gentle::OpenPalette"), None),
                b(1, "cmd-space", None, None),
            ]
        );
    }

    #[test]
    fn comments_do_not_move_the_reported_binding_index() {
        let k = parse(
            r#"{
              "bindings": [
                // the first one is fine
                {"keys": "cmd-p", "action": "gentle::OpenPalette"},
                /* this one is not */
                {"keys": "ctrl-", "action": "gentle::OpenPalette"},
                {"keys": "cmd-j", "action": "gentle::NewTab"}, // and this one
              ],
            }"#,
        );
        assert_eq!(k.errors.len(), 1, "{:?}", k.errors);
        assert_eq!(k.errors[0].index, Some(1));
        assert!(
            k.errors[0]
                .describe()
                .starts_with("keymap.json bindings[1]:"),
            "{}",
            k.errors[0].describe()
        );
        let kept: Vec<_> = k.bindings.iter().map(|b| b.index).collect();
        assert_eq!(kept, vec![0, 2]);
    }

    #[test]
    fn an_unterminated_block_comment_is_one_file_error() {
        let k = parse("/* never closed");
        assert_eq!(k.errors.len(), 1);
        assert_eq!(k.errors[0].index, None);
        assert_eq!(k.leader, DEFAULT_LEADER);
    }

    #[test]
    fn bare_template_leader_matches_only_the_old_empty_file() {
        assert_eq!(
            bare_template_leader("{\n  \"leader\": \"ctrl-a\",\n  \"bindings\": []\n}\n"),
            Some("ctrl-a".into())
        );
        assert_eq!(
            bare_template_leader(r#"{"bindings" : [ ], "leader" : "ctrl-b"}"#),
            Some("ctrl-b".into())
        );
        for other in [
            "",
            "not json",
            r#"{"leader": "ctrl-a", "bindings": [{"keys": "cmd-p", "action": null}]}"#,
            r#"{"leader": "ctrl-a", "bindings": [], "version": 1}"#,
            r#"{"bindings": []}"#,
            r#"{"leader": "ctrl-a"}"#,
            // Plain JSON only: a commented file is someone's edit.
            "// mine\n{\"leader\": \"ctrl-a\", \"bindings\": []}",
            r#"{"leader": 3, "bindings": []}"#,
        ] {
            assert_eq!(bare_template_leader(other), None, "{other:?}");
        }
    }

    #[test]
    fn template_documents_the_leader_and_the_whole_reference() {
        let text = template("ctrl-space", &defaults());
        let k = parse(&text);
        assert!(k.errors.is_empty(), "{:?}", k.errors);
        assert_eq!(k.leader, "ctrl-space");
        assert!(k.bindings.is_empty());

        // It documents itself: comments, the reload path, the leader examples.
        assert!(text.starts_with("// "), "{text}");
        assert!(text.contains("Reload keymap"));
        assert!(text.contains("\"ctrl-a\"") && text.contains("\"ctrl-b\""));
        assert!(text.contains("\"bindings\": []"));

        // Every labelled action is named, with its label and this platform's
        // default keys and its context.
        for (action, label) in crate::shortcuts::LABELS {
            assert!(text.contains(action), "{action} missing");
            assert!(text.contains(label), "{label} missing");
        }
        for keys in ["cmd-t", "ctrl-space c", "cmd-shift-b", "j"] {
            assert!(text.contains(keys), "{keys} missing");
        }
        assert!(text.contains("(FileTree && vim && !Input)"));

        // Grouped like the cheat sheet, with the contexts listed once.
        for title in ["Direct", "Leader", "Files & Git changes"] {
            assert!(text.contains(title), "{title} missing");
        }
        assert!(!text.contains("\n// Composer & session\n"));
        for context in [
            "GentleWorkspace",
            "GentleSession",
            "FileTree",
            "GentleChanges",
            "GentleFileView",
            "GentleCanvas",
            "GentleTabList",
            "Input",
            "vim",
        ] {
            assert!(text.contains(context), "{context} missing");
        }
    }

    #[test]
    fn template_follows_the_leader_and_is_deterministic() {
        // The app hands the defaults over with the leader's own spelling.
        let mut defaults = defaults();
        defaults[1].keys = vec!["ctrl-a".into(), "c".into()];
        let text = template("ctrl-a", &defaults);
        assert_eq!(text, template("ctrl-a", &defaults));
        assert!(text.contains("\"leader\": \"ctrl-a\""));
        assert!(text.contains("ctrl-a c"), "{text}");
        // The example follows the file's leader too.
        assert!(text.contains("ctrl-a e"));
        assert!(!text.contains("ctrl-space c"));
    }

    #[test]
    fn template_without_defaults_still_names_every_action() {
        let text = template("ctrl-space", &[]);
        let k = parse(&text);
        assert!(k.errors.is_empty(), "{:?}", k.errors);
        for (action, _) in crate::shortcuts::LABELS {
            assert!(text.contains(action), "{action} missing");
        }
    }

    #[test]
    fn contextual_entry_on_a_global_default_warns() {
        let globals = vec!["ctrl-space h".to_string(), "cmd-t".to_string()];
        let entry = b(3, "ctrl-space h", None, Some("GentleWorkspace"));
        let issue = contextual_override_warning(&entry, &globals).unwrap();
        assert_eq!(
            issue.describe(),
            "keymap.json bindings[3]: \"ctrl-space h\" is bound globally by default; omit \"context\" to change it"
        );
        // Spelled differently, same keys.
        let entry = b(0, "ctrl-space  h", Some("x"), Some("GentleWorkspace"));
        assert!(contextual_override_warning(&entry, &globals).is_some());
    }

    #[test]
    fn no_warning_without_context_or_without_a_global_default() {
        let globals = vec!["ctrl-space h".to_string()];
        assert!(contextual_override_warning(&b(0, "ctrl-space h", None, None), &globals).is_none());
        let other = b(0, "ctrl-space j", None, Some("GentleWorkspace"));
        assert!(contextual_override_warning(&other, &globals).is_none());
    }
}
