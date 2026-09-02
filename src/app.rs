// SPDX-License-Identifier: GPL-3.0-only

//! The window: a navigation rail on the left, one page per tab on the right.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::prelude::*;
use cosmic::widget::{self, nav_bar, table};

use crate::fl;
use crate::group::{self, Group, Section};
use crate::probe::Probe;
use crate::performance::{self, Performance};
use crate::processes::{Column, Kind, Row};
use crate::users::{self, Names};
use crate::windows::{self, Window};

/// How often the list is rebuilt. The reference calls this speed "normal", and a release
/// walk of 1604 processes costs 31 ms, so a second leaves the machine alone between ticks.
const TICK: Duration = Duration::from_secs(1);

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
    /// The compositor's window list changed.
    Windows(Vec<Window>),
    /// The row clicked, together with the generation of the model it was clicked in.
    ///
    /// An `Entity` is a slotmap key, and a fresh model hands out the very same keys in the
    /// same order. A click delivered in the same batch as a tick would otherwise resolve
    /// against the model built by that tick and name a different process - which, one press
    /// of "End task" later, is the wrong process killed.
    Select(u64, table::Entity),
    /// Unfold or fold a group, in the generation it was clicked in.
    Toggle(u64, table::Entity),
    Sort(Column),
    EndTask { force: bool },
    DismissError,
    Performance(performance::Message),
}

/// What a selection points at, in terms that outlive a rebuild of the model.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Selection {
    /// A group, by the key the grouping layer assigns it.
    Group(group::Key),
    /// One process inside an unfolded group. The start time tells a recycled pid apart from
    /// the process that held it before.
    Process(u32, u64),
}

impl Selection {
    fn of(row: &Row, probe: &Probe) -> Option<Self> {
        if let Some(key) = &row.key {
            return Some(Self::Group(key.clone()));
        }
        let pid = *row.pids.first()?;
        Some(Self::Process(pid, probe.get(pid).map_or(0, |p| p.starttime)))
    }

    fn matches(&self, row: &Row) -> bool {
        match self {
            Self::Group(key) => row.key.as_ref() == Some(key),
            // A child row carries exactly one pid; the start time was checked when the
            // selection was made and a mismatch simply drops the highlight next tick.
            Self::Process(pid, _) => row.key.is_none() && row.pids.as_slice() == [*pid],
        }
    }
}

pub struct App {
    core: Core,
    nav: nav_bar::Model,
    probe: Probe,
    windows: Vec<Window>,
    /// The person at the machine. Everything else is a system process.
    uid: u32,
    expanded: HashSet<group::Key>,
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
        let groups = group::build(&self.probe, &self.windows, self.uid);
        let (column, reversed) = self.sort;
        let mut model = table::Model::new(Column::ALL.to_vec());

        for section in [Section::Applications, Section::Background, Section::System] {
            let in_section: Vec<&Group> =
                groups.iter().filter(|g| g.section == section).collect();
            if in_section.is_empty() {
                continue;
            }

            let mut rows: Vec<(Row, &Group)> = in_section
                .iter()
                .map(|g| (Row::group(g, self.expanded.contains(&g.key)), *g))
                .collect();
            rows.sort_by(|a, b| {
                let order = crate::processes::compare(&a.0, &b.0, column);
                if reversed { order.reverse() } else { order }
            });

            let _ = model.insert(Row::section(section, in_section.len()));
            for (row, g) in rows {
                let expanded = matches!(row.kind, Kind::Group { expanded: true, .. });
                let _ = model.insert(row);
                if expanded {
                    for pid in &g.pids {
                        if let Some(p) = self.probe.get(*pid) {
                            let _ = model.insert(Row::child(p));
                        }
                    }
                }
            }
        }

        // The arrow only draws the direction; the order above is already built.
        let ascending = match column {
            Column::Name => !reversed,
            _ => reversed,
        };
        model.set_sort(Some((column, ascending)));

