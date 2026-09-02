// SPDX-License-Identifier: GPL-3.0-only

//! The window: a navigation rail on the left, one page per tab on the right.

use std::collections::HashMap;
use std::time::Duration;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::prelude::*;
use cosmic::widget::{self, nav_bar, table};

use crate::fl;
use crate::performance::{self, Performance};
use crate::probe::Probe;
use crate::processes::{Column, Row};
use crate::users::{self, Names, Range};
use crate::windows::{self, Event, Window};

/// How often the list is rebuilt. The reference calls this speed "normal", and a release
/// walk of 1604 processes costs 31 ms, so a second leaves the machine alone between ticks.
const TICK: Duration = Duration::from_secs(1);

/// How much of the machine the process list shows.
///
/// One control instead of two checkboxes: the levels nest, and a pair of tick boxes has a
/// state that means nothing - "only mine" already excludes every kernel thread, because they
/// all belong to root.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Scope {
    /// Processes of the person at the machine.
    #[default]
    Mine,
    /// Every process with a command line of its own, whoever owns it.
    Users,
    /// Kernel threads as well.
    All,
}

impl Scope {
    const ALL: [Self; 3] = [Self::Mine, Self::Users, Self::All];

    fn label(self) -> String {
        match self {
            Self::Mine => fl!("scope-mine"),
            Self::Users => fl!("scope-users"),
            Self::All => fl!("scope-all"),
        }
    }
}

/// The tabs, in the order the rail lists them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Processes,
    Performance,
    Users,
}

#[derive(Clone, Debug)]
pub enum Message {
    /// Time to walk `/proc` again.
    Tick,
    /// Something arrived from the bus: a new window list, or a second launch asking this
    /// copy to show itself.
    Bus(Event),
    /// The row clicked, together with the generation of the model it was clicked in.
    ///
    /// An `Entity` is a slotmap key, and a fresh model hands out the very same keys in the
    /// same order. A click delivered in the same batch as a tick would otherwise resolve
    /// against the model built by that tick and name a different process - which, one press
    /// of "End task" later, is the wrong process killed.
    Select(u64, table::Entity),
    /// The search box changed.
    Search(String),
    SearchClear,
    /// The scope control changed, by position in [`Scope::ALL`].
    Scope(usize),
    Sort(Column),
    EndTask { force: bool },
    DismissError,
    Performance(performance::Message),
}

/// What a selection points at, in terms that outlive a rebuild of the model.
///
/// The start time tells a recycled pid apart from the process that held it before.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Selection {
    pid: u32,
    starttime: u64,
}

impl Selection {
    fn of(row: &Row, probe: &Probe) -> Option<Self> {
        let pid = *row.pids.first()?;
        Some(Self {
            pid,
            starttime: probe.get(pid).map_or(0, |p| p.starttime),
        })
    }

    fn matches(&self, row: &Row) -> bool {
        row.pids.as_slice() == [self.pid]
    }
}

pub struct App {
    core: Core,
    nav: nav_bar::Model,
    probe: Probe,
    windows: Vec<Window>,
    /// The person at the machine. Everything else is a system process.
    uid: u32,
    /// Quick search over the name, the command line and the pid.
    search: String,
    scope: Scope,
    /// Built once: the dropdown wants a slice that outlives the view.
    scope_labels: Vec<String>,
    /// The column the list is ordered by, and whether the natural order is reversed. Natural
    /// means largest first for a number and alphabetical for a name, which is the order each
    /// column is actually read in.
    sort: (Column, bool),
    table: table::SingleSelectModel<Row, Column>,
    /// Bumped by every rebuild. A message minted against an older model is dropped.
    generation: u64,
    /// Which row is selected, by an identity that survives a rebuild.
    selection: Option<Selection>,
    /// What "end task" would act on, refreshed from whichever row the selection matched.
    selected: Vec<u32>,
    error: Option<String>,
    performance: Performance,
    /// Read once: an account added while the window is open is vanishingly rare, and
    /// re-reading `/etc/passwd` every second is not the way to catch it.
    user_names: Names,
    /// Where regular accounts begin, out of `/etc/login.defs`.
    user_range: Range,
    users: table::SingleSelectModel<Row, Column>,
}

impl App {
    fn page(&self) -> Page {
        self.nav
            .active_data::<Page>()
            .copied()
            .unwrap_or(Page::Processes)
    }

