// SPDX-License-Identifier: GPL-3.0-only

//! The unchanging half of the load page: what the machine is, beside what it is doing.
//!
//! Both references carry it - `ref/taskman/cpu.png` and `ref/mission-center/cpu-dark.png` -
//! and it is the one part of that page WMDE had nothing in.
//!
//! Read once at start-up. A processor does not gain cores while the window is open, and the
//! model name and the cache sizes cost a `/proc/cpuinfo` parse that has no business running
//! every second. What does change - load, process and thread counts, uptime - is not here.

use std::fmt::Write as _;

/// What the machine is. Empty strings for anything the machine does not say.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// `model name` out of `/proc/cpuinfo`.
    pub model: String,
    /// Logical processors, which is what the per-core tiles count.
    pub cpus: usize,
    /// Distinct `physical id` values, or 1 when the machine does not say.
    pub sockets: usize,
    /// Level and size, largest last: `("L1d", "48 KiB")`.
    pub caches: Vec<(String, String)>,
    /// `Yes` when the kernel says it is running under a hypervisor.
    pub virtualized: bool,
}

impl Facts {
    #[must_use]
    pub fn load() -> Self {
        let mut out = Self {
            sockets: 1,
            ..Self::default()
        };

        if let Ok(info) = std::fs::read_to_string("/proc/cpuinfo") {
            let mut sockets = std::collections::BTreeSet::new();
            for line in info.lines() {
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                let (key, value) = (key.trim(), value.trim());
                match key {
                    "processor" => out.cpus += 1,
                    "model name" if out.model.is_empty() => out.model = value.to_owned(),
                    "physical id" => {
                        sockets.insert(value.to_owned());
                    }
                    // The kernel lists `hypervisor` among the flags on a guest, which is how
                    // the reference's "Virtual machine: Yes" is answered without asking dmi.
                    "flags" if !out.virtualized => {
                        out.virtualized = value.split_whitespace().any(|f| f == "hypervisor");
                    }
                    _ => {}
                }
            }
            if !sockets.is_empty() {
                out.sockets = sockets.len();
            }
        }

        out.caches = caches();
        out
    }
}

/// Cache levels of the first processor, from `/sys`.
///
/// Only cpu0: every processor of a socket reports the same geometry, and listing it once per
/// core would fill the panel with the same four lines forty times.
fn caches() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu/cpu0/cache") else {
        return out;
    };
    let mut entries: Vec<_> = dir.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let read = |name: &str| {
            std::fs::read_to_string(path.join(name))
                .ok()
                .map(|s| s.trim().to_owned())
        };
        let (Some(level), Some(size)) = (read("level"), read("size")) else {
            continue;
        };
        // `type` tells Data from Instruction apart, which is why a machine has both L1d and
        // L1i and why the level alone would name two different caches the same.
        let kind = read("type").unwrap_or_default();
        let suffix = match kind.as_str() {
            "Data" => "d",
            "Instruction" => "i",
            _ => "",
        };
        let mut label = String::new();
        let _ = write!(label, "L{level}{suffix}");
        if !out.iter().any(|(l, _): &(String, String)| *l == label) {
            out.push((label, size));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_running_machine_describes_itself() {
        let facts = Facts::load();
        assert!(facts.cpus > 0, "a machine has at least one processor");
        assert!(facts.sockets >= 1);
        // A model name and cache geometry are not guaranteed - a container or a foreign
        // architecture may say nothing - so neither is asserted, only that the read is safe.
        let _ = facts.model;
        let _ = facts.caches;
    }

    #[test]
    fn cache_levels_are_not_repeated() {
        let facts = Facts::load();
        let mut names: Vec<&str> = facts.caches.iter().map(|(l, _)| l.as_str()).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "a cache level is listed twice");
    }
}
