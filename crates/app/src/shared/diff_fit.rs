//! Diff rows that span their pane from the first frame.
//!
//! `gpui_kit`'s `Diff` widens its rows to the body size it records while
//! painting and asks for a frame to apply it, but GPUI drops a request made
//! mid-frame: rows stayed as wide as their text until something else redrew
//! (a hover). [`DiffFit`] draws the diff's owner again once after the diff
//! appears or changes size.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::diff::Diff;
use gpui_kit::{Div, EntityId, ParentElement as _, Pixels, Size, Styled as _, canvas, div};

/// The size a diff had in the last frame, kept by the view that draws it.
#[derive(Default)]
pub struct DiffFit(Rc<Cell<Size<Pixels>>>);

impl DiffFit {
    /// A new diff in the same place hasn't measured itself yet: draw again
    /// after its first frame even if its size holds.
    pub fn reset(&self) {
        self.0.set(Size::default());
    }

    /// `diff`, filling its box, with `view` (the entity whose render
    /// returns it) drawn again once after the diff's size changes.
    pub fn fill(&self, diff: Diff, view: EntityId) -> Div {
        let size = self.0.clone();
        div()
            .relative()
            .size_full()
            .child(diff.size_full())
            // After the diff in prepaint order, so it has recorded this
            // frame's size.
            .child(
                canvas(
                    move |bounds, _, cx| {
                        if size.replace(bounds.size) != bounds.size {
                            cx.defer(move |cx| cx.notify(view));
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::diff::{DiffFile, DiffState};
    use gpui_kit::{AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, Window};

    use super::*;

    const PATCH: &str = "diff --git a/a.txt b/a.txt
index 0a1b2c3..4d5e6f7 100644
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-one
+two
";

    /// A view drawing one diff, with or without a [`DiffFit`].
    struct Owner {
        state: Entity<DiffState>,
        fit: Option<DiffFit>,
        renders: usize,
    }

    impl Render for Owner {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.renders += 1;
            let diff = Diff::new(&self.state);
            match &self.fit {
                Some(fit) => fit.fill(diff, cx.entity_id()),
                None => div().size_full().child(diff.size_full()),
            }
        }
    }

    fn files() -> Vec<DiffFile> {
        DiffFile::parse(PATCH).expect("parses")
    }

    /// How many times the owner rendered: once the diff showed, after a
    /// redraw at the same size, and after new files in the same place.
    fn renders(cx: &mut TestAppContext, fit: bool) -> [usize; 3] {
        cx.update(gpui_kit::init);
        let (owner, cx) = cx.add_window_view(|_, cx| Owner {
            state: cx.new(|cx| DiffState::new(files(), cx)),
            fit: fit.then(DiffFit::default),
            renders: 0,
        });
        let mut counts = [0; 3];
        cx.run_until_parked();
        counts[0] = cx.update(|_, cx| owner.read(cx).renders);
        cx.update(|_, cx| owner.update(cx, |_, cx| cx.notify()));
        cx.run_until_parked();
        counts[1] = cx.update(|_, cx| owner.read(cx).renders);
        cx.update(|_, cx| {
            owner.update(cx, |owner, cx| {
                if let Some(fit) = &owner.fit {
                    fit.reset();
                }
                owner
                    .state
                    .update(cx, |state, cx| state.set_files(files(), cx));
            })
        });
        cx.run_until_parked();
        counts[2] = cx.update(|_, cx| owner.read(cx).renders);
        counts
    }

    /// Without the fit, the frame the diff asks for while painting never
    /// comes; with it, the owner draws once more after the diff first shows
    /// and after new files, and not again while the size holds.
    #[gpui::test]
    fn the_owner_draws_again_once_after_the_diff_measures_itself(
        plain: &mut TestAppContext,
        fitted: &mut TestAppContext,
    ) {
        let [shown, same, new_files] = renders(plain, false);
        assert_eq!(renders(fitted, true), [shown + 1, same + 1, new_files + 2]);
    }
}