    /// Rebuild the whole list from the last walk.
    ///
    /// Rebuilt rather than patched: a tick can add, remove and reorder rows at once, and the
    /// selection is carried over by pid, which survives all three.
    fn rebuild(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        let (column, reversed) = self.sort;

        let needle = self.search.trim().to_lowercase();
        let scope = self.scope;
        let uid = self.uid;

        let mut rows: Vec<Row> = self
            .probe
            .processes()
            .filter(|p| match scope {
                // A kernel thread belongs to root, so this covers them too.
                Scope::Mine => p.uid == uid,
                Scope::Users => !p.is_kernel_thread(),
                Scope::All => true,
            })
            .filter(|p| {
                needle.is_empty()
                    || p.name.to_lowercase().contains(&needle)
                    || p.cmdline.to_lowercase().contains(&needle)
                    || p.pid.to_string().contains(&needle)
            })
            .map(Row::process)
            .collect();
        rows.sort_by(|a, b| {
            let order = crate::processes::compare(a, b, column);
            if reversed { order.reverse() } else { order }
        });

        let mut model = table::Model::new(Column::ALL.to_vec());
        for row in rows {
            let _ = model.insert(row);
        }

        // The arrow only draws the direction; the order above is already built.
        let ascending = match column {
            Column::Name => !reversed,
            _ => reversed,
        };
        model.set_sort(Some((column, ascending)));

        // Restored by pid and start time, not by position: the list is rebuilt from scratch
        // every second and re-sorted while it is.
        self.selected.clear();
        if let Some(want) = self.selection {
            let again = model
                .iter()
                .find(|entity| model.item(*entity).is_some_and(|r| want.matches(r)));
            match again {
                Some(entity) => {
                    self.selected = model.item(entity).map(|r| r.pids.clone()).unwrap_or_default();
                    model.activate(entity);
                }
                // The process is gone. Forget it rather than leave a selection naming a pid
                // that something else may now hold.
                None => self.selection = None,
            }
        }

        self.table = model;
    }

    /// The selected row.
    ///
    /// Read out of the item map. `active_data` reads the model's `storage` map, which only
    /// `EntityMut::data()` ever fills, and the rows here are inserted without it - so that
    /// call returned `None` for every row and left the header button dead.
    fn rebuild_users(&mut self) {
        let mut model = table::Model::new(Column::ALL.to_vec());
        for row in users::rows(&self.probe, &self.user_names, self.user_range, self.uid) {
            let _ = model.insert(row);
        }
        model.set_sort(Some((Column::Data, false)));
        self.users = model;
    }

    fn processes_view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let generation = self.generation;

        let search = widget::text_input::search_input(fl!("search"), &self.search)
            .width(Length::Fixed(260.0))
            .on_input(Message::Search)
            .on_clear(Message::SearchClear);

        let scope = widget::dropdown(
            &self.scope_labels,
            Scope::ALL.iter().position(|s| *s == self.scope),
            Message::Scope,
        );

        let bar = widget::row::with_capacity(2)
            .spacing(spacing.space_xs)
            .align_y(cosmic::iced::Alignment::Center)
            .push(search)
            .push(scope);

        let table = widget::table(&self.table)
            .on_item_left_click(move |entity| Message::Select(generation, entity))
            // Right click selects too, so the menu below always acts on the row it was
            // opened over rather than on whatever was selected before.
            .on_item_right_click(move |entity| Message::Select(generation, entity))
            .on_category_left_click(Message::Sort)
            .item_context(|row: &Row| {
                row.is_task().then(|| {
                    widget::menu::items(
                        &HashMap::new(),
                        vec![
                            widget::menu::Item::Button(fl!("end-task"), None, Action::EndTask),
                            widget::menu::Item::Button(fl!("kill-task"), None, Action::KillTask),
                        ],
                    )
                })
            })
            .apply(widget::scrollable)
            .width(Length::Fill)
            .height(Length::Fill);

        let mut column = widget::column::with_capacity(3)
            .spacing(spacing.space_xxs)
            .push(bar)
            .push(table);
        if let Some(error) = &self.error {
            column = column.push(widget::warning(error.clone()).on_close(Message::DismissError));
        }
        column.padding(spacing.space_xxs).into()
    }
}

impl cosmic::Application for App {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    /// Matches the basename of `res/fun.wmde.taskmgr.desktop` and its `Icon=`: the toolkit
    /// hands this out as the Wayland app id, and that is the only thing tying the running
    /// window to its entry and icon.
    const APP_ID: &'static str = "fun.wmde.taskmgr";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let mut nav = nav_bar::Model::default();
        nav.insert()
            .text(fl!("processes"))
            .icon(widget::icon::from_name("view-list-symbolic"))
            .data(Page::Processes);
        nav.insert()
            .text(fl!("performance"))
            .icon(widget::icon::from_name("utilities-system-monitor-symbolic"))
            .data(Page::Performance);
        nav.insert()
            .text(fl!("users"))
            .icon(widget::icon::from_name("system-users-symbolic"))
            .data(Page::Users);
        nav.activate_position(0);

        let mut app = App {
            core,
            nav,
            probe: Probe::new(),
            windows: Vec::new(),
            uid: rustix::process::getuid().as_raw(),
            search: String::new(),
            scope: Scope::default(),
            scope_labels: Scope::ALL.iter().map(|s| s.label()).collect(),
            // Memory first: it is the column the list is opened for.
            sort: (Column::Data, false),
            table: table::Model::new(Column::ALL.to_vec()),
            generation: 0,
            selection: None,
            selected: Vec::new(),
            error: None,
            performance: Performance::new(),
            user_names: Names::load(),
            user_range: Range::load(),
            users: table::Model::new(Column::ALL.to_vec()),
        };
        // The first walk has nothing to take a delta against, so every rate reads zero until
        // the second one a second later. Taking it here means the window is not empty when
        // it opens.
        app.probe.sample();
        app.rebuild();
        app.rebuild_users();
        app.performance.tick();

