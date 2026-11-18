//! What generative UI keeps in the app's data folder
//! (`<data>/gentle-shell-gpui/`): saved templates, one JSON file each in
//! `genui-templates/` (format in `gentle_core::genui::template`), and the
//! surfaces pinned per conversation in `genui-pins.json`
//! (`{"sessions": {"<session file>": ["id", …]}}`).

use std::io;
use std::path::{Path, PathBuf};

use gentle_core::genui::template::{self, Template};
use gpui_kit::{App, Global};
use serde_json::{Map, Value, json};

/// Where the files live instead of the data folder (tests).
pub struct StoreDir(pub PathBuf);

impl Global for StoreDir {}

/// The app's data folder, or [`StoreDir`] when set.
pub fn dir(cx: &App) -> Option<PathBuf> {
    match cx.try_global::<StoreDir>() {
        Some(dir) => Some(dir.0.clone()),
        None => crate::shared::data_dir::app(),
    }
}

fn templates_dir(dir: &Path) -> PathBuf {
    dir.join("genui-templates")
}

/// Write `text` to `path` through a temporary file, so a reader never sees
/// half of it.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The saved templates by name, with their files; unreadable files are skipped.
pub fn templates(dir: &Path) -> Vec<(PathBuf, Template)> {
    let Ok(entries) = std::fs::read_dir(templates_dir(dir)) else {
        return Vec::new();
    };
    let mut list: Vec<(PathBuf, Template)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            Some((p, Template::parse(&text).ok()?))
        })
        .collect();
    list.sort_by_key(|(_, t)| t.name.to_lowercase());
    list
}

/// A name no other template has, whose file stem is free too.
fn free_name(dir: &Path, name: &str, except: Option<&Path>) -> String {
    let others: Vec<Template> = templates(dir)
        .into_iter()
        .filter(|(p, _)| Some(p.as_path()) != except)
        .map(|(_, t)| t)
        .collect();
    let file_taken = |stem: &str| {
        let file = templates_dir(dir).join(format!("{stem}.json"));
        file.exists() && Some(file.as_path()) != except
    };
    template::free_name(name, |candidate| {
        others
            .iter()
            .any(|t| t.name.eq_ignore_ascii_case(candidate))
            || file_taken(candidate)
    })
}

/// Save `template` under a free name (`Name (2)`… when taken); its file.
pub fn save_template(dir: &Path, mut template: Template) -> io::Result<PathBuf> {
    template.name = free_name(dir, &template.name, None);
    let path = templates_dir(dir).join(format!("{}.json", template::stem(&template.name)));
    write_atomic(&path, &template.to_json())?;
    Ok(path)
}

/// Give the template in `path` a new name (made free); its file now.
pub fn rename_template(dir: &Path, path: &Path, name: &str) -> io::Result<PathBuf> {
    let text = std::fs::read_to_string(path)?;
    let mut template =
        Template::parse(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    template.name = free_name(dir, name, Some(path));
    let target = templates_dir(dir).join(format!("{}.json", template::stem(&template.name)));
    write_atomic(&target, &template.to_json())?;
    if target != path {
        std::fs::remove_file(path)?;
    }
    Ok(target)
}

pub fn delete_template(path: &Path) -> io::Result<()> {
    std::fs::remove_file(path)
}

fn pins_file(dir: &Path) -> PathBuf {
    dir.join("genui-pins.json")
}

fn read_pins(dir: &Path) -> Map<String, Value> {
    std::fs::read_to_string(pins_file(dir))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.get("sessions").and_then(Value::as_object).cloned())
        .unwrap_or_default()
}

/// The ids pinned in the conversation saved at `session`, in pin order.
pub fn pins(dir: &Path, session: &Path) -> Vec<String> {
    read_pins(dir)
        .get(&session.to_string_lossy().into_owned())
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Remember `ids` as `session`'s pins (none: forget it). Conversations whose
/// file is gone are dropped on the way.
pub fn set_pins(dir: &Path, session: &Path, ids: &[String]) -> io::Result<()> {
    let mut sessions = read_pins(dir);
    let key = session.to_string_lossy().into_owned();
    if ids.is_empty() {
        sessions.remove(&key);
    } else {
        sessions.insert(key.clone(), json!(ids));
    }
    sessions.retain(|file, _| *file == key || Path::new(file).exists());
    let text = serde_json::to_string_pretty(&json!({ "sessions": sessions })).unwrap_or_default();
    write_atomic(&pins_file(dir), &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("genui-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn template(name: &str) -> Template {
        Template {
            name: name.into(),
            kind: "table".into(),
            title: "Flaky".into(),
            spec: json!({"columns": ["name"], "rows": []}),
            saved_at: 1,
        }
    }

    #[test]
    fn templates_save_list_rename_and_delete() {
        let dir = temp("templates");
        let first = save_template(&dir, template("Flaky tests")).unwrap();
        let second = save_template(&dir, template("Flaky tests")).unwrap();
        assert_eq!(first.file_name().unwrap(), "flaky-tests.json");
        assert_eq!(second.file_name().unwrap(), "flaky-tests-2.json");
        let names: Vec<String> = templates(&dir).into_iter().map(|(_, t)| t.name).collect();
        assert_eq!(names, ["Flaky tests", "Flaky tests (2)"]);
        let renamed = rename_template(&dir, &second, "Release plan").unwrap();
        assert_eq!(renamed.file_name().unwrap(), "release-plan.json");
        assert!(!second.exists());
        delete_template(&first).unwrap();
        let names: Vec<String> = templates(&dir).into_iter().map(|(_, t)| t.name).collect();
        assert_eq!(names, ["Release plan"]);
        std::fs::write(templates_dir(&dir).join("junk.json"), "{").unwrap();
        assert_eq!(templates(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pins_are_kept_per_session() {
        let dir = temp("pins");
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        std::fs::write(&a, "").unwrap();
        set_pins(&dir, &a, &["arch".into(), "plan".into()]).unwrap();
        set_pins(&dir, &b, &["x".into()]).unwrap();
        assert_eq!(pins(&dir, &a), ["arch", "plan"]);
        assert_eq!(pins(&dir, &b), ["x"]);
        // b's file doesn't exist: dropped the next time another is saved.
        set_pins(&dir, &a, &["plan".into()]).unwrap();
        assert_eq!(pins(&dir, &b), Vec::<String>::new());
        set_pins(&dir, &a, &[]).unwrap();
        assert!(pins(&dir, &a).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
