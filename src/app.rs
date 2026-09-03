// SPDX-License-Identifier: GPL-3.0-only

//! The window: a navigation rail on the left, one page per tab on the right.

use std::collections::HashMap;
use std::time::Duration;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::prelude::*;
use cosmic::widget::{self, segmented_button, table};

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
    /// A tab in the title bar was picked.
    Tab(segmented_button::Entity),
    Sort(Column),
    /// End the process the menu was opened over. Never the selected one: a right click
    /// dropped by the generation guard would otherwise leave the previous row armed, and the
    /// signal would go somewhere the user never pointed.
    EndTask { pid: u32, force: bool },
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

    /// The pid alone is not enough: a number is handed out again once the process holding it
    /// exits, and the selection would then land on a stranger.
    fn matches(&self, row: &Row, probe: &Probe) -> bool {
        row.pids.as_slice() == [self.pid]
            && probe.get(self.pid).map_or(0, |p| p.starttime) == self.starttime
    }
}

pub struct App {
    core: Core,
    tabs: segmented_button::SingleSelectModel,
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
        self.tabs
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
        // PID is the one number built smallest-first, like the reference; every other
        // number column is built largest-first. The arrow follows the order, not the column.
        let ascending = match column {
            Column::Name | Column::Pid => !reversed,
            _ => reversed,
        };
        model.set_sort(Some((column, ascending)));

        // Restored by pid and start time, not by position: the list is rebuilt from scratch
        // every second and re-sorted while it is.
        if let Some(want) = self.selection {
            let again = model
                .iter()
                .find(|entity| {
                    model
                        .item(*entity)
                        .is_some_and(|r| want.matches(r, &self.probe))
                });
            match again {
                Some(entity) => model.activate(entity),
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
    /// Hand the machine-wide figures to the column headings.
    fn publish_totals(&self) {
        let s = self.performance.last();
        crate::processes::set_totals(crate::processes::Totals {
            cpu: s.cpu.iter().sum(),
            memory: if s.mem_total > 0 {
                s.mem[0] as f32 / s.mem_total as f32
            } else {
                0.0
            },
            disk: s.disk[0] + s.disk[1],
        });
    }

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
            .width(Length::Fill)
            .on_input(Message::Search)
            .on_clear(Message::SearchClear);

        let scope = widget::dropdown(
            &self.scope_labels,
            Scope::ALL.iter().position(|s| *s == self.scope),
            Message::Scope,
        );

        // Fill on the row as well as on the field. A Shrink row resolves a Fill child
        // against its own intrinsic width, and the search box would keep the width of its
        // placeholder. The dropdown is Shrink by default and keeps the width it had; it
        // simply ends up against the right edge.
        let bar = widget::row::with_capacity(2)
            .spacing(spacing.space_xs)
            .align_y(cosmic::iced::Alignment::Center)
            .width(Length::Fill)
            .push(search)
            .push(scope);

        let table = widget::table(&self.table)
            .on_item_left_click(move |entity| Message::Select(generation, entity))
            // Right click selects too, so the menu below always acts on the row it was
            // opened over rather than on whatever was selected before.
            .on_item_right_click(move |entity| Message::Select(generation, entity))
            .on_category_left_click(Message::Sort)
            .item_context(|row: &Row| {
                let pid = *row.pids.first()?;
                row.is_task().then(|| {
                    widget::menu::items(
                        &HashMap::new(),
                        vec![
                            widget::menu::Item::Button(
                                fl!("end-task"),
                                None,
                                Action::EndTask(pid),
                            ),
                            widget::menu::Item::Button(
                                fl!("kill-task"),
                                None,
                                Action::KillTask(pid),
                            ),
                        ],
                    )
                })
            })
            // Without a spacing the scrollbar is drawn over the content: the table is laid
            // out across the whole width, and the last column ends up underneath the bar.
            // With one, the room is reserved in the layout - and only while the bar is
            // showing, so a short list keeps its full width.
            .apply(widget::scrollable)
            .spacing(spacing.space_xxxs)
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
        let mut tabs = segmented_button::ModelBuilder::default();
        for (label, page) in [
            (fl!("processes"), Page::Processes),
            (fl!("performance"), Page::Performance),
            (fl!("users"), Page::Users),
        ] {
            tabs = tabs.insert(move |b| b.text(label).data(page));
        }
        let mut tabs = tabs.build();
        let first = tabs.iter().next();
        if let Some(first) = first {
            tabs.activate(first);
        }

        let mut app = App {
            core,
            tabs,
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
        // Otherwise the first frame draws the static's zero default in every heading.
        app.publish_totals();

        let title = app.set_window_title(fl!("app-title"), app.core.main_window_id().unwrap());
        (app, title)
    }

    /// Tabs at the left of the title bar, the way the terminal and Files carry theirs.
    ///
    /// Known and accepted: the header bar wraps its whole width in a `mouse_area` that starts
    /// a window move as soon as the pointer travels a single pixel, and a segmented button
    /// only activates on release. A click with any drag in it therefore moves the window
    /// instead of switching tabs. Both other apps behave the same way; fixing it means one
    /// line in libcosmic and a rebuild of the whole stack.
    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        vec![
            widget::tab_bar::horizontal(&self.tabs)
                .button_height(32)
                .style(crate::tabs::style())
                .on_activate(Message::Tab)
                .width(Length::Shrink)
                .into(),
        ]
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Tick => {
                self.probe.sample();
                self.rebuild();
                self.rebuild_users();
                self.performance.counts = (
                    self.probe.processes().count(),
                    self.probe.processes().map(|p| p.threads).sum(),
                );
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
                self.selection = self
                    .table
                    .item(entity)
                    .and_then(|row| Selection::of(row, &self.probe));
            }
            Message::Search(text) => {
                self.search = text;
                self.rebuild();
            }
            Message::SearchClear => {
                self.search.clear();
                self.rebuild();
            }
            Message::Tab(entity) => self.tabs.activate(entity),
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
            Message::EndTask { pid, force } => {
                self.error = crate::processes::end(&[pid], force).err();
            }
            Message::DismissError => self.error = None,
            Message::Performance(message) => {
                match message {
                    performance::Message::Select(device) => self.performance.device = device,
                    performance::Message::PerCore(on) => self.performance.per_core = on,
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
            // Spacing for the same reason the process table has one: without it the last
            // column is drawn under the scrollbar.
            Page::Users => widget::table(&self.users)
                .apply(widget::scrollable)
                .spacing(cosmic::theme::spacing().space_xxxs)
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
    EndTask(u32),
    /// `SIGKILL`: it does not.
    KillTask(u32),
}

impl widget::menu::Action for Action {
    type Message = Message;

    fn message(&self) -> Self::Message {
        match *self {
            Self::EndTask(pid) => Message::EndTask { pid, force: false },
            Self::KillTask(pid) => Message::EndTask { pid, force: true },
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