        let title = app.set_window_title(fl!("app-title"), app.core.main_window_id().unwrap());
        (app, title)
    }

    fn nav_model(&self) -> Option<&nav_bar::Model> {
        Some(&self.nav)
    }

    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<Self::Message> {
        self.nav.activate(id);
        Task::none()
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Tick => {
                self.probe.sample();
                self.rebuild();
                self.rebuild_users();
                self.performance.tick();
            }
            // Kept, not used: the flat list needs nothing from it, and the folding in
            // `group` needs all of it the moment it is wired back up.
            Message::Bus(Event::Windows(windows)) => self.windows = windows,
            Message::Bus(Event::Present) => {
                if let Some(id) = self.core.main_window_id() {
                    return cosmic::iced::window::gain_focus(id);
                }
            }
            Message::Select(generation, entity) => {
                if generation != self.generation {
                    return Task::none();
                }
                self.table.activate(entity);
                let (selection, pids) = match self.table.item(entity) {
                    Some(row) => (Selection::of(row, &self.probe), row.pids.clone()),
                    None => (None, Vec::new()),
                };
                self.selection = selection;
                self.selected = pids;
            }
            Message::Search(text) => {
                self.search = text;
                self.rebuild();
            }
            Message::SearchClear => {
                self.search.clear();
                self.rebuild();
            }
            Message::Scope(index) => {
                self.scope = Scope::ALL.get(index).copied().unwrap_or_default();
                self.rebuild();
            }
            Message::Sort(column) => {
                // The same column again turns the order around; a different one starts from
                // its natural order, which is largest first for a number.
                self.sort = if self.sort.0 == column {
                    (column, !self.sort.1)
                } else {
                    (column, false)
                };
                self.rebuild();
            }
            Message::EndTask { force } => {
                if !self.selected.is_empty() {
                    self.error = crate::processes::end(&self.selected, force).err();
                }
            }
            Message::DismissError => self.error = None,
            Message::Performance(message) => {
                match message {
                    performance::Message::Select(device) => self.performance.device = device,
                    performance::Message::TogglePerCore => {
                        self.performance.per_core = !self.performance.per_core;
                    }
                }
                // The plots are cached by size, and every large plot is the same size, so
                // without this the new device is drawn with the old device's geometry until
                // the next tick a second later.
                self.performance.invalidate();
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        match self.page() {
            Page::Processes => self.processes_view(),
            Page::Performance => self.performance.view().map(Message::Performance),
            Page::Users => widget::table(&self.users)
                .apply(widget::scrollable)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            cosmic::iced::time::every(TICK).map(|_| Message::Tick),
            Subscription::run(windows::subscription).map(Message::Bus),
        ])
    }
}

/// What the context menu over a row offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    /// `SIGTERM`: the application gets to close its windows and save.
    EndTask,
    /// `SIGKILL`: it does not.
    KillTask,
}

impl widget::menu::Action for Action {
    type Message = Message;

    fn message(&self) -> Self::Message {
        match self {
            Self::EndTask => Message::EndTask { force: false },
            Self::KillTask => Message::EndTask { force: true },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The levels nest: what "mine" shows is a subset of the user processes, which is a
    /// subset of everything. A pair of checkboxes would allow a state that says otherwise.
    #[test]
    fn the_scopes_nest() {
        let mut probe = Probe::new();
        probe.sample();
        let uid = rustix::process::getuid().as_raw();

        let count = |scope: Scope| {
            probe
                .processes()
                .filter(|p| match scope {
                    Scope::Mine => p.uid == uid,
                    Scope::Users => !p.is_kernel_thread(),
                    Scope::All => true,
                })
                .count()
        };

        // Containment, and nothing about what the machine happens to be running: a build
        // container has its own pid namespace and shows no kernel threads at all, so
        // demanding that the last step be a strict one is a claim about the environment.
        assert!(count(Scope::Mine) > 0, "the test process is one of ours");
        assert!(count(Scope::Mine) <= count(Scope::Users));
        assert!(count(Scope::Users) <= count(Scope::All));
    }

    /// Every scope has a label in the catalogue, and no two read the same.
    #[test]
    fn every_scope_is_named() {
        let labels: Vec<String> = Scope::ALL.iter().map(|s| s.label()).collect();
        assert!(labels.iter().all(|l| !l.is_empty()));
        let mut sorted = labels.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len(), "two scopes share a label");
    }
}

pub fn run() -> cosmic::iced::Result {
    let settings = Settings::default().size_limits(
        cosmic::iced::Limits::NONE
            .min_width(360.0)
            .min_height(180.0),
    );
    cosmic::app::run::<App>(settings, ())
}
