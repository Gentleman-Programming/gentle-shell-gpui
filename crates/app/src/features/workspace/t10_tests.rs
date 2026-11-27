use super::t8_verifier::{active, category, explorer_open, fixture};
use gpui_kit::TestAppContext;

use super::{Category, PaneKind, PaneTree, SavedPane, SavedTab, Tab, TabState};

fn saved_terminal_tab(id: usize, cwd: std::path::PathBuf) -> Tab {
    Tab {
        id,
        pinned: false,
        state: TabState::Unbuilt(SavedTab {
            layout: PaneTree::Leaf(SavedPane {
                kind: PaneKind::Terminal,
                cwd,
                path: None,
                session: None,
            }),
            pinned: false,
            focused: 0,
        }),
    }
}

#[gpui::test]
fn t10_create_workspace_names_after_folder(cx: &mut TestAppContext) {
    let (host, cx, root, _trace) = fixture(cx, "create-name", false);
    let gamma = root.join("Gamma");
    std::fs::create_dir(&gamma).unwrap();

    cx.update(|window, cx| {
        host.update(cx, |w, cx| {
            let index = w.push_space(None, gamma.clone());
            w.spaces[index].tabs = vec![saved_terminal_tab(31, gamma.clone())];
            w.switch_workspace(index, window, cx);
        });
    });
    cx.run_until_parked();

    let created = cx.update(|_, cx| {
        host.read(cx)
            .spaces
            .iter()
            .find(|s| s.root == gamma)
            .map(|s| s.name.clone())
    });
    assert_eq!(created, Some("Gamma".into()));
    assert_eq!(cx.update(|_, cx| host.read(cx).spaces.len()), 3);
}

#[gpui::test]
fn t10_create_workspace_cancel_creates_nothing(cx: &mut TestAppContext) {
    let (host, cx, _root, trace) = fixture(cx, "create-cancel", false);
    let palette_key = if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    };

    cx.simulate_keystrokes(palette_key);
    cx.simulate_input("Create workspace");
    cx.simulate_keystrokes("enter");
    // The folder is chosen in the app's own explorer (⌘O), not a native dialog.
    assert!(!cx.did_prompt_for_paths());
    assert!(explorer_open(&host, cx));
    cx.simulate_keystrokes("escape");
    assert!(!explorer_open(&host, cx));

    assert_eq!(cx.update(|_, cx| host.read(cx).spaces.len()), 2);
    assert!(trace.roots.borrow().is_empty());
}

#[gpui::test]
fn t10_create_workspace_chooses_the_folder_in_the_explorer(cx: &mut TestAppContext) {
    let (host, cx, root, trace) = fixture(cx, "create-explorer", false);
    let palette_key = if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    };

    cx.simulate_keystrokes(palette_key);
    cx.simulate_input("Create workspace");
    cx.simulate_keystrokes("enter");
    assert!(explorer_open(&host, cx));
    // It opens in the focused pane's folder (Alpha/twin); `-` twice reaches
    // the folder both workspaces live in. The explorer reads each folder in
    // the background, so wait for every listing before typing on.
    cx.run_until_parked();
    cx.simulate_keystrokes("-");
    cx.run_until_parked();
    cx.simulate_keystrokes("-");
    cx.run_until_parked();
    cx.simulate_input("Beta");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    });

    // The chosen folder's workspace is shown (it existed already: no duplicate).
    assert!(!explorer_open(&host, cx));
    assert_eq!(active(&host, cx), (1, 22));
    assert_eq!(cx.update(|_, cx| host.read(cx).spaces.len()), 2);
    assert_eq!(*trace.roots.borrow(), [root.join("Beta")]);
}

#[gpui::test]
fn t10_delete_workspace_picker_deletes_after_confirmation(cx: &mut TestAppContext) {
    let (host, cx, _root, _trace) = fixture(cx, "delete-picker", false);
    let palette_key = if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    };

    cx.simulate_keystrokes(palette_key);
    cx.simulate_input("Delete workspace");
    cx.simulate_keystrokes("enter");
    assert_eq!(category(&host, cx), Some(Category::Workspaces));

    // The first workspace row is selected; Enter deletes it.
    cx.simulate_keystrokes("enter");
    // Confirm the alert dialog.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(cx.update(|_, cx| host.read(cx).spaces.len()), 1);
}

#[gpui::test]
fn t10_delete_current_workspace_switches_to_last_active(cx: &mut TestAppContext) {
    let (host, cx, _root, _trace) = fixture(cx, "delete-current", false);
    assert_eq!(active(&host, cx).0, 0);

    cx.update(|window, cx| {
        host.update(cx, |w, cx| w.delete_workspace(0, window, cx));
    });

    cx.update(|_, cx| {
        let w = host.read(cx);
        assert_eq!(w.spaces.len(), 1);
        assert_eq!(w.current, 0);
        assert_eq!(w.spaces[0].name, "Beta");
    });
}

#[gpui::test]
fn t10_delete_last_workspace_is_ignored(cx: &mut TestAppContext) {
    let (host, cx, _root, _trace) = fixture(cx, "delete-last", false);

    cx.update(|window, cx| {
        host.update(cx, |w, cx| w.delete_workspace(1, window, cx));
    });
    cx.update(|window, cx| {
        host.update(cx, |w, cx| w.delete_workspace(0, window, cx));
    });

    assert_eq!(cx.update(|_, cx| host.read(cx).spaces.len()), 1);
}
