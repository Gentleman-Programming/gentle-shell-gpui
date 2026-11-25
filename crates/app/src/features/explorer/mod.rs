//! Folder explorer in the style of oil.nvim: one directory at a time, driven
//! from the keyboard, to pick where Gentle Shell opens. ↵ enters a folder,
//! `-` (or ←, or ⌫ with nothing typed) goes to the parent, `~` goes home and
//! typing filters. ⌘↵ / Ctrl+↵ opens an agent tab in the selected folder (or
//! the one shown), ⌥↵ / Alt+↵ a terminal; both show the workspace rooted
//! there, creating it, like `gsd <folder>`. Picking a folder for something
//! else ([`Purpose::Choose`]), ⌘↵ hands it back. Where agents run on a
//! Gentle Shell server, it browses the server's folders (through its API),
//! home being the server's.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use gentle_core::explorer::{Entry, visible};

use crate::features::backend;
use crate::shared::ui;

/// Directories with more entries than this list only the first ones.
const MAX_ENTRIES: usize = 5_000;
/// How many of a server folder's folders are checked for a `.git` at once
/// (each is a request).
const REPO_CHECKS: usize = 8;

/// What picking a folder in the explorer is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Show the workspace rooted there (creating it), with an agent or a
    /// terminal tab.
    Open,
    /// Hand the folder back ([`ExplorerEvent::Choose`]); the button says
    /// what for.
    Choose(&'static str),
}

pub enum ExplorerEvent {
    /// Open an agent tab in this folder's workspace.
    OpenAgent(PathBuf),
    /// Open a terminal tab in this folder's workspace.
    OpenTerminal(PathBuf),
    /// The folder picked for [`Purpose::Choose`].
    Choose(PathBuf),
    OpenFile(PathBuf),
    Close,
}

impl EventEmitter<ExplorerEvent> for ExplorerView {}

pub struct ExplorerView {
    purpose: Purpose,
    dir: PathBuf,
    entries: Vec<Entry>,
    /// Folders holding a `.git`, marked as repositories.
    repos: HashSet<String>,
    /// `entries` matching the filter, as shown.
    shown: Vec<Entry>,
    selected: usize,
    filter: Entity<InputState>,
    /// Why `dir` couldn't be listed.
    error: Option<String>,
    /// A folder is being read (a server's answers over the network).
    loading: bool,
    scroll: ScrollHandle,
    /// The folder being read; reading another one replaces it.
    _load: Task<()>,
    _subscription: Subscription,
}

