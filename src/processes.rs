// SPDX-License-Identifier: GPL-3.0-only

//! The process list: what a row is, how the columns read, and how a task is ended.

use std::borrow::Cow;

use cosmic::widget::table::{ItemCategory, ItemInterface};
use cosmic::widget::{Icon, icon};
use cosmic::iced::{Length, alignment::Horizontal};

use crate::fl;
use crate::group::{Group, Key, Section};
use crate::probe::Process;

/// The columns, in the order the list shows them.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Column {
    #[default]
    Name,
    Cpu,
    Memory,
    Data,
    Disk,
    Threads,
    Pid,
}

impl Column {
    pub const ALL: [Self; 7] = [
        Self::Name,
        Self::Cpu,
        Self::Memory,
        Self::Data,
        Self::Disk,
        Self::Threads,
        Self::Pid,
    ];
}

impl std::fmt::Display for Column {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self {
            Self::Name => fl!("column-name"),
            Self::Cpu => fl!("column-cpu"),
            Self::Memory => fl!("column-memory"),
            Self::Data => fl!("column-data"),
            Self::Disk => fl!("column-disk"),
            Self::Threads => fl!("column-threads"),
            Self::Pid => fl!("column-pid"),
        })
    }
}

impl ItemCategory for Column {
    /// Wide enough for the longest value each column actually prints, and no wider: what is
    /// left over goes to the name, which is the column that runs out of room first.
    fn width(&self) -> Length {
        match self {
            Self::Name => Length::Fill,
            Self::Cpu => Length::Fixed(60.0),
            Self::Memory | Self::Data => Length::Fixed(92.0),
            Self::Disk => Length::Fixed(96.0),
            Self::Threads => Length::Fixed(56.0),
            Self::Pid => Length::Fixed(60.0),
        }
    }

    /// Every number lines up on the right, so that the digits of one row sit above the digits
    /// of the next and the column can be read down. Only the name reads from the left.
    fn align(&self) -> Horizontal {
        match self {
            Self::Name => Horizontal::Left,
            _ => Horizontal::Right,
        }
    }
}

/// What a row stands for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    /// A heading with nothing but a name. Carries no numbers of its own.
    Section(Section),
    /// An application or a named group. `expandable` is false for a group of one, which has
    /// nothing to unfold.
    Group { expanded: bool, expandable: bool },
    /// One process inside an unfolded group.
    Child,
}

/// One line of the list, with every cell already rendered.
///
/// The text is built when the list is rebuilt, once a second, rather than in `get_text`,
/// which the toolkit calls on every frame.
#[derive(Clone, Debug)]
pub struct Row {
    pub kind: Kind,
    /// Which group the row unfolds. `None` for a heading and for a child row.
    pub key: Option<Key>,
    /// Processes this row would end. Empty for a section heading.
    pub pids: Vec<u32>,
    /// The icon name for an application row.
    icon_name: Option<String>,

    name: String,
    cpu: String,
    memory: String,
    data: String,
    disk: String,
    threads: String,
    pid: String,

    /// What `compare` sorts on, so that sorting never re-reads the formatted text.
    pub sort_cpu: f32,
    pub sort_memory: u64,
    pub sort_data: u64,
    pub sort_disk: f64,
    pub sort_threads: u32,
    pub sort_pid: u32,
}

impl Row {
    #[must_use]
    pub fn section(section: Section, count: usize) -> Self {
        let name = match section {
            Section::Applications => fl!("section-applications"),
            Section::Background => fl!("section-background"),
            Section::System => fl!("section-system"),
        };
        Self {
            kind: Kind::Section(section),
            key: None,
            pids: Vec::new(),
            icon_name: None,
            name: format!("{name} ({count})"),
            cpu: String::new(),
            memory: String::new(),
            data: String::new(),
            disk: String::new(),
            threads: String::new(),
            pid: String::new(),
            sort_cpu: 0.0,
            sort_memory: 0,
            sort_data: 0,
            sort_disk: 0.0,
            sort_threads: 0,
            sort_pid: 0,
        }
    }

    #[must_use]
    pub fn group(g: &Group, expanded: bool) -> Self {
        let expandable = g.pids.len() > 1;
        // "Panel (15)", not "Panel - 15 processes": the name column is what is left after
        // six number columns, and the long form wrapped a group onto three lines.
        let name = if expandable {
            format!("{} ({})", group_label(g), g.pids.len())
        } else {
            group_label(g)
        };
        Self {
            kind: Kind::Group {
                // A group that has shrunk to one process while it was unfolded has nothing
                // left to unfold, and a child row under it would repeat the row itself.
                expanded: expanded && expandable,
                expandable,
            },
            key: Some(g.key.clone()),
            pids: g.pids.clone(),
            icon_name: g.app_id.clone(),
            name,
            cpu: percent(g.cpu),
            memory: size(g.rss),
            data: size(g.data),
            disk: rate(g.disk),
            threads: g.threads.to_string(),
            // A group of one names its process; a group of many has no single pid to show.
            pid: if expandable {
                fl!("no-value")
            } else {
                g.pids.first().map_or_else(|| fl!("no-value"), u32::to_string)
            },
            sort_cpu: g.cpu,
            sort_memory: g.rss,
            sort_data: g.data,
            sort_disk: g.disk.map_or(0.0, |d| d[0] + d[1]),
            sort_threads: g.threads,
            sort_pid: g.pids.first().copied().unwrap_or(0),
        }
    }

