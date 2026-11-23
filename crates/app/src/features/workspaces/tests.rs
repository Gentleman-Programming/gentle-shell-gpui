use std::path::PathBuf;

use super::store::{load_from, save_to};
use super::{
    Badge, BadgeColor, BadgeIcon, SavedPane, SavedState, SavedTab, SavedWorkspace, TabLayout,
    VERSION,
};
use crate::shared::pane::{PaneKind, PaneTree, SplitAxis};

/// Exercise the store boundary without relying on identity's Rust representation.
#[test]
fn workspace_identity_normalizes_and_survives_save() {
    for recovery in [false, true] {
        let dir = std::env::temp_dir().join(format!(
            "gentle-ws-identity-{}-{recovery}",
            std::process::id()
        ));
        let path = dir.join("workspaces.json");
        std::fs::create_dir_all(&dir).unwrap();
        let mut input = serde_json::to_value(sample()).unwrap();
        let template = input["workspaces"][1].clone();
        input["workspaces"] = serde_json::json!([
            template.clone(),
            template.clone(),
            template.clone(),
            template.clone(),
            template.clone(),
            template.clone()
        ]);
        let kept = "0123456789abcdef0123456789abcdef";
        input["workspaces"][0]["id"] = kept.into();
        input["workspaces"][1]["id"] = kept.into();
        input["workspaces"][2].as_object_mut().unwrap().remove("id");
        input["workspaces"][3]["id"] = "".into();
        input["workspaces"][4]["id"] = 42.into();
        input["workspaces"][5]["id"] = "invalid".into();
        // Recovery drops only the extra unknown tab; all original fields survive.
        if recovery {
            let mut unknown = template["tabs"][0].clone();
            unknown["layout"] = serde_json::json!({"leaf": {"kind": "future", "cwd": "/tmp"}});
            input["workspaces"][0]["tabs"]
                .as_array_mut()
                .unwrap()
                .push(unknown);
        }
        let text = input.to_string();
        std::fs::write(&path, &text).unwrap();
        let state = load_from(&path).expect("identity repair must not reject a workspace");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text,
            "load never writes"
        );
        assert!(!path.with_extension("json.unreadable").exists());
        let actual = serde_json::to_value(&state).unwrap();
        let mut ids = std::collections::HashSet::new();
        for workspace in actual["workspaces"].as_array().unwrap() {
            let id = workspace["id"]
                .as_str()
                .expect("every workspace gets an ID");
            assert_eq!(id.len(), 32);
            assert!(
                id.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
            assert!(ids.insert(id), "same-root workspaces need distinct IDs");
        }
        assert_eq!(actual["workspaces"][0]["id"], kept);
        let mut expected = input;
        if recovery {
            expected["workspaces"][0]["tabs"]
                .as_array_mut()
                .unwrap()
                .pop();
        }
        let strip_ids = |mut value: serde_json::Value| {
            for workspace in value["workspaces"].as_array_mut().unwrap() {
                workspace.as_object_mut().unwrap().remove("id");
            }
            value
        };
        assert_eq!(strip_ids(actual), strip_ids(expected));
        save_to(&path, &state).unwrap();
        assert_eq!(load_from(&path), Some(state.clone()));
        // Rename and root changes retain identity on the existing save path.
        let mut changed = state;
        changed.workspaces[0].name = "renamed".into();
        changed.workspaces[0].root = PathBuf::from("/elsewhere");
        save_to(&path, &changed).unwrap();
        assert_eq!(load_from(&path), Some(changed));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

fn sample() -> SavedState {
    let agent = SavedPane {
        kind: PaneKind::Agent,
        cwd: PathBuf::from("/tmp/project"),
        path: None,
        session: Some(PathBuf::from("/tmp/sessions/a.jsonl")),
    };
    let terminal = SavedPane {
        kind: PaneKind::Terminal,
        cwd: PathBuf::from("/tmp/project"),
        path: None,
        session: None,
    };
    let file = SavedPane {
        kind: PaneKind::File,
        cwd: PathBuf::from("/tmp/project"),
        path: Some(PathBuf::from("/tmp/project/README.md")),
        session: None,
    };
    SavedState {
        version: VERSION,
        current: 1,
        workspaces: vec![
            SavedWorkspace {
                id: "11111111111111111111111111111111".into(),
                name: "Default".into(),
                root: PathBuf::from("/tmp"),
                tabs: vec![SavedTab {
                    layout: PaneTree::Leaf(agent.clone()),
                    pinned: true,
                    focused: 0,
                }],
                active_tab: 0,
                last_active: 10,
                badge: Badge::default(),
            },
            SavedWorkspace {
                id: "22222222222222222222222222222222".into(),
                name: "project".into(),
                root: PathBuf::from("/tmp/project"),
                tabs: vec![SavedTab {
                    pinned: false,
                    layout: PaneTree::Split {
                        axis: SplitAxis::Horizontal,
                        ratio: 0.4,
                        first: Box::new(PaneTree::Leaf(agent)),
                        second: Box::new(PaneTree::Split {
                            axis: SplitAxis::Vertical,
                            ratio: 0.5,
                            first: Box::new(PaneTree::Leaf(terminal)),
                            second: Box::new(PaneTree::Leaf(file)),
                        }),
                    },
                    focused: 2,
                }],
                active_tab: 0,
                last_active: 20,
                badge: Badge {
                    icon: Some(BadgeIcon::Rocket),
                    color: Some(BadgeColor::Teal),
                },
            },
        ],
        sidebar: false,
        tab_layout: TabLayout::Vertical,
    }
}

/// An icon or color saved by a newer build reads as unset, keeping the rest
/// of the badge and the workspace.
#[test]
fn unknown_badge_values_read_as_unset() {
    let mut json = serde_json::to_value(sample()).unwrap();
    json["workspaces"][1]["badge"]["icon"] = "unicorn".into();
    let state: SavedState = serde_json::from_value(json).unwrap();
    assert_eq!(
        state.workspaces[1].badge,
        Badge {
            icon: None,
            color: Some(BadgeColor::Teal),
        }
    );
    // A workspace without a chosen badge doesn't write one.
    let plain = serde_json::to_value(&state.workspaces[0]).unwrap();
    assert!(plain.get("badge").is_none());
}

#[test]
fn json_round_trip() {
    let state = sample();
    let json = serde_json::to_string(&state).unwrap();
    let back: SavedState = serde_json::from_str(&json).unwrap();
    assert_eq!(back, state);
    // Optional fields stay out of the file and default back in.
    assert!(!json.contains("\"path\":null"));
    let minimal: SavedState =
        serde_json::from_str(r#"{"workspaces":[{"name":"x","root":"/x"}]}"#).unwrap();
    assert_eq!(minimal.workspaces[0].tabs.len(), 0);
    assert_eq!(minimal.current, 0);
    // A file from before the sidebar was saved keeps it showing.
    assert!(minimal.sidebar);
    assert_eq!(minimal.tab_layout, TabLayout::Horizontal);
    assert!(minimal.workspaces[0].tabs.is_empty());
    assert!(back.workspaces[0].tabs[0].pinned);

    // Data saved before this feature must still load without layout or pin fields.
    let mut legacy = serde_json::to_value(&state).unwrap();
    legacy.as_object_mut().unwrap().remove("tab_layout");
    legacy["workspaces"][0]["tabs"][0]
        .as_object_mut()
        .unwrap()
        .remove("pinned");
    let old: SavedState = serde_json::from_value(legacy).unwrap();
    assert_eq!(old.tab_layout, TabLayout::Horizontal);
    assert!(!old.workspaces[0].tabs[0].pinned);
    assert_eq!(SavedState::default().sidebar, minimal.sidebar);
}

#[test]
fn file_round_trip() {
    let dir = std::env::temp_dir().join(format!("gentle-ws-test-{}", std::process::id()));
    let path = dir.join("nested").join("workspaces.json");
    let state = sample();
    save_to(&path, &state).unwrap();
    assert_eq!(load_from(&path), Some(state));
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(load_from(&path), None);
    let _ = std::fs::remove_dir_all(dir);
}

/// A newer build saved a pane kind this one doesn't know (as the History
/// pane was for older builds): the file still loads with every workspace,
/// and only that pane goes.
#[test]
fn unknown_pane_kinds_drop_only_those_panes() {
    let dir = std::env::temp_dir().join(format!("gentle-ws-unknown-{}", std::process::id()));
    let path = dir.join("workspaces.json");
    std::fs::create_dir_all(&dir).unwrap();
    let mut json = serde_json::to_value(sample()).unwrap();
    // Workspace 1: the file pane (second of the inner split) becomes unknown.
    json["workspaces"][1]["tabs"][0]["layout"]["split"]["second"]["split"]["second"]["leaf"]["kind"] =
        "from_a_newer_build".into();
    // Workspace 0: its only tab is unknown and goes; a second tab stays.
    json["workspaces"][0]["tabs"][0]["layout"]["leaf"]["kind"] = "from_a_newer_build".into();
    let kept = serde_json::to_value(&sample().workspaces[0].tabs[0]).unwrap();
    json["workspaces"][0]["tabs"]
        .as_array_mut()
        .unwrap()
        .push(kept);
    json["workspaces"][0]["active_tab"] = 1.into();
    std::fs::write(&path, json.to_string()).unwrap();

    let state = load_from(&path).expect("loads despite the unknown panes");
    assert_eq!(state.workspaces.len(), 2);
    assert_eq!(state.workspaces[0].tabs.len(), 1);
    assert_eq!(state.workspaces[0].active_tab, 0);
    let tab = &state.workspaces[1].tabs[0];
    let kinds: Vec<PaneKind> = tab.layout.leaves().iter().map(|p| p.kind).collect();
    assert_eq!(kinds, [PaneKind::Agent, PaneKind::Terminal]);
    // The focused pane was the dropped one: focus falls back to the first.
    assert_eq!(tab.focused, 0);
    assert!(!dir.join("workspaces.json.unreadable").exists());
    let _ = std::fs::remove_dir_all(dir);
}

/// A file that doesn't read at all is kept aside before the next save
/// replaces it.
#[test]
fn an_unreadable_file_is_copied_aside() {
    let dir = std::env::temp_dir().join(format!("gentle-ws-unreadable-{}", std::process::id()));
    let path = dir.join("workspaces.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&path, "{ not json").unwrap();
    assert_eq!(load_from(&path), None);
    assert_eq!(
        std::fs::read_to_string(dir.join("workspaces.json.unreadable")).unwrap(),
        "{ not json"
    );
    let _ = std::fs::remove_dir_all(dir);
}
