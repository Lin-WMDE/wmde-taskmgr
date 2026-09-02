// SPDX-License-Identifier: GPL-3.0-only

//! Folding processes into the rows the list shows.
//!
//! Chrome is a hundred and eighty processes, the panel is fifteen, and a row per process
//! answers nothing. The order of resolution runs from fact to guess:
//!
//! 1. **The process owns a window.** Not a guess at all - the compositor said so.
//! 2. **An ancestor owns a window.** Electron and Chrome helpers run as
//!    `/proc/self/exe --type=utility` and carry no application name of their own.
//! 3. **A rule matches.** For the parts of the stack that never open a window.
//! 4. **An ancestor is grouped**, walking up `ppid` and stopping at pid 1 and at a barrier.
//! 5. **Nothing matched** - the process stands as its own row, which is what the reference
//!    does with a background process it cannot name.
//!
//! The barriers in [`BARRIERS`] are what keeps step 4 honest, and one of them is load-bearing
//! in a way the others are not: **a Wayland client is a direct child of the compositor**. Let
//! `wmde-comp` be grouped without also being a barrier and every unmatched application on the
//! desktop walks up into it.

use std::collections::HashMap;

use crate::probe::{Probe, Process};
use crate::windows::Window;

/// Processes that start unrelated programs. An ancestor walk that crosses one of these has
/// stopped explaining anything: `systemd --user` is the parent of half the session, and a
/// group inherited across it is a guess wearing a fact's clothes.
///
/// `wmde-comp` is here because every Wayland client is its child, `wmde-session` because it
/// starts the whole session, and the bus daemons because activation makes them the parent of
/// whatever they started.
const BARRIERS: &[&str] = &[
    "systemd",
    "init",
    "dbus-daemon",
    "dbus-broker",
    "dbus-broker-lau",
    "xdg-desktop-por",
    "sshd",
    "wmde-comp",
    "wmde-session",
];

/// The kernel's limit on `comm`, and therefore on how much of a rule can ever be compared.
/// `wmde-notifications` arrives as `wmde-notificati`; a rule written out in full would never
/// match anything.
const COMM_LEN: usize = 15;

/// Groups that own no window and would otherwise be a row each.
///
/// Written out in full and cut to [`COMM_LEN`] at comparison time, so a rule reads as the
/// program it names. The panel is the reason this list exists at all: fifteen applet
/// processes sharing one binary read as fifteen rows and one big number each, when what the
/// machine is actually spending is their sum.
const RULES: &[(&str, &[&str])] = &[
    (
        "panel",
        &["wmde-panel", "wmde-applets", "wmde-applet-", "wmde-start-menu"],
    ),
    (
        "desktop-shell",
        &[
            "wmde-bg",
            "wmde-osd",
            "wmde-notification",
            "wmde-settings-daemon",
            "wmde-portal",
            "xdg-desktop-por",
        ],
    ),
];

/// Which part of the list a row belongs to, in the reference's order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Section {
    /// The group owns at least one window.
    Applications,
    /// No window, and it runs as the person sitting at the machine.
    Background,
    /// A system account, or a kernel thread.
    System,
}

/// What a row is keyed by. Survives a tick, so the expanded rows stay expanded.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Key {
    /// An application, by the app id of the window it owns.
    App(String),
    /// A built-in rule, by its name.
    Rule(&'static str),
    /// One process that matched nothing.
    Process(u32),
}

/// One row of the list: an application, a named group, or a single process.
#[derive(Clone, Debug)]
pub struct Group {
    pub key: Key,
    /// What the row is called. An app id where there is one, else the process name.
    pub name: String,
    /// Set when a `.desktop` icon can be looked up for the row.
    pub app_id: Option<String>,
    pub section: Section,
    /// The processes folded into this row, most expensive first.
    pub pids: Vec<u32>,
    pub cpu: f32,
    pub rss: u64,
    pub data: u64,
    pub swap: u64,
    pub threads: u32,
    /// Summed over the processes whose counters were readable. `None` when none of them were.
    pub disk: Option<[f64; 2]>,
}

