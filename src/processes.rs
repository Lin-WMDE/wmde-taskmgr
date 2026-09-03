// SPDX-License-Identifier: GPL-3.0-only

//! The process list: what a row is, how the columns read, and how a task is ended.

use std::borrow::Cow;

use cosmic::iced::{Length, alignment::Horizontal};
use cosmic::widget::Icon;
use cosmic::widget::table::{ItemCategory, ItemInterface};

use crate::fl;
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

/// What the whole machine is doing, written into the column headings.
///
/// Taken from the reference: the total sits where the column it totals is, so a glance at the
/// heading answers "is anything busy" without leaving the list.
#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    /// Share of the machine, 0.0 to 1.0.
    pub cpu: f32,
    /// Share of physical memory held, 0.0 to 1.0.
    pub memory: f32,
    /// Bytes per second to and from storage.
    pub disk: f64,
}

/// Written into the headings on the next view. A `Display` implementation has nowhere to take
/// an argument, and the table asks the category to name itself.
static TOTALS: std::sync::RwLock<Totals> = std::sync::RwLock::new(Totals {
    cpu: 0.0,
    memory: 0.0,
    disk: 0.0,
});

pub fn set_totals(totals: Totals) {
    if let Ok(mut slot) = TOTALS.write() {
        *slot = totals;
    }
}

impl Column {
    /// The machine-wide figure this column heads, or nothing for a column that has none.
    fn total(self) -> Option<String> {
        let totals = TOTALS.read().ok()?;
        match self {
            Self::Cpu => Some(format!("{:.0}%", totals.cpu * 100.0)),
            Self::Memory => Some(format!("{:.0}%", totals.memory * 100.0)),
            Self::Disk => Some(rate(Some([totals.disk, 0.0]))),
            _ => None,
        }
    }
}

impl std::fmt::Display for Column {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(total) = self.total() {
            // Two lines, as the reference writes it: the name, and the machine-wide figure
            // under it. Side by side the pair does not fit a column sized for its values.
            return write!(f, "{}\n{total}", self.title());
        }
        f.write_str(&self.title())
    }
}

impl Column {
    fn title(self) -> String {
        match self {
            Self::Name => fl!("column-name"),
            Self::Cpu => fl!("column-cpu"),
            Self::Memory => fl!("column-memory"),
            Self::Data => fl!("column-data"),
            Self::Disk => fl!("column-disk"),
            Self::Threads => fl!("column-threads"),
            Self::Pid => fl!("column-pid"),
        }
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// One process in the list.
    Process,
    /// One account on the users page. Not a task: it cannot be ended as a unit.
    Account,
}

/// One line of the list, with every cell already rendered.
///
/// The text is built when the list is rebuilt, once a second, rather than in `get_text`,
/// which the toolkit calls on every frame.
#[derive(Clone, Debug)]
pub struct Row {
    pub kind: Kind,
    /// Processes this row would end. Empty for a section heading.
    pub pids: Vec<u32>,
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
    /// One process.
    ///
    /// Named by `comm`, the way the reference's details view names a process.
    #[must_use]
    pub fn process(p: &Process) -> Self {
        Self {
            kind: Kind::Process,
            pids: vec![p.pid],
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

    /// One account on the users page.
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
            kind: Kind::Account,
            name: format!("{name} ({})", pids.len()),
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

    /// What the name column shows. Only the tests need to read it back.
    #[cfg(test)]
    #[must_use]
    pub fn label(&self) -> &str {
        &self.name
    }

    /// An account is not a task and cannot be ended.
    #[must_use]
    pub fn is_task(&self) -> bool {
        matches!(self.kind, Kind::Process)
    }
}

impl ItemInterface<Column> for Row {
    /// No icons in this list. Most processes have none, and the ragged indent a handful of
    /// icons produced was worse than the little they told.
    fn get_icon(&self, _category: Column) -> Option<Icon> {
        None
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
        Column::Cpu => b
            .sort_cpu
            .partial_cmp(&a.sort_cpu)
            .unwrap_or(Ordering::Equal),
        Column::Memory => b.sort_memory.cmp(&a.sort_memory),
        Column::Data => b.sort_data.cmp(&a.sort_data),
        Column::Disk => b
            .sort_disk
            .partial_cmp(&a.sort_disk)
            .unwrap_or(Ordering::Equal),
        Column::Threads => b.sort_threads.cmp(&a.sort_threads),
        Column::Pid => a.sort_pid.cmp(&b.sort_pid),
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
        fl!(
            "size-gib",
            value = format!("{:.1}", value / (1024.0 * 1024.0))
        )
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
        fl!(
            "rate-gib",
            value = format!("{:.1}", value / (1024.0 * 1024.0 * 1024.0))
        )
    } else if value >= 1024.0 * 1024.0 {
        fl!(
            "rate-mib",
            value = format!("{:.1}", value / (1024.0 * 1024.0))
        )
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

    /// PID is the one number column built smallest-first, the way the reference orders it.
    /// The arrow that describes the order is derived from this in app.rs and got it backwards.
    #[test]
    fn pid_sorts_smallest_first_unlike_every_other_number() {
        let a = Row::process(&Process::stub(10, 1, "a"));
        let b = Row::process(&Process::stub(20, 1, "b"));
        assert_eq!(compare(&a, &b, Column::Pid), std::cmp::Ordering::Less);

        // Memory, for contrast: the bigger number comes first.
        let mut big = Row::process(&Process::stub(30, 1, "c"));
        let mut small = Row::process(&Process::stub(40, 1, "d"));
        big.sort_memory = 100;
        small.sort_memory = 1;
        assert_eq!(
            compare(&big, &small, Column::Memory),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn sizes_climb_through_the_units() {
        assert!(size(512).ends_with("КіБ") || size(512).ends_with("KiB"));
        assert!(size(2048).contains("2.0"));
        assert!(size(3 * 1024 * 1024).contains("3.0"));
    }
}