impl ExplorerView {
    pub fn new(
        dir: PathBuf,
        purpose: Purpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter…"));
        filter.update(cx, |state, cx| state.focus(window, cx));
        let subscription = cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.refilter(None, cx);
            }
        });
        let mut this = Self {
            purpose,
            dir: PathBuf::new(),
            entries: Vec::new(),
            repos: HashSet::new(),
            shown: Vec::new(),
            selected: 0,
            filter,
            error: None,
            loading: false,
            scroll: ScrollHandle::new(),
            _load: Task::ready(()),
            _subscription: subscription,
        };
        this.load(dir, None, window, cx);
        this
    }

    /// Show `dir`, read off the UI thread, selecting the entry named
    /// `select` when it is there.
    fn load(
        &mut self,
        dir: PathBuf,
        select: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = true;
        cx.notify();
        self._load = cx.spawn_in(window, async move |this, cx| {
            let listed = dir.clone();
            let read = cx
                .background_executor()
                .spawn(async move { read_dir(&listed) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let (entries, repos, error) = match read {
                    Ok((entries, repos)) => (entries, repos, None),
                    Err(error) => (Vec::new(), HashSet::new(), Some(error.to_string())),
                };
                this.loading = false;
                this.dir = dir;
                this.entries = entries;
                this.repos = repos;
                this.error = error;
                this.filter
                    .update(cx, |state, cx| state.set_value("", window, cx));
                this.refilter(select, cx);
            });
        });
    }

    fn refilter(&mut self, select: Option<String>, cx: &mut Context<Self>) {
        let filter = self.filter.read(cx).value();
        self.shown = visible(&self.entries, &filter);
        self.selected = select
            .and_then(|name| self.shown.iter().position(|e| e.name == name))
            .unwrap_or(0);
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.shown.get(self.selected)
    }

    /// The folder an "open here" acts on: the selected folder, else the one shown.
    fn target(&self) -> PathBuf {
        match self.selected_entry() {
            Some(entry) if entry.is_dir => self.dir.join(&entry.name),
            _ => self.dir.clone(),
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.shown.is_empty() {
            return;
        }
        let len = self.shown.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn enter(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.shown.get(index).cloned() else {
            return;
        };
        let path = self.dir.join(&entry.name);
        if entry.is_dir {
            self.load(path, None, window, cx);
        } else {
            cx.emit(ExplorerEvent::OpenFile(path));
        }
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let from = self
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        self.load(parent, from, window, cx);
    }

    /// The home folder: the server's where the folder shown is one's (asked
    /// off the UI thread the first time), else this computer's.
    fn go_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.dir.clone();
        self._load = cx.spawn_in(window, async move |this, cx| {
            let home = cx
                .background_executor()
                .spawn(async move { home_of(&dir) })
                .await;
            if let Some(home) = home {
                let _ = this.update_in(cx, |this, window, cx| this.load(home, None, window, cx));
            }
        });
    }

    /// ⌘↵: an agent tab in the target folder, or the folder handed back
    /// when picking one.
    fn open_agent(&mut self, cx: &mut Context<Self>) {
        cx.emit(match self.purpose {
            Purpose::Open => ExplorerEvent::OpenAgent(self.target()),
            Purpose::Choose(_) => ExplorerEvent::Choose(self.target()),
        });
    }

    /// ⌥↵: a terminal tab in the target folder (nothing when picking one).
    fn open_terminal(&mut self, cx: &mut Context<Self>) {
        if self.purpose == Purpose::Open {
            cx.emit(ExplorerEvent::OpenTerminal(self.target()));
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;
        let secondary = if cfg!(target_os = "macos") {
            m.platform
        } else {
            m.control
        };
        let typed = !self.filter.read(cx).value().is_empty();
        match keystroke.key.as_str() {
            "escape" if typed => {
                self.filter
                    .update(cx, |state, cx| state.set_value("", window, cx));
                self.refilter(None, cx);
            }
            "escape" => cx.emit(ExplorerEvent::Close),
            "enter" if secondary => self.open_agent(cx),
            "enter" if m.alt => self.open_terminal(cx),
            "enter" => self.enter(self.selected, window, cx),
            "up" => self.move_selection(-1, cx),
            "down" => self.move_selection(1, cx),
            "p" if m.control => self.move_selection(-1, cx),
            "n" if m.control => self.move_selection(1, cx),
            "right" if !typed && self.selected_entry().is_some_and(|e| e.is_dir) => {
                self.enter(self.selected, window, cx)
            }
            "left" | "backspace" if !typed && !m.modified() => self.go_up(window, cx),
            // oil's keys, while nothing is typed (then they filter as usual).
            "-" if !typed && !m.modified() => self.go_up(window, cx),
            _ if !typed && keystroke.key_char.as_deref() == Some("~") => self.go_home(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The path shown as clickable segments: click one to go there.
    fn render_crumbs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut crumbs = Vec::new();
        let mut path = PathBuf::new();
        for component in self.dir.components() {
            path.push(component);
            let label = component.as_os_str().to_string_lossy().into_owned();
            crumbs.push((label, path.clone()));
        }
        let last = crumbs.len().saturating_sub(1);
        h_flex()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .gap_0p5()
            .text_size(px(13.))
            .children(crumbs.into_iter().enumerate().map(|(i, (label, path))| {
                let current = i == last;
                h_flex()
                    .flex_none()
                    .gap_0p5()
                    .when(i > 0, |d| {
                        d.child(
                            Icon::new(IconName::ChevronRight)
                                .xsmall()
                                .text_color(ui::muted()),
                        )
                    })
                    .child(
                        div()
                            .id(("explorer-crumb", i))
                            .px_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(if current { ui::fg() } else { ui::muted() })
                            .hover(|s| s.bg(ui::surface_hover()).text_color(ui::fg()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.load(path.clone(), None, window, cx)
                            }))
                            .child(label),
                    )
            }))
    }

    fn render_row(&self, i: usize, entry: &Entry, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = i == self.selected;
        let repo = entry.is_dir && self.repos.contains(&entry.name);
        let icon = match (entry.is_dir, repo) {
            (true, true) => IconName::FolderGit,
            (true, false) => IconName::Folder,
            _ => IconName::File,
        };
        h_flex()
            .id(("explorer-row", i))
            .h(px(32.))
            .flex_none()
            .px_3()
            .gap_2()
            .rounded_lg()
            .cursor_pointer()
            .when(selected, |d| d.bg(ui::raised()))
            .when(!selected, |d| d.hover(|s| s.bg(ui::surface_hover())))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.selected = i;
                if event.click_count() > 1 {
                    this.enter(i, window, cx);
                }
                cx.notify();
            }))
            .child(Icon::new(icon).small().text_color(if entry.is_dir {
                ui::accent()
            } else {
                ui::muted()
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(14.))
                    .text_color(if entry.is_dir { ui::fg() } else { ui::muted() })
                    .child(if entry.is_dir {
                        format!("{}/", entry.name)
                    } else {
                        entry.name.clone()
                    }),
            )
            .when(repo, |d| {
                d.child(div().text_xs().text_color(ui::muted()).child("repo"))
            })
    }
}