    /// A row for one account on the users page. It is a group of processes like any other,
    /// but keyed by nothing: the users page does not unfold.
    #[must_use]
    pub fn user(
        name: String,
        pids: Vec<u32>,
        cpu: f32,
        rss: u64,
        data: u64,
        threads: u32,
        disk: Option<[f64; 2]>,
    ) -> Self {
        Self {
            kind: Kind::Group {
                expanded: false,
                expandable: false,
            },
            key: None,
            name: format!("{name} ({})", pids.len()),
            icon_name: None,
            cpu: percent(cpu),
            memory: size(rss),
            data: size(data),
            disk: rate(disk),
            threads: threads.to_string(),
            pid: fl!("no-value"),
            sort_cpu: cpu,
            sort_memory: rss,
            sort_data: data,
            sort_disk: disk.map_or(0.0, |d| d[0] + d[1]),
            sort_threads: threads,
            sort_pid: 0,
            pids,
        }
    }

    #[must_use]
    pub fn child(p: &Process) -> Self {
        Self {
            kind: Kind::Child,
            key: None,
            pids: vec![p.pid],
            icon_name: None,
            name: p.name.to_string(),
            cpu: percent(p.cpu),
            memory: size(p.rss),
            data: size(p.data),
            disk: rate(p.disk),
            threads: p.threads.to_string(),
            pid: p.pid.to_string(),
            sort_cpu: p.cpu,
            sort_memory: p.rss,
            sort_data: p.data,
            sort_disk: p.disk.map_or(0.0, |d| d[0] + d[1]),
            sort_threads: p.threads,
            sort_pid: p.pid,
        }
    }

    /// A bare row for the ordering tests.
    #[cfg(test)]
    #[must_use]
    pub fn stub_named(name: &str, pid: u32) -> Self {
        let mut row = Self::section(Section::Background, 0);
        row.kind = Kind::Child;
        row.name = name.to_owned();
        row.pids = vec![pid];
        row.sort_pid = pid;
        row
    }

    /// What the name column shows.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.name
    }

    /// A heading is not a task and cannot be ended.
    #[must_use]
    pub fn is_task(&self) -> bool {
        !matches!(self.kind, Kind::Section(_))
    }
}

impl ItemInterface<Column> for Row {
    fn get_icon(&self, category: Column) -> Option<Icon> {
        if category != Column::Name {
            return None;
        }
        match &self.kind {
            // The chevron is the only thing saying a row unfolds, so it stands where the
            // application icon would and points the way it will move.
            Kind::Group {
                expandable: true,
                expanded,
            } => Some(
                icon::from_name(if *expanded {
                    "pan-down-symbolic"
                } else {
                    "pan-end-symbolic"
                })
                .icon(),
            ),
            Kind::Group { .. } => self
                .icon_name
                .as_ref()
                .map(|name| icon::from_name(name.as_str()).icon()),
            _ => None,
        }
    }

    fn get_text(&self, category: Column) -> Cow<'static, str> {
        match category {
            Column::Name => self.name.clone().into(),
            Column::Cpu => self.cpu.clone().into(),
            Column::Memory => self.memory.clone().into(),
            Column::Data => self.data.clone().into(),
            Column::Disk => self.disk.clone().into(),
            Column::Threads => self.threads.clone().into(),
            Column::Pid => self.pid.clone().into(),
        }
    }

    /// Never called: the order is built outside, because a tree does not survive the flat
    /// sort the widget would otherwise apply. See `.doc/lists`.
    fn compare(&self, other: &Self, category: Column) -> std::cmp::Ordering {
        compare(self, other, category)
    }
}

/// Order two rows by one column, largest first for a number and alphabetically for a name.
#[must_use]
pub fn compare(a: &Row, b: &Row, category: Column) -> std::cmp::Ordering {
    // Ties fall through to the name and then the pid. Groups arrive from a HashMap, whose
    // order changes between ticks, so without a total order two rows holding the same number
    // - and a idle machine has many - swap places every second.
    by(a, b, category)
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| a.sort_pid.cmp(&b.sort_pid))
}

fn by(a: &Row, b: &Row, category: Column) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match category {
        Column::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        Column::Cpu => b.sort_cpu.partial_cmp(&a.sort_cpu).unwrap_or(Ordering::Equal),
        Column::Memory => b.sort_memory.cmp(&a.sort_memory),
        Column::Data => b.sort_data.cmp(&a.sort_data),
        Column::Disk => b.sort_disk.partial_cmp(&a.sort_disk).unwrap_or(Ordering::Equal),
        Column::Threads => b.sort_threads.cmp(&a.sort_threads),
        Column::Pid => a.sort_pid.cmp(&b.sort_pid),
    }
}

