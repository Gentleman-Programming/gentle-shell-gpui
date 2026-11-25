//! `app_theme`: the app-theme workflow of the app's instructions (read the
//! guide and examples, write a theme and its scene into the app's themes
//! folder, look at its previews) as actions, for an agent that can't see
//! that folder. Paths on the user's computer are never shown.

use std::path::Path;

use pi_rpc::desktop_rpc::{HostTool, HostToolContent};
use serde_json::{Value, json};

use super::{Outcome, tool};
use crate::shared::backdrop::{self, model::file_stem, preview};
use crate::shared::themes::{self, Source};

const ACTIONS: [&str; 5] = ["guide", "examples", "example", "write", "preview"];
/// What a scene file may weigh, as the theme loader reads it.
const MAX_SCENE: usize = 1 << 20;

/// An `app_theme` call.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Guide,
    Examples,
    Example(String),
    Write { theme: Value, scene: Option<String> },
    Preview(String),
}

/// How the guide's file names map onto this tool.
const GUIDE_NOTE: &str = "\
Through the app_theme tool: the files this guide names live in the app's themes folder on the user's \
computer. Read examples with `example <name>`; write a theme (and its scene) with `write` (the scene \
is saved beside it as `<stem>.js` and the theme points at it); look at `previews/<stem>.png`, \
`.txt`, `.grid.png` and `.effects.png` with `preview <stem>` (stem: the name lowercased, runs of \
other characters as `-`). A picture must be inline in the theme (`\"image\": \"data:image/png;base64,…\"`).

";

pub fn definition() -> HostTool {
    tool(
        "app_theme",
        "App theme",
        "Make themes for the Gentle Shell app itself (its colors, an animated background or scene \
like falling petals, a koi pond, rain): guide (how), examples / example <name> (references), \
write {theme, scene?} (saves it in the app, checked by its theme loader), preview <stem> (the \
app's rendering of it and its frame cost)."
            .into(),
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ACTIONS },
                "name": { "type": "string", "description": "example: an example's name. write: the theme's name (else the theme's own)" },
                "theme": { "description": "write: the theme file's JSON (an object, or its text)" },
                "scene": { "type": "string", "description": "write: the scene's JavaScript, saved beside the theme" },
                "stem": { "type": "string", "description": "preview: the theme's file stem (or its name)" }
            },
            "required": ["action"]
        }),
        &[
            "When the user asks to theme, restyle or decorate the Gentle Shell app itself, use app_theme: read guide and two similar examples first, then write the theme.",
            "A few seconds after each write, call app_theme preview <stem> (four moments of the scene, frame cost, errors); fix and write again until it looks good and runs at full rate.",
            "Tell the user to pick the theme in Configuration › Theme › Animated backgrounds.",
        ],
    )
}

/// The call `args` asks for, or what is wrong with it.
pub fn route(args: &Value) -> Result<Op, String> {
    let text = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    match text("action").unwrap_or_default() {
        "guide" => Ok(Op::Guide),
        "examples" => Ok(Op::Examples),
        "example" => {
            let name = text("name").ok_or("app_theme example needs name")?;
            Ok(Op::Example(name.to_string()))
        }
        "write" => {
            let mut theme = match args.get("theme") {
                // Some models send the JSON as a string.
                Some(Value::String(text)) => serde_json::from_str(text)
                    .map_err(|error| format!("app_theme write: theme isn't JSON: {error}"))?,
                Some(theme) => theme.clone(),
                None => return Err("app_theme write needs theme".into()),
            };
            let Some(object) = theme.as_object_mut() else {
                return Err("app_theme write: theme must be a JSON object".into());
            };
            if let Some(name) = text("name") {
                object.insert("name".into(), name.into());
            }
            if object
                .get("name")
                .and_then(Value::as_str)
                .is_none_or(|name| name.trim().is_empty())
            {
                return Err("app_theme write needs the theme's name".into());
            }
            let scene = args
                .get("scene")
                .and_then(Value::as_str)
                .filter(|scene| !scene.trim().is_empty());
            if scene.is_some_and(|scene| scene.len() > MAX_SCENE) {
                return Err("app_theme write: the scene is over 1 MiB".into());
            }
            Ok(Op::Write {
                theme,
                scene: scene.map(str::to_string),
            })
        }
        "preview" => {
            let stem = text("stem")
                .or_else(|| text("name"))
                .ok_or("app_theme preview needs stem")?;
            Ok(Op::Preview(file_stem(stem)))
        }
        other => Err(format!("app_theme: unknown action {other:?}")),
    }
}

