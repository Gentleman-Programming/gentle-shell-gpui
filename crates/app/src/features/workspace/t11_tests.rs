use super::sidebars;
use super::{
    CloseOverlay, FocusTabList, PaneFactories, PaneItem, PaneKind, SavedPane, SavedTab, Space, Tab,
    TabLayout, TabState, Workspace,
};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, Focusable, IntoElement, KeyBinding, Render, SharedString,
    Task, WindowOptions,
};
use gpui_kit::{TestAppContext, VisualTestContext};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Default)]
struct Trace {
    built: RefCell<Vec<PathBuf>>,
}

struct ProbePane(Entity<InputState>);
impl Render for ProbePane {
    fn render(&mut self, _: &mut gpui_kit::Window, _: &mut Context<Self>) -> impl IntoElement {
        Input::new(&self.0)
    }
}

fn saved(id: usize, kind: PaneKind, cwd: PathBuf) -> Tab {
    Tab {
        id,
        pinned: false,
        state: TabState::Unbuilt(SavedTab {
            layout: crate::shared::pane::PaneTree::Leaf(SavedPane {
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

struct TestRoot(PathBuf);

impl TestRoot {
    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture<'a>(
    cx: &'a mut TestAppContext,
    tag: &str,
) -> (
    Entity<Workspace>,
    &'a mut VisualTestContext,
    TestRoot,
    Rc<Trace>,
) {
    let root = std::env::temp_dir().join(format!("t11-{tag}-{}", std::process::id()));
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
            KeyBinding::new("ctrl-space w", FocusTabList, None),
            KeyBinding::new("escape", CloseOverlay, Some("GentleWorkspace")),
        ]);
        sidebars::bind_keys(cx);
        cx.set_global(PaneFactories {
            terminal: Some(Rc::new(
                move |path, window: &mut gpui_kit::Window, cx: &mut gpui_kit::App| {
                    record.built.borrow_mut().push(path.clone());
                    let input = cx.new(|cx| InputState::new(window, cx));
                    let focus = input.focus_handle(cx);
                    let title: SharedString =
                        format!("Terminal · {}", path.file_name().unwrap().to_string_lossy())
                            .into();
                    PaneItem {
                        kind: PaneKind::Terminal,
                        view: cx.new(|_| ProbePane(input)).into(),
                        focus,
                        title: Rc::new(move |_| title.clone()),
                        unsaved: None,
                    }
                },
            )),
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
                        saved(12, PaneKind::Terminal, alpha.clone()),
                    ];
                    source.active = 0;

                    let mut target = Space::new("Beta".into(), beta.clone());
                    target.tabs = vec![
                        saved(21, PaneKind::Terminal, beta.join("twin")),
                        saved(22, PaneKind::Terminal, beta.join("saved")),
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
                        tab_layout: TabLayout::Vertical,
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
                        notices: cx.new(|_| crate::features::notices::Notices::default()),
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
                        music: cx.new(|cx| crate::features::music::Music::new(window, cx)),
                        devices: cx.new(crate::features::devices::DevicePanel::new),
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
    cx.run_until_parked();
    (host, cx, root, trace)
}

fn active(host: &Entity<Workspace>, cx: &mut VisualTestContext) -> (usize, usize) {
    cx.update(|_, cx| {
        let w = host.read(cx);
        (w.current, w.tab().unwrap().id)
    })
}

fn highlight(host: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<(usize, usize)> {
    cx.update(|_, cx| host.read(cx).tab_list_highlight)
}

fn list_is_focused(host: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| host.read(cx).tab_list_focus.is_focused(window))
}

#[gpui::test]
fn t11_focus_reopens_a_collapsed_workspace_section(cx: &mut TestAppContext) {
    let (host, cx, root, trace) = fixture(cx, "collapsed");
    assert_eq!(active(&host, cx), (0, 11));
    // Main's model folds a workspace's sessions in place. Fold both.
    cx.update(|_, cx| {
        host.update(cx, |w, _| {
            w.spaces[0].sidebar_expanded = false;
            w.spaces[1].sidebar_expanded = false;
        })
    });
    assert!(!cx.update(|_, cx| host.read(cx).sidebar_space_open(0)));

    cx.simulate_keystrokes("ctrl-space w");
    cx.run_until_parked();
    assert!(list_is_focused(&host, cx));

    // The shown workspace opens so its highlighted tab is on screen; the other
    // stays folded and focusing the list activates nothing.
    assert!(cx.update(|_, cx| host.read(cx).sidebar_space_open(0)));
    assert!(!cx.update(|_, cx| host.read(cx).sidebar_space_open(1)));
    assert_eq!(highlight(&host, cx), Some((0, 0)));
    assert_eq!(active(&host, cx), (0, 11));
    assert_eq!(*trace.built.borrow(), [root.join("Alpha/twin")]);

    // The reopened section is navigable: one step down stays in Alpha.
    cx.simulate_keystrokes("down");
    assert_eq!(highlight(&host, cx), Some((0, 1)));
    assert_eq!(active(&host, cx), (0, 11));
}

#[gpui::test]
fn t11_keyboard_tab_sidebar_navigation(cx: &mut TestAppContext) {
    let (host, cx, root, trace) = fixture(cx, "nav");
    let original_focus = cx.update(|window, cx| window.focused(cx));

    assert_eq!(active(&host, cx), (0, 11));
    assert_eq!(*trace.built.borrow(), [root.join("Alpha/twin")]);

    // Focus the sidebar: highlight starts on the active tab, nothing is built.
    cx.simulate_keystrokes("ctrl-space w");
    cx.run_until_parked();
    assert!(list_is_focused(&host, cx));
    assert_eq!(highlight(&host, cx), Some((0, 0)));
    assert_eq!(active(&host, cx), (0, 11));
    assert_eq!(*trace.built.borrow(), [root.join("Alpha/twin")]);

    // Down moves the highlight; workspace headers are skipped and no tab activates.
    cx.simulate_keystrokes("down");
    assert_eq!(highlight(&host, cx), Some((0, 1)));
    assert_eq!(active(&host, cx), (0, 11));

    cx.simulate_keystrokes("down");
    assert_eq!(highlight(&host, cx), Some((1, 0)));
    assert_eq!(active(&host, cx), (0, 11));
    assert!(!cx.update(|_, cx| host.read(cx).spaces[1].tabs[0].is_built()));
    assert!(!cx.update(|_, cx| host.read(cx).spaces[1].tabs[1].is_built()));

    // Esc returns focus to where it was and changes nothing.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(cx.update(|window, cx| window.focused(cx)), original_focus);
    assert_eq!(active(&host, cx), (0, 11));
    assert_eq!(highlight(&host, cx), None);

    // Focus again and activate the Beta "twin" tab.
    cx.simulate_keystrokes("ctrl-space w");
    cx.run_until_parked();
    assert!(list_is_focused(&host, cx));
    cx.simulate_keystrokes("down down");
    assert_eq!(highlight(&host, cx), Some((1, 0)));

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(active(&host, cx), (1, 21));
    assert!(!cx.update(|_, cx| host.read(cx).spaces[1].tabs[1].is_built()));
    assert_eq!(
        *trace.built.borrow(),
        [root.join("Alpha/twin"), root.join("Beta/twin")]
    );

    // j/k also move the highlight in the list.
    cx.simulate_keystrokes("ctrl-space w");
    cx.run_until_parked();
    assert_eq!(highlight(&host, cx), Some((1, 0)));
    cx.simulate_keystrokes("j");
    assert_eq!(highlight(&host, cx), Some((1, 1)));
    cx.simulate_keystrokes("k");
    assert_eq!(highlight(&host, cx), Some((1, 0)));
}