/// Fold a walk into rows.
///
/// `uid` is the person at the machine: anything else is a system process, whatever it is
/// named.
#[must_use]
pub fn build(probe: &Probe, windows: &[Window], uid: u32) -> Vec<Group> {
    let by_pid: HashMap<u32, &Process> = probe.processes().map(|p| (p.pid, p)).collect();

    let mut window_of: HashMap<u32, &Window> = HashMap::new();
    for w in windows {
        // A process with several windows names the row once. The focused one wins, so the
        // row is called what the person is looking at.
        match window_of.get(&w.pid) {
            Some(existing) if existing.activated => {}
            _ => {
                window_of.insert(w.pid, w);
            }
        }
    }

    // Step 1 and 3: what each process resolves to on its own, before any walking.
    let mut direct: HashMap<u32, Key> = HashMap::new();
    for p in probe.processes() {
        if let Some(w) = window_of.get(&p.pid) {
            direct.insert(p.pid, Key::App(app_key(w)));
        } else if let Some(rule) = rule_for(p) {
            direct.insert(p.pid, Key::Rule(rule));
        }
    }

    let mut groups: HashMap<Key, Group> = HashMap::new();
    for p in probe.processes() {
        let key = direct
            .get(&p.pid)
            .cloned()
            .or_else(|| inherit(p, &by_pid, &direct))
            .unwrap_or(Key::Process(p.pid));

        let entry = groups.entry(key.clone()).or_insert_with(|| Group {
            name: label(&key, p, &window_of),
            app_id: match &key {
                Key::App(id) => Some(id.clone()),
                _ => None,
            },
            key,
            section: Section::Background,
            pids: Vec::new(),
            cpu: 0.0,
            rss: 0,
            data: 0,
            swap: 0,
            threads: 0,
            disk: None,
        });

        entry.pids.push(p.pid);
        entry.cpu += p.cpu;
        entry.rss += p.rss;
        entry.data += p.data;
        entry.swap += p.swap;
        entry.threads += p.threads;
        if let Some([r, w]) = p.disk {
            let acc = entry.disk.get_or_insert([0.0, 0.0]);
            acc[0] += r;
            acc[1] += w;
        }
    }

    let mut out: Vec<Group> = groups.into_values().collect();
    for g in &mut out {
        g.section = section_of(g, &by_pid, &window_of, uid);
        // Most expensive first, so the expanded row leads with what is worth looking at.
        g.pids.sort_unstable_by(|a, b| {
            let (a, b) = (by_pid.get(a), by_pid.get(b));
            b.map_or(0, |p| p.data)
                .cmp(&a.map_or(0, |p| p.data))
                .then_with(|| a.map_or(0, |p| p.pid).cmp(&b.map_or(0, |p| p.pid)))
        });
    }
    out
}

/// Walk up to the first ancestor that resolved to something, stopping at pid 1 and at a
/// barrier.
fn inherit(p: &Process, by_pid: &HashMap<u32, &Process>, direct: &HashMap<u32, Key>) -> Option<Key> {
    let mut seen = vec![p.pid];
    let mut cur = by_pid.get(&p.ppid).copied();
    while let Some(c) = cur {
        if c.pid == 1 || seen.contains(&c.pid) {
            return None;
        }
        if is_barrier(c) {
            return None;
        }
        if let Some(key) = direct.get(&c.pid) {
            return Some(key.clone());
        }
        seen.push(c.pid);
        cur = by_pid.get(&c.ppid).copied();
    }
    None
}

fn is_barrier(p: &Process) -> bool {
    BARRIERS.iter().any(|b| p.name.as_ref() == *b)
}

fn rule_for(p: &Process) -> Option<&'static str> {
    RULES.iter().find_map(|(name, patterns)| {
        patterns
            .iter()
            .any(|pat| p.name.starts_with(&pat[..pat.len().min(COMM_LEN)]))
            .then_some(*name)
    })
}

/// The app id a window's row is keyed by. An empty one falls back to the title, so two
/// windows that declare nothing do not collapse into one row.
fn app_key(w: &Window) -> String {
    if w.app_id.is_empty() {
        w.title.clone()
    } else {
        w.app_id.clone()
    }
}

fn label(key: &Key, p: &Process, window_of: &HashMap<u32, &Window>) -> String {
    match key {
        Key::App(id) => window_of
            .get(&p.pid)
            .filter(|w| !w.title.is_empty())
            .map_or_else(|| id.clone(), |w| w.title.clone()),
        // The rule names are message ids; the view resolves them, this layer stays free of
        // localization the way the sampling layer does.
        Key::Rule(name) => (*name).to_owned(),
        Key::Process(_) => p.name.to_string(),
    }
}

