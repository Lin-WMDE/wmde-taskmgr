// SPDX-License-Identifier: GPL-3.0-only

//! The same walk, folded by the account that owns each process.
//!
//! Only real accounts. A desktop runs a dozen system accounts - `root`, `dbus`, `polkitd`,
//! `avahi`, `systemd-timesync` - and a page listing them answers nothing the process list does
//! not already answer better. The reference lists the people signed in, and so does this.

use std::collections::HashMap;

use crate::probe::Probe;
use crate::processes::Row;

/// Where regular accounts begin and end.
///
/// Taken from `/etc/login.defs`, because a machine is free to move the boundary; the defaults
/// are what `useradd` falls back to. `nobody` sits at 65534, above `UID_MAX`, and is excluded
/// by the upper bound rather than by name.
#[derive(Clone, Copy, Debug)]
pub struct Range {
    min: u32,
    max: u32,
}

impl Default for Range {
    fn default() -> Self {
        Self {
            min: 1000,
            max: 60000,
        }
    }
}

impl Range {
    #[must_use]
    pub fn load() -> Self {
        let mut out = Self::default();
        let Ok(defs) = std::fs::read_to_string("/etc/login.defs") else {
            return out;
        };
        for line in defs.lines() {
            let mut f = line.split_whitespace();
            let (Some(key), Some(value)) = (f.next(), f.next()) else {
                continue;
            };
            let Ok(value) = value.parse::<u32>() else {
                continue;
            };
            match key {
                "UID_MIN" => out.min = value,
                "UID_MAX" => out.max = value,
                _ => {}
            }
        }
        out
    }

    #[must_use]
    fn holds(self, uid: u32) -> bool {
        (self.min..=self.max).contains(&uid)
    }
}

/// Names for the user ids that appear, read once from `/etc/passwd`.
///
/// Read directly rather than through `getpwuid`: that would pull libc's NSS in, which on a
/// machine with a network directory means a lookup that blocks, and a task manager that
/// stalls once a second is worse than one that shows a number.
pub struct Names(HashMap<u32, Box<str>>);

impl Names {
    #[must_use]
    pub fn load() -> Self {
        let mut names = HashMap::new();
        if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
            for line in passwd.lines() {
                // name:x:uid:gid:gecos:home:shell
                let mut fields = line.split(':');
                let (Some(name), Some(_), Some(uid)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    continue;
                };
                if let Ok(uid) = uid.parse::<u32>() {
                    names.entry(uid).or_insert_with(|| name.into());
                }
            }
        }
        Self(names)
    }

    /// The account's name, or its number when `/etc/passwd` does not have it - which is the
    /// ordinary case for a container's user seen from outside.
    #[must_use]
    pub fn get(&self, uid: u32) -> String {
        self.0
            .get(&uid)
            .map_or_else(|| uid.to_string(), ToString::to_string)
    }
}

/// One row per real account, heaviest first.
///
/// `current` is always listed, whatever its number: a session running as a system account is
/// odd, but a page that then shows nothing at all is worse.
#[must_use]
pub fn rows(probe: &Probe, names: &Names, range: Range, current: u32) -> Vec<Row> {
    struct Fold {
        pids: Vec<u32>,
        cpu: f32,
        rss: u64,
        data: u64,
        threads: u32,
        disk: Option<[f64; 2]>,
    }

    let mut by_uid: HashMap<u32, Fold> = HashMap::new();
    for p in probe.processes() {
        if !range.holds(p.uid) && p.uid != current {
            continue;
        }
        let fold = by_uid.entry(p.uid).or_insert_with(|| Fold {
            pids: Vec::new(),
            cpu: 0.0,
            rss: 0,
            data: 0,
            threads: 0,
            disk: None,
        });
        fold.pids.push(p.pid);
        fold.cpu += p.cpu;
        fold.rss += p.rss;
        fold.data += p.data;
        fold.threads += p.threads;
        if let Some([r, w]) = p.disk {
            let acc = fold.disk.get_or_insert([0.0, 0.0]);
            acc[0] += r;
            acc[1] += w;
        }
    }

    let mut out: Vec<Row> = by_uid
        .into_iter()
        .map(|(uid, f)| {
            Row::user(
                names.get(uid),
                f.pids,
                f.cpu,
                f.rss,
                f.data,
                f.threads,
                f.disk,
            )
        })
        .collect();
    out.sort_by(|a, b| b.sort_data.cmp(&a.sort_data));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_running_system_has_the_current_user_in_it() {
        let mut probe = Probe::new();
        probe.sample();
        let names = Names::load();
        let me = rustix::process::getuid().as_raw();
        let rows = rows(&probe, &names, Range::load(), me);
        assert!(!rows.is_empty(), "a running system has at least one account");
        let total: u64 = rows.iter().map(|r| r.sort_data).sum();
        assert!(total > 0, "the accounts hold some memory between them");
    }

    /// root owns more processes than anyone on a desktop, and none of them belong on a page
    /// about the people using the machine.
    #[test]
    fn system_accounts_are_left_out() {
        let mut probe = Probe::new();
        probe.sample();
        let names = Names::load();
        let me = rustix::process::getuid().as_raw();
        let rows = rows(&probe, &names, Range::load(), me);
        assert!(
            !rows.iter().any(|r| r.label().starts_with("root ")),
            "root is on the users page"
        );
    }

    #[test]
    fn the_boundary_comes_from_login_defs() {
        let range = Range { min: 1000, max: 60000 };
        assert!(!range.holds(0), "root");
        assert!(!range.holds(999), "a system account");
        assert!(range.holds(1000), "the first regular account");
        assert!(!range.holds(65534), "nobody sits above UID_MAX");
    }

    #[test]
    fn an_unknown_account_reads_as_its_number() {
        let names = Names(HashMap::new());
        assert_eq!(names.get(4242), "4242");
    }
}
