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
use crate::processes::{Column, Kind, Row};
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
    Select(table::Entity),
    /// Unfold or fold a group.
    Toggle(table::Entity),
    Sort(Column),
    EndTask { force: bool },
    DismissError,
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
    /// What "end task" would act on. Kept apart from the table's own selection because the
    /// table is rebuilt from scratch every tick.
    selected: Vec<u32>,
    error: Option<String>,
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

        if !self.selected.is_empty() {
            let again = model
                .iter()
                .find(|entity| model.item(*entity).is_some_and(|r| r.pids == self.selected));
            if let Some(entity) = again {
                model.activate(entity);
            }
        }

        self.table = model;
    }

    fn active_row(&self) -> Option<&Row> {
        self.table.active_data::<Row>()
    }

    fn processes_view(&self) -> Element<'_, Message> {
        let table = widget::table(&self.table)
            .on_item_left_click(Message::Select)
            // Right click selects too, so the menu below always acts on the row it was
            // opened over rather than on whatever was selected before.
            .on_item_right_click(Message::Select)
            .on_item_double_click(Message::Toggle)
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
            selected: Vec::new(),
            error: None,
        };
        // The first walk has nothing to take a delta against, so every rate reads zero until
        // the second one a second later. Taking it here means the window is not empty when
        // it opens.
        app.probe.sample();
        app.rebuild();

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
            }
            Message::Windows(windows) => {
                self.windows = windows;
                self.rebuild();
            }
            Message::Select(entity) => {
                self.table.activate(entity);
                self.selected = self
                    .table
                    .item(entity)
                    .map(|row| row.pids.clone())
                    .unwrap_or_default();
            }
            Message::Toggle(entity) => {
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
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        match self.page() {
            Page::Processes => self.processes_view(),
            // Both land in the next steps; an empty page beats a page that lies.
            Page::Performance => widget::text::body(fl!("performance")).into(),
            Page::Users => widget::text::body(fl!("users")).into(),
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
