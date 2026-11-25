//! `update` patches, as the extension merges them: each kind's lists
//! upsert by id (null clears a field), `remove: {...}` deletes, and the
//! merged spec is normalized again, so a patch can't produce what a full
//! spec couldn't.

use serde_json::{Map, Value, json};

use super::id_text;
use super::normalize::normalize;

type Json = Map<String, Value>;
/// How a found item takes a patch item, when not field by field.
type MergeOne<'a> = &'a dyn Fn(&mut Value, &Json);

/// `spec` (normalized, of `kind`) with `patch` merged, normalized.
pub fn patch(kind: &str, spec: &Value, patch: &Value, root: &str) -> Result<Value, String> {
    let Some(patch) = patch.as_object() else {
        return Err("patch must be an object".into());
    };
    let mut next = spec.clone();
    match kind {
        "diagram" => diagram(&mut next, patch)?,
        "table" => table(&mut next, patch)?,
        "form" => form(&mut next, patch)?,
        "checklist" => checklist(&mut next, patch)?,
        "kanban" => kanban(&mut next, patch)?,
        "chart" => chart(&mut next, patch)?,
        "html" => html(&mut next, patch)?,
        "review" => review(&mut next, patch)?,
        "sequence" => sequence(&mut next, patch)?,
        "erd" => erd(&mut next, patch)?,
        "state" => state(&mut next, patch)?,
        "timeline" => timeline(&mut next, patch)?,
        _ => return Err(format!("unknown kind \"{kind}\"")),
    }
    normalize(kind, &next, root)
}

/// `item`'s fields onto `target`, but `skip`: null deletes a field.
fn merge_into(target: &mut Value, item: &Json, skip: &[&str]) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    for (key, value) in item {
        if skip.contains(&key.as_str()) {
            continue;
        }
        if value.is_null() {
            target.remove(key);
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// `next[key]` as a list (made one when missing).
fn list_mut<'a>(next: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    if !next[key].is_array() {
        next[key] = json!([]);
    }
    next[key].as_array_mut().expect("an array")
}

/// The ids of `patch.remove[key]`, as `idText` reads them.
fn removed(patch: &Json, key: &str) -> Vec<String> {
    patch
        .get("remove")
        .filter(|r| r.is_object())
        .and_then(|r| r.get(key))
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(|id| id_text(Some(id))).collect())
        .unwrap_or_default()
}

/// The strings of `patch.remove[key]` (the diagram's exact-id removal).
fn removed_strings(patch: &Json, key: &str) -> Vec<String> {
    patch
        .get("remove")
        .filter(|r| r.is_object())
        .and_then(|r| r.get(key))
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn has(list: &[String], value: &Value) -> bool {
    value.as_str().is_some_and(|v| list.iter().any(|x| x == v))
}

/// `patch[key]`: absent or null → none; otherwise an array of objects.
fn items<'a>(patch: &'a Json, key: &str, where_: &str) -> Result<Vec<&'a Json>, String> {
    match patch.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(list)) if list.iter().all(Value::is_object) => {
            Ok(list.iter().filter_map(Value::as_object).collect())
        }
        Some(_) => Err(format!("patch.{where_} must be an array of objects")),
    }
}

/// Upsert `patch[where_]` into `target` by `key` (`idText` on both sides):
/// a found item merges (null clears; `merge_one` when given), a new one is
/// appended.
fn upsert(
    target: &mut Vec<Value>,
    patch: &Json,
    where_: &str,
    key: &str,
    merge_one: Option<MergeOne>,
) -> Result<(), String> {
    for item in items(patch, where_, where_)? {
        let Some(id) = id_text(item.get(key)) else {
            return Err(format!("every item in patch.{where_} needs a {key}"));
        };
        match target
            .iter_mut()
            .find(|existing| id_text(existing.get(key)).as_deref() == Some(&id))
        {
            None => target.push(Value::Object(item.clone())),
            Some(found) => match merge_one {
                Some(merge_one) => merge_one(found, item),
                None => merge_into(found, item, &[key]),
            },
        }
    }
    Ok(())
}

/// Top-level `keys` of `patch` onto `next` (null clears).
fn scalars(next: &mut Value, patch: &Json, keys: &[&str]) {
    for key in keys {
        if let Some(value) = patch.get(*key) {
            if value.is_null() {
                if let Some(next) = next.as_object_mut() {
                    next.remove(*key);
                }
            } else {
                next[*key] = value.clone();
            }
        }
    }
}

