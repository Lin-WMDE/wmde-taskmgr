// SPDX-License-Identifier: GPL-3.0-only

//! The window: a navigation rail on the left, one page per tab on the right.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::prelude::*;
use cosmic::widget::{self, dropdown, segmented_button, table};

use crate::fl;

/// The search field, named so the tab switch can hand it the keyboard.
static SEARCH: LazyLock<widget::Id> = LazyLock::new(|| widget::Id::new("search"));
use crate::performance::{self, Performance};
use crate::probe::Probe;
use crate::processes::{Column, Row};
use crate::sessions::{self, Session};
use crate::users::{self, Names, Range, Whose};
use crate::wifi::{self, WifiInfo};
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
    /// One row of the users page: a login, or what an account runs outside every login.
    /// Answers "what is root running over ssh", which no level of the three can.
    Whose(Whose),
}

impl Scope {
    /// The nesting levels, which are the whole of the first part of the control. The rest of
    /// it is built from the rows of the users page and changes as people come and go.
    const LEVELS: [Self; 3] = [Self::Mine, Self::Users, Self::All];

    /// How a level reads. A login names itself, out of the row it came from - see
    /// `Row::choice`.
    fn label(self) -> String {
        match self {
            Self::Mine => fl!("scope-mine"),
            Self::Users => fl!("scope-users"),
            Self::All => fl!("scope-all"),
            Self::Whose(_) => String::new(),
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
    /// logind's list of logins, as it stands. Arrives on start and whenever one comes or
    /// goes, never on the tick: sessions change on the scale of people, not of seconds.
    Sessions(Vec<Session>),
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
    /// The scope control changed.
    Scope(Scope),
    /// Show the processes of one row of the users page, and go to the list to do it.
    Show(Whose),
    /// A tab in the title bar was picked.
    Tab(segmented_button::Entity),
    Sort(Column),
    /// End the process the menu was opened over. Never the selected one: a right click
    /// dropped by the generation guard would otherwise leave the previous row armed, and the
    /// signal would go somewhere the user never pointed.
    EndTask {
        pid: u32,
        force: bool,
    },
    DismissError,
    Performance(performance::Message),
    /// What NetworkManager said about the wireless card on screen.
    Wifi(String, Option<WifiInfo>),
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
    /// The scope control: the three levels, and under them one entry per row of the users
    /// page. Rebuilt with that page, so a login that ends leaves the list with it.
    scopes: dropdown::multi::Model<String, Scope>,
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
    /// Who is logged in. Empty until logind answers, and empty for good on a machine that has
    /// no logind - the users page then folds by account, the way it did before.
    sessions: Vec<Session>,
    /// A NetworkManager query is out: a slow bus must not stack one up every tick.
    wifi_pending: bool,
    /// Wi-Fi is asked about every other tick; the signal does not move faster than that.
    wifi_tick: bool,
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
                // Kernel threads pass this too when they belong to the account picked, which
                // is the point: the filter names whose processes to show, not how deep to go.
                Scope::Whose(whose) => whose.holds(p, &self.sessions),
            })
            .filter(|p| {
                needle.is_empty()
                    || p.name.to_lowercase().contains(&needle)
                    || p.cmdline.to_lowercase().contains(&needle)
                    || p.pid.to_string().contains(&needle)
            })
            .map(|p| Row::process(p, self.performance.gpu_usage(p.pid)))
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
            let again = model.iter().find(|entity| {
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
            gpu: self.performance.gpu_busiest(),
        });
    }

    fn rebuild_users(&mut self) {
        let rows = users::rows(
            self.probe.processes(),
            &self.sessions,
            &self.user_names,
            self.user_range,
            self.uid,
        );

        // The scope control offers exactly these rows, so the two never disagree about who is
        // logged in. A pick that lost its row - somebody logged out - falls back to the
        // person at the machine rather than filtering on a login that is gone.
        let logins: Vec<(String, Scope)> = rows
            .iter()
            .filter_map(|row| Some((row.choice(), Scope::Whose(row.whose()?))))
            .collect();
        self.scope = valid(self.scope, &logins);

        let mut scopes = dropdown::multi::model();
        scopes.insert(dropdown::multi::list(
            None,
            Scope::LEVELS.iter().map(|s| (s.label(), *s)).collect(),
        ));
        if !logins.is_empty() {
            scopes.insert(dropdown::multi::list(Some(fl!("scope-logins")), logins));
        }
        scopes.selected = Some(self.scope);
        self.scopes = scopes;

        let mut model = table::Model::new(Column::USERS.to_vec());
        for row in rows {
            let _ = model.insert(row);
        }
        model.set_sort(Some((Column::Data, false)));
        self.users = model;
    }

    fn processes_view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let generation = self.generation;

        let search = widget::text_input::search_input(fl!("search"), &self.search)
            .id(SEARCH.clone())
            .width(Length::Fill)
            .on_input(Message::Search)
            .on_clear(Message::SearchClear);

        let scope = widget::dropdown::multi::dropdown(&self.scopes, Message::Scope);

        // Fill on the row as well as on the field. A Shrink row resolves a Fill child
        // against its own intrinsic width, and the search box would keep the width of its
        // placeholder. The dropdown is Shrink and keeps the width of what is picked; its list
        // is wider than that and slides left to fit, see libcosmic's multi dropdown overlay.
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
                            widget::menu::Item::Button(fl!("end-task"), None, Action::EndTask(pid)),
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

    fn init(mut core: Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        core.window.title_bar = cosmic::app::TitleBar::Tabs;
        let mut tabs = segmented_button::ModelBuilder::default();
        for (label, page) in [
            (fl!("processes"), Page::Processes),
            (fl!("performance"), Page::Performance),
            (fl!("users"), Page::Users),
        ] {
            tabs = tabs.insert(move |b| b.text(label).data(page));
        }
        let mut tabs = tabs.build();
        // The window opens on the load page, not on the first tab. A task manager is opened
        // to see how busy the machine is far more often than to hunt for one process, and the
        // load page answers that without a click. The process list stays first in the strip:
        // the reference orders it that way, and the order is not the choice being made here.
        let opening = tabs
            .iter()
            .find(|entity| tabs.data::<Page>(*entity) == Some(&Page::Performance))
            .or_else(|| tabs.iter().next());
        if let Some(opening) = opening {
            tabs.activate(opening);
        }

        let mut app = App {
            core,
            tabs,
            probe: Probe::new(),
            windows: Vec::new(),
            uid: rustix::process::getuid().as_raw(),
            search: String::new(),
            scope: Scope::default(),
            scopes: dropdown::multi::model(),
            // Memory first: it is the column the list is opened for.
            sort: (Column::Data, false),
            table: table::Model::new(Column::ALL.to_vec()),
            generation: 0,
            selection: None,
            error: None,
            performance: Performance::new(),
            user_names: Names::load(),
            user_range: Range::load(),
            users: table::Model::new(Column::USERS.to_vec()),
            sessions: Vec::new(),
            wifi_pending: false,
            wifi_tick: false,
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
                // The load half before the rows: the rows carry each process's use of the
                // graphics cards, and the headings the machine-wide figures - both of this
                // second, the one the rows describe.
                self.performance.counts = (
                    self.probe.processes().count(),
                    self.probe.processes().map(|p| p.threads).sum(),
                );
                self.performance.tick();
                self.publish_totals();
                // The users page first: it is what tells a scope that its login has gone, and
                // the list below must not be built against a filter that no longer names
                // anybody.
                self.rebuild_users();
                self.rebuild();

                // The wireless card on screen, every other tick, off the update thread.
                self.wifi_tick = !self.wifi_tick;
                if let Some(iface) = self.performance.wireless()
                    && self.wifi_tick
                    && !self.wifi_pending
                {
                    self.wifi_pending = true;
                    return cosmic::task::future(async move {
                        let info = wifi::query(iface.clone()).await;
                        Message::Wifi(iface, info)
                    });
                }
            }
            Message::Wifi(iface, info) => {
                self.wifi_pending = false;
                self.performance.set_wifi(iface, info);
            }
            // Kept, not used: the flat list needs nothing from it, and the folding in
            // `group` needs all of it the moment it is wired back up.
            Message::Bus(Event::Windows(windows)) => self.windows = windows,
            Message::Sessions(sessions) => {
                self.sessions = sessions;
                // Straight away rather than at the next tick: a login that has just arrived
                // is exactly what somebody is looking at the page to see.
                self.rebuild_users();
            }
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
            // Arriving at the process list puts the keyboard in the search field: the first
            // thing anyone does on this page is look for a process, and the list itself takes
            // no typing. Any other page leaves the focus where it was.
            Message::Tab(entity) => {
                self.tabs.activate(entity);
                if self.page() == Page::Processes {
                    return widget::text_input::focus(SEARCH.clone());
                }
            }
            Message::Scope(scope) => {
                self.scope = scope;
                self.scopes.selected = Some(scope);
                self.rebuild();
            }
            Message::Show(whose) => {
                self.scope = Scope::Whose(whose);
                self.scopes.selected = Some(self.scope);
                self.rebuild();
                // Through the tab message rather than by activating the tab here: arriving at
                // the process list hands the keyboard to the search field, and a second way in
                // would behave differently from the strip.
                let processes = self
                    .tabs
                    .iter()
                    .find(|entity| self.tabs.data::<Page>(*entity) == Some(&Page::Processes));
                if let Some(entity) = processes {
                    return self.update(Message::Tab(entity));
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
            Message::EndTask { pid, force } => {
                self.error = crate::processes::end(&[pid], force).err();
            }
            Message::DismissError => self.error = None,
            Message::Performance(message) => {
                match message {
                    performance::Message::Select(device) => self.performance.select(device),
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
                // One item, and it carries the row's own key rather than the selection: the
                // menu opens over a row whether or not the table took the click.
                .item_context(|row: &Row| {
                    let whose = row.whose()?;
                    Some(widget::menu::items(
                        &HashMap::new(),
                        vec![widget::menu::Item::Button(
                            fl!("show-processes"),
                            None,
                            Action::Show(whose),
                        )],
                    ))
                })
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
            Subscription::run(sessions::subscription).map(Message::Sessions),
        ])
    }
}

/// A pick that lost its row - somebody logged out, an account stopped running anything - falls
/// back to the person at the machine. Filtering on a login that is gone would leave an empty
/// list and no way to see that this is why.
fn valid(scope: Scope, logins: &[(String, Scope)]) -> Scope {
    match scope {
        Scope::Whose(_) if !logins.iter().any(|(_, offered)| *offered == scope) => Scope::default(),
        _ => scope,
    }
}

/// What the context menu over a row offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    /// `SIGTERM`: the application gets to close its windows and save.
    EndTask(u32),
    /// `SIGKILL`: it does not.
    KillTask(u32),
    /// Go to the process list, filtered to one row of the users page.
    Show(Whose),
}