/// A group's built-in rules carry a message id, an application carries its own name.
fn group_label(g: &Group) -> String {
    match g.name.as_str() {
        "panel" => fl!("group-panel"),
        "desktop-shell" => fl!("group-desktop-shell"),
        other => other.to_owned(),
    }
}

/// End a task: `SIGTERM` first, which lets an application close its windows and save.
///
/// The error is worth showing rather than swallowing: refusing a signal to another user's
/// process is the ordinary answer here, not an exception, and a row that simply stays put
/// tells the person nothing.
pub fn end(pids: &[u32], force: bool) -> Result<(), String> {
    use rustix::process::{Pid, Signal, kill_process};

    let signal = if force { Signal::KILL } else { Signal::TERM };
    let mut failures = Vec::new();
    for pid in pids {
        let Some(handle) = i32::try_from(*pid).ok().and_then(Pid::from_raw) else {
            continue;
        };
        if let Err(err) = kill_process(handle, signal) {
            failures.push(format!("{pid}: {err}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join(", "))
    }
}

/// `0.0` to `1.0` as a percentage with one decimal, the way the reference writes it.
fn percent(share: f32) -> String {
    format!("{:.1}%", (share * 100.0).clamp(0.0, 100.0))
}

/// KiB from `/proc` as the largest unit that leaves a number worth reading.
fn size(kib: u64) -> String {
    let value = kib as f64;
    if value >= 1024.0 * 1024.0 {
        fl!("size-gib", value = format!("{:.1}", value / (1024.0 * 1024.0)))
    } else if value >= 1024.0 {
        fl!("size-mib", value = format!("{:.1}", value / 1024.0))
    } else {
        fl!("size-kib", value = format!("{value:.0}"))
    }
}

/// Bytes per second, read and written added together. A missing pair is a dash rather than a
/// zero: the counters were refused, which is not the same as no traffic.
fn rate(disk: Option<[f64; 2]>) -> String {
    let Some([read, written]) = disk else {
        return fl!("no-value");
    };
    let value = read + written;
    if value >= 1024.0 * 1024.0 * 1024.0 {
        fl!("rate-gib", value = format!("{:.1}", value / (1024.0 * 1024.0 * 1024.0)))
    } else if value >= 1024.0 * 1024.0 {
        fl!("rate-mib", value = format!("{:.1}", value / (1024.0 * 1024.0)))
    } else if value >= 1024.0 {
        fl!("rate-kib", value = format!("{:.1}", value / 1024.0))
    } else {
        fl!("rate-b", value = format!("{value:.0}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_line_up_on_the_right_and_the_name_does_not() {
        assert_eq!(Column::Name.align(), Horizontal::Left);
        for column in Column::ALL.iter().filter(|c| **c != Column::Name) {
            assert_eq!(column.align(), Horizontal::Right, "{column:?}");
        }
    }

    #[test]
    fn a_refused_disk_counter_is_a_dash_and_not_a_zero() {
        assert_eq!(rate(None), fl!("no-value"));
        assert_ne!(rate(Some([0.0, 0.0])), fl!("no-value"));
    }

    #[test]
    fn cpu_never_reads_above_the_whole_machine() {
        // The share is measured against every core at once, so a rounding wobble must not
        // print 100.4% and make the column look broken.
        assert_eq!(percent(1.004), "100.0%");
        assert_eq!(percent(0.25), "25.0%");
    }

    /// A group of one is never unfolded, whatever was remembered about it: the child row
    /// would be the group row again.
    #[test]
    fn a_group_that_shrank_to_one_stops_being_unfolded() {
        let one = Group {
            key: Key::Process(7),
            name: "one".into(),
            app_id: None,
            section: Section::Background,
            pids: vec![7],
            cpu: 0.0,
            rss: 0,
            data: 0,
            swap: 0,
            threads: 1,
            disk: None,
        };
        let row = Row::group(&one, true);
        assert_eq!(
            row.kind,
            Kind::Group {
                expanded: false,
                expandable: false
            }
        );
    }

    /// Two rows that tie on the sorted column must still have a fixed order, or the list
    /// reshuffles itself every second on an idle machine where most numbers are equal.
    #[test]
    fn ties_are_broken_the_same_way_every_time() {
        let mut a = Row::stub_named("alpha", 10);
        let mut b = Row::stub_named("beta", 20);
        a.sort_data = 0;
        b.sort_data = 0;
        assert_eq!(compare(&a, &b, Column::Data), std::cmp::Ordering::Less);
        assert_eq!(compare(&b, &a, Column::Data), std::cmp::Ordering::Greater);

        // Same name too: the pid is the last resort.
        let mut c = Row::stub_named("alpha", 99);
        c.sort_data = 0;
        assert_eq!(compare(&a, &c, Column::Data), std::cmp::Ordering::Less);
    }

    #[test]
    fn sizes_climb_through_the_units() {
        assert!(size(512).ends_with("КіБ") || size(512).ends_with("KiB"));
        assert!(size(2048).contains("2.0"));
        assert!(size(3 * 1024 * 1024).contains("3.0"));
    }
}