/// `dir`'s entries (at most [`MAX_ENTRIES`]) and which folders are git
/// repos. A server's folders are checked a few at a time, hidden ones
/// (shown only when the filter asks) not at all.
fn read_dir(dir: &Path) -> std::io::Result<(Vec<Entry>, HashSet<String>)> {
    // Links to folders are folders, so they can be entered.
    let entries: Vec<Entry> = backend::fs::read_dir(dir)?
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|e| Entry {
            name: e.name,
            is_dir: e.is_dir,
        })
        .collect();
    let folders = entries.iter().filter(|e| e.is_dir).map(|e| e.name.as_str());
    let repos = match backend::fs::at(dir)? {
        backend::fs::Fs::Local => folders
            .filter(|name| dir.join(name).join(".git").exists())
            .map(str::to_string)
            .collect(),
        backend::fs::Fs::Server(client) => {
            let folders: Vec<&str> = folders.filter(|name| !name.starts_with('.')).collect();
            server_repos(&client, dir, &folders)
        }
    };
    Ok((entries, repos))
}

/// Which of `folders` (in the server folder `dir`) hold a `.git`, asked
/// [`REPO_CHECKS`] at a time.
fn server_repos(
    client: &pi_rpc::serve::ServeClient,
    dir: &Path,
    folders: &[&str],
) -> HashSet<String> {
    let per_check = folders.len().div_ceil(REPO_CHECKS).max(1);
    std::thread::scope(|scope| {
        let checks: Vec<_> = folders
            .chunks(per_check)
            .map(|names| {
                scope.spawn(move || {
                    names
                        .iter()
                        .filter(|name| {
                            let git = dir.join(name).join(".git");
                            client.fs_stat(&backend::fs::server_path(&git)).is_ok()
                        })
                        .map(|name| name.to_string())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        checks
            .into_iter()
            .flat_map(|check| check.join().unwrap_or_default())
            .collect()
    })
}

/// The home folder where `dir` is: the server's when work there runs on
/// one (asked the first time), else this computer's. Blocking.
fn home_of(dir: &Path) -> Option<PathBuf> {
    let choice = backend::for_tab(dir);
    if choice.is_server() {
        backend::server::home(&choice).map(PathBuf::from)
    } else {
        dirs::home_dir()
    }
}

fn hint(keys: impl IntoIterator<Item = impl IntoElement>, label: &'static str) -> Div {
    h_flex().gap_1().children(keys).child(
        div()
            .pl_0p5()
            .text_xs()
            .text_color(ui::muted())
            .child(label),
    )
}

impl Render for ExplorerView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = v_flex()
            .id("explorer-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p_2();
        for (i, entry) in self.shown.iter().enumerate() {
            list = list.child(self.render_row(i, entry, cx));
        }
        if self.shown.is_empty() {
            let text = match &self.error {
                _ if self.loading => "Loading…".into(),
                Some(error) => format!("Can't read this folder: {error}"),
                None if self.entries.is_empty() => "Empty folder".into(),
                None => "No matches".into(),
            };
            list = list.child(
                div()
                    .py_8()
                    .text_center()
                    .text_sm()
                    .text_color(ui::muted())
                    .child(text),
            );
        }
        let (secondary, alt) = if cfg!(target_os = "macos") {
            ("⌘", "⌥")
        } else {
            ("Ctrl", "Alt")
        };

        let panel = v_flex()
            .id("explorer")
            .w(px(680.))
            .h(px(560.))
            .max_h_full()
            .rounded_2xl()
            .border_1()
            .border_color(ui::border_strong())
            .bg(ui::screen_bg())
            .shadow_2xl()
            .overflow_hidden()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .h(px(48.))
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(ui::border())
                    .child(
                        Button::new("explorer-up")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::ArrowUp))
                            .tooltip("Parent folder (-)")
                            .on_click(cx.listener(|this, _, window, cx| this.go_up(window, cx))),
                    )
                    .child(self.render_crumbs(cx))
                    .child({
                        let (icon, label) = match self.purpose {
                            Purpose::Open => (IconName::Bot, "Open Gentle Shell here"),
                            Purpose::Choose(label) => (IconName::FolderOpen, label),
                        };
                        Button::new("explorer-open-agent")
                            .primary()
                            .small()
                            .icon(Icon::new(icon))
                            .label(label)
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.emit(match this.purpose {
                                    Purpose::Open => ExplorerEvent::OpenAgent(this.dir.clone()),
                                    Purpose::Choose(_) => ExplorerEvent::Choose(this.dir.clone()),
                                })
                            }))
                    }),
            )
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_none()
                    .px_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(ui::border())
                    .child(Icon::new(IconName::Search).text_color(ui::muted()))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(14.))
                            .child(Input::new(&self.filter).appearance(false)),
                    ),
            )
            .child(list)
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_none()
                    .px_4()
                    .gap_4()
                    .border_t_1()
                    .border_color(ui::border())
                    .child(hint([ui::kbd("↵")], "enter"))
                    .child(hint([ui::kbd("-")], "parent"))
                    .child(hint([ui::kbd("~")], "home"))
                    .child(hint(
                        [ui::key_button("explorer-agent", format!("{secondary} ↵"))
                            .on_click(cx.listener(|this, _, _, cx| this.open_agent(cx)))],
                        match self.purpose {
                            Purpose::Open => "Gentle Shell",
                            Purpose::Choose(label) => label,
                        },
                    ))
                    .when(self.purpose == Purpose::Open, |d| {
                        d.child(hint(
                            [ui::key_button("explorer-terminal", format!("{alt} ↵"))
                                .on_click(cx.listener(|this, _, _, cx| this.open_terminal(cx)))],
                            "terminal",
                        ))
                    })
                    .child(div().flex_1())
                    .child(hint(
                        [ui::key_button("explorer-close", "esc")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ExplorerEvent::Close)))],
                        "close",
                    )),
            );

        div()
            .id("explorer-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .p(px(32.))
            .occlude()
            .capture_key_down(cx.listener(Self::on_key_down))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ExplorerEvent::Close)))
            .child(ui::scrim("explorer-scrim"))
            .child(ui::pop_in(
                "explorer-in",
                div().flex().max_h_full().child(panel),
            ))
    }
}
