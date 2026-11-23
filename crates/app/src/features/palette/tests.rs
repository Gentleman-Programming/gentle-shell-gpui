use gpui_kit::{Context, Entity, TestAppContext, VisualTestContext, Window};

use super::{
    Activity, Category, PaletteAction, PaletteInput, PaletteView, TabLayout, WorkspaceRow,
};

fn update(
    view: &Entity<PaletteView>,
    cx: &mut VisualTestContext,
    f: impl FnOnce(&mut PaletteView, &mut Window, &mut Context<PaletteView>),
) {
    cx.update(|window, cx| view.update(cx, |view, cx| f(view, window, cx)));
}

fn actions(view: &Entity<PaletteView>, cx: &mut VisualTestContext) -> Vec<PaletteAction> {
    view.read_with(cx, |view, _| {
        view.rows.iter().map(|row| row.action.clone()).collect()
    })
}

#[gpui::test]
fn recent_workspaces_keep_original_indices_and_search_behavior(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    // A fresh, empty root keeps file and branch discovery away from user data.
    let root = std::env::temp_dir().join(format!(
        "palette-recents-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let workspaces: Vec<_> = (0..8)
        .map(|index| WorkspaceRow {
            index,
            name: format!("Workspace {index}"),
            root: root.clone(),
            tabs: 1,
            current: index == 3,
            activity: Activity::default(),
            last_active: if index < 2 { 0 } else { 10 },
        })
        .collect();
    let (view, cx) = cx.add_window_view(|window, cx| {
        PaletteView::new(
            PaletteInput {
                commands: Vec::new(),
                workspaces: workspaces.clone(),
                root: root.clone(),
                category: Category::Workspaces,
                tab_layout: TabLayout::default(),
            },
            window,
            cx,
        )
    });
    let switches = |indices: &[usize]| {
        indices
            .iter()
            .copied()
            .map(PaletteAction::SwitchWorkspace)
            .collect::<Vec<_>>()
    };
    let folder = PaletteAction::CreateWorkspace {
        name: String::new(),
        pick_folder: true,
    };
    let mut expected = switches(&[3, 2, 4, 5, 6, 7, 0, 1]);
    expected.push(folder.clone());
    assert_eq!(actions(&view, cx), expected);

    update(&view, cx, |view, window, cx| {
        view.set_category(Category::All, window, cx);
    });
    let all = actions(&view, cx);
    assert_eq!(&all[..6], &switches(&[3, 2, 4, 5, 6, 7]));
    assert_eq!(
        all.iter()
            .filter(|action| matches!(action, PaletteAction::SwitchWorkspace(_)))
            .count(),
        6
    );
    assert!(
        !all.iter()
            .any(|action| matches!(action, PaletteAction::CreateWorkspace { .. }))
    );

    update(&view, cx, |view, window, cx| {
        // Programmatic set_value doesn't deliver the user-input Change event.
        view.query
            .update(cx, |query, cx| query.set_value("Workspace", window, cx));
        view.set_category(Category::Workspaces, window, cx);
    });
    cx.run_until_parked();
    let mut filtered = vec![PaletteAction::CreateWorkspace {
        name: "Workspace".into(),
        pick_folder: false,
    }];
    filtered.extend(switches(&[0, 1, 2, 3, 4, 5, 6, 7]));
    filtered.push(PaletteAction::CreateWorkspace {
        name: "Workspace".into(),
        pick_folder: true,
    });
    assert_eq!(actions(&view, cx), filtered);

    update(&view, cx, |view, window, cx| {
        view.query
            .update(cx, |query, cx| query.set_value("workspace 1", window, cx));
        view.set_category(Category::Workspaces, window, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        actions(&view, cx),
        vec![PaletteAction::SwitchWorkspace(1), folder]
    );

    update(&view, cx, |view, window, cx| {
        view.query
            .update(cx, |query, cx| query.set_value("", window, cx));
        let mut updated = workspaces.clone();
        updated[0].last_active = 20;
        view.set_workspaces(updated, cx);
    });
    cx.run_until_parked();
    assert_eq!(actions(&view, cx)[0], PaletteAction::SwitchWorkspace(0));
    assert_eq!(
        view.read_with(cx, |view, _| {
            view.workspaces.iter().map(|w| w.index).collect::<Vec<_>>()
        }),
        (0..8).collect::<Vec<_>>()
    );
    std::fs::remove_dir_all(root).unwrap();
}
