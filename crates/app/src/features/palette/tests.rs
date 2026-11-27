use gpui_kit::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription,
    TestAppContext, Window, div,
};

use super::{
    Category, IconName, PaletteAction, PaletteEvent, PaletteInput, PaletteView, TabLayout, TabRow,
};

struct Picker {
    palette: Option<Entity<PaletteView>>,
    events: Vec<PaletteEvent>,
    _subscription: Subscription,
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.palette.clone())
    }
}

fn exercise(cx: &mut TestAppContext, key: &str, expected: PaletteEvent) {
    // The picker only needs tab metadata; file and branch reads see a missing root.
    let root = std::env::temp_dir().join(format!("gentle-picker-missing-{}", std::process::id()));
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let (host, cx) = cx.add_window_view(|window, cx| {
        let palette = cx.new(|cx| {
            PaletteView::new(
                PaletteInput {
                    tabs: [(17, "Alpha"), (42, "Beta")]
                        .map(|(id, workspace)| TabRow {
                            id,
                            title: "Shared title".into(),
                            workspace: workspace.into(),
                            icon: IconName::Layers,
                            current: id == 17,
                        })
                        .into(),
                    workspaces: vec![],
                    commands: vec![],
                    root,
                    category: Category::Tabs,
                    tab_layout: TabLayout::Horizontal,
                },
                window,
                cx,
            )
        });
        let subscription = cx.subscribe(&palette, |host: &mut Picker, _, event, cx| {
            host.events.push(event.clone());
            host.palette = None;
            cx.notify();
        });
        Picker {
            palette: Some(palette),
            events: vec![],
            _subscription: subscription,
        }
    });
    cx.run_until_parked();
    cx.simulate_input("Shared title");
    host.read_with(cx, |host, cx| {
        let palette = host.palette.as_ref().unwrap().read(cx);
        assert_eq!(palette.selected, 0);
        assert_eq!(palette.rows.len(), 2);
        assert_eq!(palette.rows[0].meta, "Alpha");
        assert_eq!(palette.rows[1].meta, "Beta");
    });
    cx.simulate_keystrokes("down");
    host.read_with(cx, |host, cx| {
        let palette = host.palette.as_ref().unwrap().read(cx);
        assert_eq!(palette.selected, 1);
        assert_eq!(palette.rows[1].action, PaletteAction::ActivateGlobalTab(42));
        assert!(host.events.is_empty());
    });
    cx.simulate_keystrokes(key);
    host.read_with(cx, |host, _| {
        assert_eq!(host.events, [expected]);
        assert!(host.palette.is_none());
    });
}

#[gpui::test]
fn global_tab_picker_enter_emits_one_exact_target(cx: &mut TestAppContext) {
    exercise(
        cx,
        "enter",
        PaletteEvent::Run(PaletteAction::ActivateGlobalTab(42)),
    );
}

#[gpui::test]
fn global_tab_picker_escape_closes_without_activation(cx: &mut TestAppContext) {
    exercise(cx, "escape", PaletteEvent::Close);
}
