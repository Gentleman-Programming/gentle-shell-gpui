use super::{
    Category, CloseOverlay, DevicePanel, GoToTab, Music, NextTab, Notices, OpenPalette,
    OpenWorkspaces, Overlay, PaneFactories, PaneItem, PaneKind, PaneTree, PrevTab, SavedPane,
    SavedTab, Space, Tab, TabLayout, TabState, TogglePaneMode, Workspace, WorkspaceEvent,
};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    AppContext, Context, Entity, Focusable, IntoElement, KeyBinding, Render, SharedString, Task,
    Window, WindowOptions,
};
use gpui_kit::{TestAppContext, VisualTestContext};
use std::cell::RefCell;
use std::{collections::HashMap, path::PathBuf, rc::Rc};

#[derive(Default)]
pub(crate) struct Trace {
    pub(crate) built: RefCell<Vec<PathBuf>>,
    pub(crate) roots: RefCell<Vec<PathBuf>>,
}

struct ProbePane(Entity<InputState>);
impl Render for ProbePane {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Input::new(&self.0)
    }
}

fn saved(id: usize, kind: PaneKind, cwd: PathBuf) -> Tab {
    Tab {
        id,
        pinned: false,
        state: TabState::Unbuilt(SavedTab {
            layout: PaneTree::Leaf(SavedPane {
                kind,
                cwd,
                path: None,
                session: None,
            }),
            pinned: false,
            focused: 0,
        }),
    }
}

pub(crate) struct TestRoot(PathBuf);

