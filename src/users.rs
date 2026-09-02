// SPDX-License-Identifier: GPL-3.0-only

//! The same walk, folded by the account that owns each process.

use std::collections::HashMap;

use crate::probe::Probe;
use crate::processes::Row;

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

/// One row per account, heaviest first.
#[must_use]
pub fn rows(probe: &Probe, names: &Names) -> Vec<Row> {
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
        let rows = rows(&probe, &names);
        assert!(!rows.is_empty(), "a running system has at least one account");
        let total: u64 = rows.iter().map(|r| r.sort_data).sum();
        assert!(total > 0, "the accounts hold some memory between them");
    }

    #[test]
    fn an_unknown_account_reads_as_its_number() {
        let names = Names(HashMap::new());
        assert_eq!(names.get(4242), "4242");
    }
}