/// Run `op`. Blocking.
pub fn run(op: Op) -> Outcome {
    let result = match op {
        Op::Guide => Ok(Outcome::text(format!(
            "{GUIDE_NOTE}{}",
            crate::features::devices::theme_guide()
        ))),
        Op::Examples => Ok(Outcome::text(examples())),
        Op::Example(name) => example(&name).map(Outcome::text),
        Op::Write { theme, scene } => themes::app_themes_dir()
            .ok_or_else(|| "the app has no data folder to keep themes in".to_string())
            .and_then(|dir| write(&dir, theme, scene.as_deref()))
            .map(Outcome::text),
        Op::Preview(stem) => themes::app_themes_dir()
            .ok_or_else(|| "the app has no data folder".to_string())
            .and_then(|dir| read_preview(&dir, &stem)),
    };
    result.unwrap_or_else(Outcome::error)
}

/// The example themes (with the stems `example` takes) and built-in scenes.
fn examples() -> String {
    let themes: Vec<String> = themes::example_themes()
        .iter()
        .map(|theme| format!("- {} ({})", file_stem(&theme.name), theme.name))
        .collect();
    let scenes: Vec<String> = backdrop::builtin_scene_names()
        .map(|name| format!("- {name}"))
        .collect();
    format!(
        "Example themes (app_theme example <stem>):\n{}\n\nBuilt-in scenes (a theme's \"scene\" may name one; app_theme example <name> shows its code):\n{}",
        themes.join("\n"),
        scenes.join("\n")
    )
}

/// The example theme and built-in scene `name` names (`.json` or `.js`
/// optional).
fn example(name: &str) -> Result<String, String> {
    let name = name.trim_end_matches(".json").trim_end_matches(".js");
    let stem = file_stem(name);
    let mut parts = Vec::new();
    for text in themes::example_theme_texts() {
        let Some(theme) = themes::parse_pi_theme(text, Source::App, None) else {
            continue;
        };
        if file_stem(&theme.name) == stem {
            parts.push(format!("{stem}.json:\n{text}"));
        }
    }
    if let Some(scene) = backdrop::builtin_scene(name).or_else(|| backdrop::builtin_scene(&stem)) {
        parts.push(format!("{stem}.js (built-in scene \"{stem}\"):\n{scene}"));
    }
    if parts.is_empty() {
        return Err(format!(
            "app_theme: no example {name:?}; examples lists them"
        ));
    }
    Ok(parts.join("\n\n"))
}