impl TestRoot {
    pub(crate) fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn fixture<'a>(
    cx: &'a mut TestAppContext,
    tag: &str,
    cold: bool,
) -> (
    Entity<Workspace>,
    &'a mut VisualTestContext,
    TestRoot,
    Rc<Trace>,
) {
    let root = std::env::temp_dir().join(format!("t8-{tag}-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let root = TestRoot(root);
    let alpha = root.join("Alpha");
    let beta = root.join("Beta");
    for dir in [alpha.join("twin"), beta.join("twin"), beta.join("saved")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let trace = Rc::new(Trace::default());
    let record = trace.clone();
    cx.update(|cx| {
        gpui_kit::init(cx);
        // Seed only built-in styles; production init discovers user themes and launchers.
        cx.set_global(crate::shared::themes::Themes {
            catalog: crate::shared::themes::builtin_themes(),
            active: 0,
            glass: crate::shared::themes::Glass {
                transparency: 0.0,
                ..crate::shared::themes::Glass::DEFAULT
            },
            interface_size: gentle_core::interface_size::DEFAULT,
            fonts: Default::default(),
        });
        crate::features::browser::init(cx);
        crate::features::hydration::init(cx);
        cx.set_reduce_motion(true);
        cx.bind_keys([
            KeyBinding::new("ctrl-space w", GoToTab, None),
            KeyBinding::new("ctrl-space shift-w", OpenWorkspaces, None),
            KeyBinding::new("ctrl-space space", TogglePaneMode, None),
            KeyBinding::new("ctrl-space n", NextTab, None),
            KeyBinding::new("ctrl-tab", NextTab, None),
            KeyBinding::new("ctrl-shift-tab", PrevTab, None),
            KeyBinding::new(
                if cfg!(target_os = "macos") {
                    "cmd-k"
                } else {
                    "ctrl-k"
                },
                OpenPalette,
                None,
            ),
            KeyBinding::new(
                if cfg!(target_os = "macos") {
                    "ctrl-cmd-k"
                } else {
                    "ctrl-alt-k"
                },
                OpenWorkspaces,
                None,
            ),
            KeyBinding::new("escape", CloseOverlay, Some("GentleWorkspace")),
        ]);
        cx.set_global(PaneFactories {
            terminal: Some(Rc::new(move |path, window, cx| {
                record.built.borrow_mut().push(path.clone());
                let input = cx.new(|cx| InputState::new(window, cx));
                let focus = input.focus_handle(cx);
                let title: SharedString =
                    format!("Terminal · {}", path.file_name().unwrap().to_string_lossy()).into();
                PaneItem {
                    kind: PaneKind::Terminal,
                    view: cx.new(|_| ProbePane(input)).into(),
                    focus,
                    title: Rc::new(move |_| title.clone()),
                    unsaved: None,
                }
            })),
            ..Default::default()
        });
    });
    let (window, host) = cx
        .update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| {
                    let mut source = Space::new("Alpha".into(), alpha.clone());
                    source.tabs = vec![
                        saved(11, PaneKind::Terminal, alpha.join("twin")),
                        saved(
                            12,
                            if cold {
                                PaneKind::Agent
                            } else {
                                PaneKind::Terminal
                            },
                            alpha.clone(),
                        ),
                    ];
                    let mut target = Space::new("Beta".into(), beta.clone());
                    target.tabs = vec![
                        saved(21, PaneKind::Terminal, beta.join("twin")),
                        saved(
                            22,
                            if cold {
                                PaneKind::Agent
                            } else {
                                PaneKind::Terminal
                            },
                            beta.join("saved"),
                        ),
                    ];
                    target.active = 1;
                    let mut workspace = Workspace {
                        spaces: vec![source, target],
                        current: 0,
                        panes: HashMap::new(),
                        next_id: 30,
                        overlay: None,
                        web_shown: Default::default(),
                        sidebar: None,
                        sidebar_visible: false,
                        tab_layout: TabLayout::Horizontal,
                        tabs_sidebar_motion: Default::default(),
                        files_sidebar_motion: Default::default(),
                        sidebar_compact: false,
                        tabs_sidebar_hidden: false,
                        tabs_sidebar_width: gentle_core::workspaces::SIDEBAR_DEFAULT_WIDTH,
                        files_sidebar_width: gentle_core::workspaces::SIDEBAR_DEFAULT_WIDTH,
                        badge_picker: None,
                        open_menu: None,
                        workspace_rename: None,
                        tab_selection: None,
                        tab_search: cx.new(|cx| InputState::new(window, cx)),
                        save_task: Some(Task::ready(())),
                        _title_poll: Task::ready(()),
                        _lifecycle: Vec::new(),
                        notices: cx.new(|_| Notices::default()),
                        renaming: None,
                        surge: None,
                        surge_runs: 0,
                        _surge_end: None,
                        stages: Vec::new(),
                        next_stage: 1,
                        opened_outside: Default::default(),
                        canvas_outside: Default::default(),
                        browser_dock_drag: None,
                        browser_dock_room: Default::default(),
                        resume_welcome: None,
                        drop_hint: None,
                        pane_mode: false,
                        pane_mode_focus: cx.focus_handle(),
                        which_key: None,
                        leader: None,
                        tab_list_focus: cx.focus_handle(),
                        tab_list_highlight: None,
                        tab_list_return_focus: None,
                        closed: Vec::new(),
                        music: cx.new(|cx| Music::new(window, cx)),
                        devices: cx.new(DevicePanel::new),
                        focus: cx.focus_handle(),
                    };
                    workspace.activate(0, window, cx);
                    workspace
                })
            })
        })
        .unwrap();
    let cx = VisualTestContext::from_window(window, cx).into_mut();
    cx.update(|window, _| window.activate_window());
    let record = trace.clone();
    cx.update(|_, cx| {
        cx.subscribe(&host, move |_, event, _| {
            if let WorkspaceEvent::RootChanged(root) = event {
                record.roots.borrow_mut().push(root.clone());
            }
        })
        .detach()
    });
    cx.run_until_parked();
    (host, cx, root, trace)
}

pub(crate) fn active(host: &Entity<Workspace>, cx: &mut VisualTestContext) -> (usize, usize) {
    cx.update(|_, cx| {
        let w = host.read(cx);
        (w.current, w.tab().unwrap().id)
    })
}

/// Whether the folder explorer (`⌘O`, and the folder picker of "Create
/// workspace") is the open screen.
pub(crate) fn explorer_open(host: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| matches!(host.read(cx).overlay, Some((Overlay::Explorer(_), _))))
}