impl widget::menu::Action for Action {
    type Message = Message;

    fn message(&self) -> Self::Message {
        match *self {
            Self::EndTask(pid) => Message::EndTask { pid, force: false },
            Self::KillTask(pid) => Message::EndTask { pid, force: true },
            Self::Show(whose) => Message::Show(whose),
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
                    _ => true,
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

    /// A login that ends takes the filter with it.
    #[test]
    fn a_scope_never_outlives_its_login() {
        let offered = vec![
            ("softer - ssh".to_owned(), Scope::Whose(Whose::Login(20))),
            (
                "softer - background".to_owned(),
                Scope::Whose(Whose::Outside(1000)),
            ),
        ];

        assert_eq!(
            valid(Scope::Whose(Whose::Login(20)), &offered),
            Scope::Whose(Whose::Login(20)),
            "a login still listed is kept"
        );
        assert_eq!(
            valid(Scope::Whose(Whose::Login(21)), &offered),
            Scope::Mine,
            "a login that has gone falls back"
        );
        // The three levels are not rows and are never withdrawn.
        assert_eq!(valid(Scope::All, &[]), Scope::All);
    }

    /// Every scope has a label in the catalogue, and no two read the same.
    #[test]
    fn every_scope_is_named() {
        let labels: Vec<String> = Scope::LEVELS.iter().map(|s| s.label()).collect();
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