fn retain(next: &mut Value, key: &str, keep: impl FnMut(&Value) -> bool) {
    if let Some(list) = next[key].as_array_mut() {
        list.retain(keep);
    }
}

fn remove_field(item: &mut Value, key: &str) {
    if let Some(item) = item.as_object_mut() {
        item.remove(key);
    }
}

fn diagram(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone_nodes = removed_strings(patch, "nodes");
    let gone_edges = removed_strings(patch, "edges");
    let gone_groups = removed_strings(patch, "groups");
    retain(next, "nodes", |n| !has(&gone_nodes, &n["id"]));
    retain(next, "edges", |e| {
        !has(&gone_edges, &e["id"]) && !has(&gone_nodes, &e["from"]) && !has(&gone_nodes, &e["to"])
    });
    retain(next, "groups", |g| !has(&gone_groups, &g["id"]));
    for node in list_mut(next, "nodes") {
        if has(&gone_groups, &node["group"]) {
            remove_field(node, "group");
        }
    }
    for key in ["groups", "nodes"] {
        for item in items(patch, key, key)? {
            let Some(id) = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
            else {
                return Err(format!("every item in patch.{key} needs an id"));
            };
            let list = list_mut(next, key);
            match list.iter_mut().find(|existing| existing["id"] == id) {
                Some(found) => merge_into(found, item, &["id"]),
                None => list.push(Value::Object(item.clone())),
            }
        }
    }
    for item in items(patch, "edges", "edges")? {
        let edges = list_mut(next, "edges");
        let found = match item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            Some(id) => edges.iter_mut().find(|e| e["id"] == id),
            None => edges
                .iter_mut()
                .find(|e| Some(&e["from"]) == item.get("from") && Some(&e["to"]) == item.get("to")),
        };
        match found {
            Some(found) => merge_into(found, item, &["id"]),
            None => edges.push(Value::Object(item.clone())),
        }
    }
    if let Some(direction) = patch.get("direction").filter(|d| d.is_string()) {
        next["direction"] = direction.clone();
    }
    Ok(())
}