        // Restored by identity, not by the pid list: a group's members come and go, and
        // matching on the exact vector loses the highlight the moment a helper exits.
        self.selected.clear();
        if let Some(want) = &self.selection {
            let again = model
                .iter()
                .find(|entity| model.item(*entity).is_some_and(|r| want.matches(r)));
            match again {
                Some(entity) => {
                    model.activate(entity);
                    if let Some(row) = model.item(entity) {
                        self.selected = row.pids.clone();
                    }
                }
                // The row is gone. Forget it rather than leave a selection that names
                // nothing and an "End task" button that would act on a stale pid.
                None => self.selection = None,
            }
        }

        self.table = model;
    }

    fn rebuild_users(&mut self) {
        let mut model = table::Model::new(Column::ALL.to_vec());
        for row in users::rows(&self.probe, &self.user_names) {
            let _ = model.insert(row);
        }
        model.set_sort(Some((Column::Data, false)));
        self.users = model;
    }

    /// The selected row.
    ///
    /// Read out of the item map. `active_data` reads the model's `storage` map, which only
    /// `EntityMut::data()` ever fills, and the rows here are inserted without it - so that
    /// call returned `None` for every row and left the header button dead.
    fn active_row(&self) -> Option<&Row> {
        self.table.item(self.table.active())
    }

    fn processes_view(&self) -> Element<'_, Message> {
        let generation = self.generation;
        let table = widget::table(&self.table)
            .on_item_left_click(move |entity| Message::Select(generation, entity))
            // Right click selects too, so the menu below always acts on the row it was
            // opened over rather than on whatever was selected before.
            .on_item_right_click(move |entity| Message::Select(generation, entity))
            .on_item_double_click(move |entity| Message::Toggle(generation, entity))
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

        let mut column = widget::column::with_capacity(2).push(table);
        if let Some(error) = &self.error {
            column = column.push(
                widget::warning(error.clone()).on_close(Message::DismissError),
            );
        }
        column.into()
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
            expanded: HashSet::new(),
            // Memory first: it is the column the list is opened for.
            sort: (Column::Data, false),
            table: table::Model::new(Column::ALL.to_vec()),
            generation: 0,
            selection: None,
            selected: Vec::new(),
            error: None,
            performance: Performance::new(),
            user_names: Names::load(),
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

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        if self.page() != Page::Processes {
            return Vec::new();
        }
        let armed = self.active_row().is_some_and(Row::is_task);
        vec![
            widget::button::standard(fl!("end-task"))
                .on_press_maybe(armed.then_some(Message::EndTask { force: false }))
                .into(),
        ]
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Tick => {
                self.probe.sample();
                self.rebuild();
                self.rebuild_users();
                self.performance.tick();
            }
            Message::Windows(windows) => {
                self.windows = windows;
                self.rebuild();
            }
            Message::Select(generation, entity) => {
                if generation != self.generation {
                    return Task::none();
                }
                self.table.activate(entity);
                let (selection, pids) = match self.table.item(entity) {
                    Some(row) if row.is_task() => {
                        (Selection::of(row, &self.probe), row.pids.clone())
                    }
                    // A section heading is not selectable: it has nothing to end.
                    _ => (None, Vec::new()),
                };
                self.selection = selection;
                self.selected = pids;
            }
            Message::Toggle(generation, entity) => {
                if generation != self.generation {
                    return Task::none();
                }
                // Only a group of more than one has anything folded away.
                let key = self.table.item(entity).and_then(|row| match row.kind {
                    Kind::Group {
                        expandable: true, ..
                    } => row.key.clone(),
                    _ => None,
                });
                if let Some(key) = key {
                    if !self.expanded.remove(&key) {
                        self.expanded.insert(key);
                    }
                    self.rebuild();
                }
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
            Subscription::run(windows::subscription).map(Message::Windows),
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

pub fn run() -> cosmic::iced::Result {
    let settings = Settings::default().size_limits(
        cosmic::iced::Limits::NONE
            .min_width(360.0)
            .min_height(180.0),
    );
    cosmic::app::run::<App>(settings, ())
}