pub(crate) fn category(host: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<Category> {
    cx.update(|_, cx| match &host.read(cx).overlay {
        Some((Overlay::Palette(p), _)) => Some(p.read(cx).category()),
        _ => None,
    })
}

#[gpui::test]
fn t8_verifier_existing_scope(cx: &mut TestAppContext) {
    let (host, cx, root, trace) = fixture(cx, "scope", false);
    cx.simulate_keystrokes("ctrl-tab ctrl-shift-tab");
    assert_eq!(active(&host, cx), (0, 11));
    assert_eq!(
        *trace.built.borrow(),
        [root.join("Alpha/twin"), root.join("Alpha")]
    );
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    });
    cx.simulate_input("Beta");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "ctrl-cmd-k"
    } else {
        "ctrl-alt-k"
    });
    cx.run_until_parked();
    assert_eq!(category(&host, cx), Some(Category::Workspaces));
    cx.simulate_keystrokes("enter");
    assert_eq!(active(&host, cx), (1, 22));
    assert_eq!(category(&host, cx), None);
    assert_eq!(*trace.roots.borrow(), [root.join("Beta")]);
    assert_eq!(
        *trace.built.borrow(),
        [
            root.join("Alpha/twin"),
            root.join("Alpha"),
            root.join("Beta/saved")
        ]
    );
    cx.simulate_keystrokes("ctrl-tab ctrl-shift-tab");
    assert_eq!(active(&host, cx), (1, 22));
    assert_eq!(*trace.roots.borrow(), [root.join("Beta")]);
    assert_eq!(
        *trace.built.borrow(),
        [
            root.join("Alpha/twin"),
            root.join("Alpha"),
            root.join("Beta/saved"),
            root.join("Beta/twin")
        ]
    );
}

#[gpui::test]
fn t8_verifier_actual_palette(cx: &mut TestAppContext) {
    let (host, cx, root, trace) = fixture(cx, "palette", true);
    let original_focus = cx.update(|window, cx| window.focused(cx));
    assert!(original_focus.is_some());
    // The fixture suppresses the disk writer; check its exact serialized input.
    let snapshot = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| serde_json::to_vec(&host.read(cx).snapshot(cx)).unwrap())
    };
    let before = snapshot(cx);
    cx.simulate_keystrokes("ctrl-space w");
    assert_eq!(category(&host, cx), Some(Category::Tabs));
    cx.simulate_input("no matching tab at all");
    cx.simulate_keystrokes("enter");
    assert_eq!(category(&host, cx), Some(Category::Tabs));
    assert_eq!(active(&host, cx), (0, 11));
    assert_eq!(snapshot(cx), before);
    cx.simulate_keystrokes("escape");
    assert_eq!(category(&host, cx), None);
    assert_eq!(cx.update(|window, cx| window.focused(cx)), original_focus);
    cx.simulate_keystrokes("ctrl-space w");
    cx.simulate_input("twin");
    cx.simulate_keystrokes("down");
    assert_eq!(active(&host, cx), (0, 11));
    assert_eq!(*trace.built.borrow(), [root.join("Alpha/twin")]);
    assert!(trace.roots.borrow().is_empty());
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        assert_eq!(window.focused(cx), original_focus);
        let w = host.read(cx);
        assert!(w.sessions().is_empty());
    });
    assert_eq!(snapshot(cx), before);
    cx.simulate_keystrokes("ctrl-space w");
    cx.simulate_input("twin");
    cx.simulate_keystrokes("down enter");
    assert_eq!(active(&host, cx), (1, 21));
    assert_eq!(category(&host, cx), None);
    assert_eq!(
        *trace.built.borrow(),
        [root.join("Alpha/twin"), root.join("Beta/twin")]
    );
    assert_eq!(*trace.roots.borrow(), [root.join("Beta")]);
    cx.update(|window, cx| {
        let w = host.read(cx);
        assert!(!w.spaces[1].tabs[1].is_built());
        assert!(w.sessions().is_empty());
        assert!(
            w.panes[&w.focused_pane().unwrap()]
                .item
                .focus
                .is_focused(window)
        );
    });
}