fn table(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone_rows = removed(patch, "rows");
    let gone_columns = removed(patch, "columns");
    retain(next, "rows", |r| !has(&gone_rows, &r["id"]));
    retain(next, "columns", |c| !has(&gone_columns, &c["key"]));
    upsert(list_mut(next, "columns"), patch, "columns", "key", None)?;
    let cells = |found: &mut Value, item: &Json| {
        let changes: Json = match item.get("cells") {
            Some(Value::Object(cells)) => cells.clone(),
            _ => item
                .iter()
                .filter(|(k, _)| k.as_str() != "id")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        if !found["cells"].is_object() {
            found["cells"] = json!({});
        }
        merge_into(&mut found["cells"], &changes, &[]);
    };
    upsert(list_mut(next, "rows"), patch, "rows", "id", Some(&cells))?;
    scalars(
        next,
        patch,
        &["sort", "selection", "editable", "selectable", "live"],
    );
    Ok(())
}

/// An update reopens a submitted form unless the patch says `submitted: true`.
fn form(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone = removed(patch, "fields");
    retain(next, "fields", |f| !has(&gone, &f["id"]));
    upsert(list_mut(next, "fields"), patch, "fields", "id", None)?;
    if let Some(values) = patch.get("values").and_then(Value::as_object) {
        if !next["values"].is_object() {
            next["values"] = json!({});
        }
        merge_into(&mut next["values"], values, &[]);
    }
    scalars(next, patch, &["description", "submitLabel"]);
    next["submitted"] = json!(patch.get("submitted") == Some(&Value::Bool(true)));
    Ok(())
}

/// Where a checklist item is: at the top level, or under the top item at.
#[derive(Clone, Copy, PartialEq)]
enum Home {
    Top,
    Under(usize),
}

/// Items upsert by id at either level; new ones go to the end of the top
/// level or of `parent`'s sub-items; `parent` on an existing item moves it.
/// An item's `done` or `status` replaces the other; `run` and `live` replace.
fn checklist(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone = removed(patch, "items");
    retain(next, "items", |i| !has(&gone, &i["id"]));
    for top in list_mut(next, "items") {
        if let Some(subs) = top["items"].as_array_mut() {
            subs.retain(|s| !has(&gone, &s["id"]));
        }
    }
    let changes = match patch.get("items") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list)) if list.iter().all(Value::is_object) => {
            list.iter().filter_map(Value::as_object).collect()
        }
        Some(_) => return Err("patch.items must be an array of objects".into()),
    };
    for item in changes {
        let Some(id) = id_text(item.get("id")) else {
            return Err("every item in patch.items needs an id".into());
        };
        let tops = list_mut(next, "items");
        let place = |list: &[Value]| list.iter().position(|i| i["id"] == id.as_str());
        let mut found: Option<(Home, usize)> = place(tops).map(|at| (Home::Top, at));
        if found.is_none() {
            found = tops.iter().enumerate().find_map(|(t, top)| {
                place(top["items"].as_array().map_or(&[], Vec::as_slice))
                    .map(|at| (Home::Under(t), at))
            });
        }
        let parent_given = item.contains_key("parent");
        let parent = id_text(item.get("parent"));
        let target = match &parent {
            None => Some(Home::Top),
            Some(parent) => tops
                .iter()
                .position(|t| t["id"] == parent.as_str())
                .map(Home::Under),
        };
        if parent_given && target.is_none() {
            return Err(format!(
                "patch.items: \"{}\" is not a top-level item",
                parent.unwrap_or_default()
            ));
        }
        let (home, at) = match found {
            Some((home, at)) if parent_given && Some(home) != target => {
                let moved = list_at(tops, home).remove(at);
                // Taking it out may shift the parent's place: find it again.
                let target = match &parent {
                    None => Home::Top,
                    Some(parent) => Home::Under(
                        tops.iter()
                            .position(|t| t["id"] == parent.as_str())
                            .ok_or_else(|| {
                                format!("patch.items: \"{parent}\" is not a top-level item")
                            })?,
                    ),
                };
                let list = list_at(tops, target);
                list.push(moved);
                (target, list.len() - 1)
            }
            Some(found) => found,
            None => {
                let home = if parent_given {
                    target.expect("checked above")
                } else {
                    Home::Top
                };
                let list = list_at(tops, home);
                list.push(json!({ "id": id }));
                (home, list.len() - 1)
            }
        };
        let found = &mut list_at(tops, home)[at];
        if item.contains_key("done") && !item.contains_key("status") {
            remove_field(found, "status");
        }
        if item.contains_key("status") && !item.contains_key("done") {
            remove_field(found, "done");
        }
        merge_into(found, item, &["id", "parent"]);
    }
    scalars(next, patch, &["run", "live"]);
    Ok(())
}

/// The top-level list, or the sub-items of the top item at (made a list
/// when missing).
fn list_at(tops: &mut Vec<Value>, home: Home) -> &mut Vec<Value> {
    match home {
        Home::Top => tops,
        Home::Under(t) => {
            if !tops[t]["items"].is_array() {
                tops[t]["items"] = json!([]);
            }
            tops[t]["items"].as_array_mut().expect("an array")
        }
    }
}

fn kanban(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone_columns = removed(patch, "columns");
    let gone_cards = removed(patch, "cards");
    retain(next, "columns", |c| !has(&gone_columns, &c["id"]));
    retain(next, "cards", |c| {
        !has(&gone_cards, &c["id"]) && !has(&gone_columns, &c["column"])
    });
    upsert(list_mut(next, "columns"), patch, "columns", "id", None)?;
    upsert(list_mut(next, "cards"), patch, "cards", "id", None)
}

/// Series upsert by name; top-level keys replace.
fn chart(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone = removed(patch, "series");
    retain(next, "series", |s| !has(&gone, &s["name"]));
    upsert(list_mut(next, "series"), patch, "series", "name", None)?;
    scalars(
        next,
        patch,
        &[
            "type", "labels", "xTitle", "yTitle", "unit", "stacked", "live",
        ],
    );
    Ok(())
}

/// html, height and csp replace (null clears); data merges one level (null
/// deletes a key).
fn html(next: &mut Value, patch: &Json) -> Result<(), String> {
    scalars(next, patch, &["html", "height", "csp"]);
    match patch.get("data") {
        None => {}
        Some(Value::Null) => remove_field(next, "data"),
        Some(Value::Object(data)) => {
            if !next["data"].is_object() {
                next["data"] = json!({});
            }
            merge_into(&mut next["data"], data, &["id"]);
        }
        Some(_) => return Err("patch.data must be an object".into()),
    }
    Ok(())
}