fn section_of(
    g: &Group,
    by_pid: &HashMap<u32, &Process>,
    window_of: &HashMap<u32, &Window>,
    uid: u32,
) -> Section {
    if g.pids.iter().any(|pid| window_of.contains_key(pid)) {
        return Section::Applications;
    }
    let all_system = g.pids.iter().all(|pid| {
        by_pid
            .get(pid)
            .is_none_or(|p| p.uid != uid || p.is_kernel_thread())
    });
    if all_system { Section::System } else { Section::Background }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trap this whole module is arranged around. A Wayland client is a direct child of
    /// the compositor, so an application that matches nothing must NOT end up inside it.
    #[test]
    fn the_compositor_does_not_swallow_the_desktop() {
        let compositor = 100;
        let app = 200;
        let procs: HashMap<u32, &Process> = HashMap::new();
        let mut by_pid = procs;
        let comp = proc(compositor, 1, "wmde-comp");
        let editor = proc(app, compositor, "some-editor");
        by_pid.insert(compositor, &comp);
        by_pid.insert(app, &editor);

        // The compositor is grouped, which is exactly the situation that breaks without a
        // barrier.
        let mut direct = HashMap::new();
        direct.insert(compositor, Key::Rule("desktop-shell"));

        assert!(
            inherit(&editor, &by_pid, &direct).is_none(),
            "the editor inherited the compositor's group"
        );
    }

    #[test]
    fn a_helper_inherits_the_window_owning_parent() {
        let browser = 300;
        let helper = 301;
        let mut by_pid = HashMap::new();
        let parent = proc(browser, 1, "chrome");
        let child = proc(helper, browser, "chrome");
        by_pid.insert(browser, &parent);
        by_pid.insert(helper, &child);

        let mut direct = HashMap::new();
        direct.insert(browser, Key::App("google-chrome".into()));

        assert_eq!(
            inherit(&child, &by_pid, &direct),
            Some(Key::App("google-chrome".into()))
        );
    }

    #[test]
    fn a_cycle_in_the_parent_chain_terminates() {
        // /proc is read one file at a time, so two entries can disagree about who is whose
        // parent. The walk must end rather than spin.
        let a = 400;
        let b = 401;
        let mut by_pid = HashMap::new();
        let pa = proc(a, b, "a");
        let pb = proc(b, a, "b");
        by_pid.insert(a, &pa);
        by_pid.insert(b, &pb);
        assert_eq!(inherit(&pa, &by_pid, &HashMap::new()), None);
    }

    #[test]
    fn applets_fold_into_one_row() {
        for name in ["wmde-applet-tim", "wmde-applet-aud", "wmde-panel"] {
            assert_eq!(rule_for(&proc(1, 1, name)), Some("panel"), "{name}");
        }
        assert_eq!(rule_for(&proc(1, 1, "some-editor")), None);
    }

    /// The names the kernel actually reports, cut to 15 characters. A rule spelled out in
    /// full has to keep matching them.
    #[test]
    fn rules_survive_the_comm_truncation() {
        assert_eq!(rule_for(&proc(1, 1, "wmde-notificati")), Some("desktop-shell"));
        assert_eq!(rule_for(&proc(1, 1, "wmde-settings-d")), Some("desktop-shell"));
        assert_eq!(rule_for(&proc(1, 1, "xdg-desktop-por")), Some("desktop-shell"));
        // And the truncation must not turn a rule into a prefix that catches strangers.
        assert_eq!(rule_for(&proc(1, 1, "wmde-settings")), None, "the settings window");
    }

    /// Every process lands in exactly one row. A process counted twice would inflate the
    /// totals, and one counted nowhere would hide from the person looking for it.
    #[test]
    fn every_process_is_in_exactly_one_row() {
        let mut probe = Probe::new();
        probe.sample();
        let groups = build(&probe, &[], 1000);

        let mut seen = std::collections::HashSet::new();
        for g in &groups {
            for pid in &g.pids {
                assert!(seen.insert(*pid), "pid {pid} is in two rows");
            }
        }
        assert_eq!(
            seen.len(),
            probe.processes().count(),
            "the rows hold a different number of processes than the walk found"
        );

        if std::env::var_os("TASKMGR_SHOW_ROWS").is_some() {
            let mut rows = groups;
            rows.sort_unstable_by(|a, b| b.data.cmp(&a.data));
            for g in rows.iter().take(12) {
                eprintln!(
                    "{:?} {:28} {:8.1} МиБ  проц {:3}  потоков {:4}",
                    g.section,
                    g.name,
                    g.data as f64 / 1024.0,
                    g.pids.len(),
                    g.threads
                );
            }
        }
    }

    fn proc(pid: u32, ppid: u32, name: &str) -> Process {
        Process::stub(pid, ppid, name)
    }
}
