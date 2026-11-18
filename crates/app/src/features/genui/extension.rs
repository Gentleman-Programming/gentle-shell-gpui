//! The Pi extension behind generative UI: the `gentle_ui` tool the agent
//! shows diagrams with and the `gentle-ui:event` command the app sends the
//! user's edits through. Shipped inside the app and written to its data
//! folder, where every agent tab loads it with `--extension`.

use std::path::{Path, PathBuf};

const SOURCE: &str = include_str!("gentle-ui.ts");

/// Write the extension to the app's data folder; its path, or `None` when it
/// can't be written.
pub fn install() -> Option<PathBuf> {
    let path = crate::shared::data_dir::app()?
        .join("extensions")
        .join("gentle-ui.ts");
    write_if_changed(&path, SOURCE)?;
    Some(path)
}

/// Write `text` to `path` unless it already holds it, so an unchanged file
/// isn't rewritten on every start.
fn write_if_changed(path: &Path, text: &str) -> Option<()> {
    if std::fs::read_to_string(path).ok().as_deref() != Some(text) {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(path, text).ok()?;
    }
    Some(())
}