/// `applied` marks hunks written; `hunks` merge by id (null clears, e.g.
/// `decision: null` asks again); `files` upsert by path; `remove: {files, hunks}`.
fn review(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone_files = removed(patch, "files");
    let gone_hunks = removed(patch, "hunks");
    retain(next, "files", |f| !has(&gone_files, &f["path"]));
    for file in list_mut(next, "files") {
        retain(file, "hunks", |h| !has(&gone_hunks, &h["id"]));
    }
    fn hunk<'a>(next: &'a mut Value, id: &str, where_: &str) -> Result<&'a mut Value, String> {
        list_mut(next, "files")
            .iter_mut()
            .flat_map(|f| f["hunks"].as_array_mut().into_iter().flatten())
            .find(|h| h["id"] == id)
            .ok_or_else(|| format!("{where_}: unknown hunk \"{id}\" (add hunks through files)"))
    }
    match patch.get("applied") {
        None => {}
        Some(Value::Array(ids)) => {
            for id in ids {
                let id = id_text(Some(id)).unwrap_or_default();
                hunk(next, &id, "patch.applied")?["applied"] = json!(true);
            }
        }
        Some(_) => return Err("patch.applied must be an array of hunk ids".into()),
    }
    for item in items(patch, "hunks", "hunks")? {
        let id = id_text(item.get("id")).unwrap_or_default();
        merge_into(hunk(next, &id, "patch.hunks")?, item, &["id"]);
    }
    for item in items(patch, "files", "files")? {
        let Some(path) = review_path(item.get("path")) else {
            return Err("every item in patch.files needs a path".into());
        };
        let mut file = item.clone();
        file.insert("path".into(), json!(path));
        let files = list_mut(next, "files");
        match files.iter().position(|f| f["path"] == path.as_str()) {
            Some(at) => files[at] = Value::Object(file),
            None => files.push(Value::Object(file)),
        }
    }
    scalars(next, patch, &["summary"]);
    Ok(())
}

fn review_path(value: Option<&Value>) -> Option<String> {
    let path = value?.as_str()?.trim().replace('\\', "/");
    let clean = path.strip_prefix("./").unwrap_or(&path);
    (!clean.is_empty()).then(|| clean.to_string())
}

/// Items of `next[key]` with `patch[key]` upserted by id; items without an
/// id are appended (normalizing names them).
fn upsert_all(next: &mut Value, patch: &Json, key: &str) -> Result<(), String> {
    for item in items(patch, key, key)? {
        let list = list_mut(next, key);
        let id = id_text(item.get("id"));
        let found = id.as_ref().and_then(|id| {
            list.iter_mut()
                .find(|existing| id_text(existing.get("id")).as_ref() == Some(id))
        });
        match found {
            Some(found) => merge_into(found, item, &["id"]),
            None => list.push(Value::Object(item.clone())),
        }
    }
    Ok(())
}

/// Upserts by id (`at`: a message's 1-based place); removing a message drops
/// the fragments and activations it bounds.
fn sequence(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone_participants = removed(patch, "participants");
    let gone_messages = removed(patch, "messages");
    let gone_fragments = removed(patch, "fragments");
    retain(next, "participants", |p| !has(&gone_participants, &p["id"]));
    retain(next, "messages", |m| {
        !has(&gone_messages, &m["id"])
            && !has(&gone_participants, &m["from"])
            && !has(&gone_participants, &m["to"])
    });
    let kept: Vec<Value> = list_mut(next, "messages")
        .iter()
        .map(|m| m["id"].clone())
        .collect();
    let kept = |id: &Value| kept.contains(id);
    retain(next, "fragments", |f| {
        !has(&gone_fragments, &f["id"]) && kept(&f["from"]) && kept(&f["to"])
    });
    for fragment in list_mut(next, "fragments") {
        if let Some(branches) = fragment["else"].as_array_mut() {
            branches.retain(|b| kept(&b["at"]));
        }
    }
    if next["activations"].is_array() {
        retain(next, "activations", |a| {
            kept(&a["from"]) && kept(&a["to"]) && !has(&gone_participants, &a["participant"])
        });
    }
    upsert_all(next, patch, "participants")?;
    upsert_all(next, patch, "messages")?;
    upsert_all(next, patch, "fragments")?;
    for item in patch
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = id_text(item.get("id"));
        let at = item.get("at").and_then(whole_number);
        let messages = list_mut(next, "messages");
        let from = messages
            .iter()
            .position(|m| id.as_deref().is_some_and(|id| m["id"] == id));
        let (Some(at), Some(from)) = (at, from) else {
            continue;
        };
        let mut moved = messages.remove(from);
        remove_field(&mut moved, "at");
        let place = (at - 1).clamp(0, messages.len() as i64) as usize;
        messages.insert(place, moved);
    }
    match patch.get("activations") {
        None => {}
        Some(Value::Null) => remove_field(next, "activations"),
        Some(activations) => next["activations"] = activations.clone(),
    }
    Ok(())
}