/// Save `theme` (and its `scene`, beside it) in `dir` once the app's theme
/// loader reads it as written. What was saved, in words for the agent.
fn write(dir: &Path, mut theme: Value, scene: Option<&str>) -> Result<String, String> {
    let name = theme["name"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let stem = file_stem(&name);
    let scene_file = format!("{stem}.js");
    if scene.is_some() {
        let background = &mut theme["background"];
        if !background.is_object() {
            *background = json!({});
        }
        background["scene"] = scene_file.clone().into();
        if let Some(background) = background.as_object_mut() {
            background.remove("sceneCode");
        }
    }
    let text = serde_json::to_string_pretty(&theme).map_err(|e| e.to_string())?;
    // Check it as the app will read it: in a folder of its own first, so a
    // theme that doesn't load replaces nothing.
    let check =
        std::env::temp_dir().join(format!("gentle-theme-check-{}-{stem}", std::process::id()));
    let checked = (|| {
        std::fs::create_dir_all(&check).map_err(|e| e.to_string())?;
        if let Some(scene) = scene {
            std::fs::write(check.join(&scene_file), scene).map_err(|e| e.to_string())?;
        }
        let file = check.join(format!("{stem}.json"));
        let parsed = themes::parse_pi_theme(&text, Source::App, Some(&file))
            .ok_or("the app can't read it as a theme (it needs at least a \"name\")")?;
        if theme.get("background").is_some() && parsed.background.is_none() {
            return Err(
                "the app can't read its background: check it against the guide (a scene file over 1 MiB, an unknown built-in scene, or a picture it can't decode)"
                    .to_string(),
            );
        }
        Ok(parsed)
    })();
    let _ = std::fs::remove_dir_all(&check);
    let parsed = checked.map_err(|error| format!("app_theme write: {error}; nothing saved"))?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    if let Some(scene) = scene {
        std::fs::write(dir.join(&scene_file), scene)
            .map_err(|error| format!("app_theme write: {error}"))?;
    }
    std::fs::write(dir.join(format!("{stem}.json")), &text)
        .map_err(|error| format!("app_theme write: {error}"))?;
    let previewed = parsed
        .background
        .as_ref()
        .is_some_and(|b| b.scene.is_some() || b.image.is_some());
    let mut answer = format!("Saved the theme \"{name}\" (stem {stem}) in the app.");
    if previewed {
        answer.push_str(&format!(
            " Its previews follow in a few seconds: app_theme preview {stem}."
        ));
    }
    answer.push_str(" The user picks it in Configuration › Theme › Animated backgrounds.");
    Ok(answer)
}

/// The app's previews of the theme `stem`: its pictures, its report, and
/// why the active theme's scene stopped when it did.
fn read_preview(dir: &Path, stem: &str) -> Result<Outcome, String> {
    let out = dir.join(preview::FOLDER);
    let mut content = Vec::new();
    let mut said = Vec::new();
    if let Ok(report) = std::fs::read_to_string(out.join(format!("{stem}.txt"))) {
        said.push(report);
    }
    if let Ok(error) = std::fs::read_to_string(dir.join("scene-error.txt")) {
        said.push(format!("The active theme's scene failed:\n{error}"));
    }
    for (suffix, what) in [
        (".png", "four moments of the scene"),
        (
            ".grid.png",
            "the picture with its coordinates and effect regions",
        ),
        (".effects.png", "each effect in motion"),
    ] {
        if let Ok(bytes) = std::fs::read(out.join(format!("{stem}{suffix}"))) {
            content.push(HostToolContent::text(format!("{stem}{suffix}: {what}")));
            content.push(super::png(&bytes));
        }
    }
    if content.is_empty() && said.is_empty() {
        return Err(format!(
            "No preview of {stem} yet: the app draws one a few seconds after a theme with a scene or picture is saved. Try again shortly."
        ));
    }
    if !said.is_empty() {
        content.insert(0, HostToolContent::text(said.join("\n\n")));
    }
    Ok(Outcome {
        content,
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_route() {
        assert_eq!(route(&json!({"action": "guide"})), Ok(Op::Guide));
        assert_eq!(route(&json!({"action": "examples"})), Ok(Op::Examples));
        assert_eq!(
            route(&json!({"action": "example", "name": "petals"})),
            Ok(Op::Example("petals".into()))
        );
        assert_eq!(
            route(&json!({"action": "preview", "stem": "../Sakura Dusk"})),
            Ok(Op::Preview("sakura-dusk".into()))
        );
        assert_eq!(
            route(
                &json!({"action": "write", "name": "Rain", "theme": "{\"vars\": {}}", "scene": "x"})
            ),
            Ok(Op::Write {
                theme: json!({"vars": {}, "name": "Rain"}),
                scene: Some("x".into())
            })
        );
    }

    #[test]
    fn bad_calls_say_what_is_missing() {
        let error = |args: Value| route(&args).unwrap_err();
        assert!(error(json!({"action": "paint"})).contains("unknown"));
        assert!(error(json!({"action": "example"})).contains("name"));
        assert!(error(json!({"action": "write"})).contains("theme"));
        assert!(error(json!({"action": "write", "theme": "{"})).contains("JSON"));
        assert!(error(json!({"action": "write", "theme": [1]})).contains("object"));
        assert!(error(json!({"action": "write", "theme": {"vars": {}}})).contains("name"));
        assert!(error(json!({"action": "preview"})).contains("stem"));
    }

    #[test]
    fn examples_are_listed_and_read() {
        let list = examples();
        assert!(list.contains("petals"));
        assert!(example("petals.json").unwrap().contains("petals.json:"));
        assert!(example("nope").is_err());
    }

    #[test]
    fn a_written_theme_is_checked_then_saved_beside_its_scene() {
        let dir = std::env::temp_dir().join(format!("gentle-theme-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let saved = write(
            &dir,
            json!({"name": "Night Rain", "vars": {"bg": "#101020"}, "background": {}}),
            Some("SCENES.rain = { draw() {} };"),
        )
        .unwrap();
        assert!(saved.contains("night-rain"));
        let theme: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("night-rain.json")).unwrap())
                .unwrap();
        assert_eq!(theme["background"]["scene"], "night-rain.js");
        assert!(dir.join("night-rain.js").is_file());
        assert!(read_preview(&dir, "night-rain").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