#[gpui::test]
fn t8_verifier_palette_commands_and_folder_cancel(cx: &mut TestAppContext) {
    let (host, cx, _root, trace) = fixture(cx, "commands", false);
    let palette_key = if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    };
    cx.simulate_keystrokes(palette_key);
    assert_eq!(category(&host, cx), Some(Category::All));
    cx.simulate_input("Go to tab");
    cx.simulate_keystrokes("enter");
    assert_eq!(category(&host, cx), Some(Category::Tabs));
    cx.simulate_input("Beta");
    cx.simulate_keystrokes("enter");
    assert_eq!(active(&host, cx), (1, 21));
    cx.simulate_keystrokes("ctrl-space shift-w");
    assert_eq!(category(&host, cx), Some(Category::Workspaces));
    cx.simulate_input("Alpha");
    cx.simulate_keystrokes("enter");
    assert_eq!(active(&host, cx), (0, 11));
    cx.simulate_keystrokes(palette_key);
    cx.simulate_input("Create workspace");
    cx.simulate_keystrokes("enter");
    // The command picks the folder in the folder explorer now, not natively.
    assert!(!cx.did_prompt_for_paths());
    assert!(explorer_open(&host, cx));
    cx.simulate_keystrokes("escape");
    assert_eq!(active(&host, cx), (0, 11));
    assert_eq!(cx.update(|_, cx| host.read(cx).spaces.len()), 2);
    assert_eq!(trace.roots.borrow().len(), 2);
}

#[gpui::test]
fn t8_verifier_pane_mode_cancel_keeps_keyboard_scope(cx: &mut TestAppContext) {
    let (host, cx, _root, _) = fixture(cx, "pane-mode", false);
    cx.simulate_input("unsent draft");
    cx.simulate_keystrokes("ctrl-space space");
    assert!(cx.update(|_, cx| host.read(cx).pane_mode));
    cx.simulate_keystrokes("ctrl-space w");
    assert_eq!(category(&host, cx), Some(Category::Tabs));
    cx.simulate_keystrokes("escape");
    assert_eq!(category(&host, cx), None);
    cx.update(|window, cx| {
        let w = host.read(cx);
        let pane = &w.panes[&w.focused_pane().unwrap()];
        assert!(!w.pane_mode);
        assert!(pane.item.focus.is_focused(window));
        let view = pane.item.view.clone().downcast::<ProbePane>().unwrap();
        assert_eq!(view.read(cx).0.read(cx).value().as_str(), "unsent draft");
    });
    cx.simulate_keystrokes("ctrl-space n");
    assert_eq!(active(&host, cx), (0, 12));
}

#[gpui::test]
fn t8_verifier_switch_workspace_command_clears_search(cx: &mut TestAppContext) {
    let (host, cx, _root, _) = fixture(cx, "scope-switch-command", false);
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    });
    cx.simulate_input("Switch workspace");
    cx.simulate_keystrokes("enter");
    assert_eq!(category(&host, cx), Some(Category::Workspaces));
    cx.simulate_keystrokes("down enter");
    assert_eq!(
        (active(&host, cx), cx.did_prompt_for_paths()),
        ((1, 22), false)
    );
}

#[gpui::test]
fn t13_leader_waits_for_a_choice_from_any_focus(cx: &mut TestAppContext) {
    let (host, cx, _root, _trace) = fixture(cx, "leader-any-focus", false);
    // The interceptor `Workspace::new` installs on the window.
    let _leader =
        cx.update(|_, cx| super::composer_leader::watch_leader_keys(host.downgrade(), cx));
    cx.simulate_input("unsent draft");
    let pane_focus = cx.update(|window, cx| {
        let w = host.read(cx);
        let pane = &w.panes[&w.focused_pane().unwrap()];
        assert!(pane.item.focus.is_focused(window));
        pane.item.focus.clone()
    });

    // A lone leader pressed in the terminal stays pending: the guide is still
    // shown after the window's own pending-input timeout would have fired.
    cx.simulate_keystrokes("ctrl-space");
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    cx.run_until_parked();
    cx.update(|window, cx| {
        let w = host.read(cx);
        let (_, selection) = w.leader.as_ref().expect("the guide is still shown");
        assert!(selection.hint(cx).is_some());
        assert!(!window.has_pending_keystrokes());
    });

    // Esc gives focus back, with the terminal where it was.
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        let w = host.read(cx);
        assert!(w.leader.is_none());
        assert!(pane_focus.is_focused(window));
        let pane = &w.panes[&w.focused_pane().unwrap()];
        let view = pane.item.view.clone().downcast::<ProbePane>().unwrap();
        assert_eq!(view.read(cx).0.read(cx).value().as_str(), "unsent draft");
    });

    // A key pressed later resolves in the context it was started from.
    cx.simulate_keystrokes("ctrl-space");
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    cx.run_until_parked();
    cx.simulate_keystrokes("n");
    assert_eq!(active(&host, cx), (0, 12));
    assert!(cx.update(|_, cx| host.read(cx).leader.is_none()));
}