/// A JSON number that is an integer (`Number.isInteger`).
fn whole_number(value: &Value) -> Option<i64> {
    let n = value.as_f64()?;
    (n.is_finite() && n.fract() == 0.).then_some(n as i64)
}

/// Tables and relations upsert by id (a table's columns replace); removing a
/// table or a column (`remove.columns: ["table.column"]`) drops what used it.
fn erd(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone_tables = removed(patch, "tables");
    let gone_relations = removed(patch, "relations");
    let gone_columns = removed_strings(patch, "columns");
    let gone = |table: &str, column: &str| {
        gone_tables.iter().any(|t| t == table)
            || gone_columns.contains(&format!("{table}.{column}"))
    };
    retain(next, "tables", |t| !has(&gone_tables, &t["id"]));
    for table in list_mut(next, "tables") {
        let id = table["id"].as_str().unwrap_or_default().to_string();
        retain(table, "columns", |c| {
            !gone_columns.contains(&format!("{id}.{}", c["name"].as_str().unwrap_or_default()))
        });
        for column in list_mut(table, "columns") {
            let fk = &column["fk"];
            if fk.is_object()
                && gone(
                    fk["table"].as_str().unwrap_or_default(),
                    fk["column"].as_str().unwrap_or_default(),
                )
            {
                remove_field(column, "fk");
            }
        }
    }
    let end_gone = |end: &Value| {
        let end = super::js_string(end);
        gone_tables
            .iter()
            .any(|t| end.starts_with(&format!("{t}.")))
            || gone_columns.contains(&end)
    };
    retain(next, "relations", |r| {
        !has(&gone_relations, &r["id"]) && !end_gone(&r["from"]) && !end_gone(&r["to"])
    });
    upsert_all(next, patch, "tables")?;
    upsert_all(next, patch, "relations")
}

/// States, transitions and groups upsert by id; removing a state drops its
/// transitions, removing a group takes its states out of it.
fn state(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone_states = removed(patch, "states");
    let gone_groups = removed(patch, "groups");
    let gone_transitions = removed(patch, "transitions");
    retain(next, "states", |s| !has(&gone_states, &s["id"]));
    for state in list_mut(next, "states") {
        if has(&gone_groups, &state["group"]) {
            remove_field(state, "group");
        }
    }
    retain(next, "groups", |g| !has(&gone_groups, &g["id"]));
    retain(next, "transitions", |t| {
        !has(&gone_transitions, &t["id"])
            && !has(&gone_states, &t["from"])
            && !has(&gone_states, &t["to"])
    });
    if has(&gone_states, &next["current"]) {
        remove_field(next, "current");
    }
    upsert_all(next, patch, "groups")?;
    upsert_all(next, patch, "states")?;
    upsert_all(next, patch, "transitions")?;
    scalars(next, patch, &["current", "direction"]);
    Ok(())
}

/// Tasks upsert by id (`end` or `duration` replaces the other); removing a
/// task takes it out of every `dependsOn`.
fn timeline(next: &mut Value, patch: &Json) -> Result<(), String> {
    let gone = removed(patch, "tasks");
    retain(next, "tasks", |t| !has(&gone, &t["id"]));
    for task in list_mut(next, "tasks") {
        if let Some(deps) = task["dependsOn"].as_array_mut() {
            deps.retain(|d| !has(&gone, d));
        }
    }
    for item in patch
        .get("tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let id = id_text(item.get("id"));
        let Some(found) = list_mut(next, "tasks")
            .iter_mut()
            .find(|t| id.as_deref().is_some_and(|id| t["id"] == id))
        else {
            continue;
        };
        if item.contains_key("duration") && !item.contains_key("end") {
            remove_field(found, "end");
        }
        if item.contains_key("end") {
            remove_field(found, "duration");
        }
    }
    upsert_all(next, patch, "tasks")
}
