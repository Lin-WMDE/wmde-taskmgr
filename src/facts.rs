// SPDX-License-Identifier: GPL-3.0-only

//! The unchanging half of the load page: what the machine is, beside what it is doing.
//!
//! Both references carry it - `ref/taskman/cpu.png` and `ref/mission-center/cpu-dark.png` -
//! and it is the one part of that page WMDE had nothing in.
//!
//! Read once at start-up. A processor does not gain cores while the window is open, and the
//! model name and the cache sizes cost a `/proc/cpuinfo` parse that has no business running
//! every second. What does change - load, process and thread counts, uptime, the clock and
//! the descriptor count - is not here; the last two live in [`Live`] at the bottom of this
//! file, which reads two small files a tick and nothing else.
//!
//! Every source is world-readable, and a source that is missing means the row is not drawn.
//! `base_frequency` is the ordinary case: `intel_pstate` publishes it only in active mode,
//! and DMI, which knows it regardless, is `0400`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const CPU_DIR: &str = "/sys/devices/system/cpu";
const HWMON_DIR: &str = "/sys/class/hwmon";

/// What the machine is. Empty strings and `None` for anything the machine does not say.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// `model name` out of `/proc/cpuinfo`.
    pub model: String,
    /// Logical processors, which is what the per-core tiles count.
    pub cpus: usize,
    /// Distinct `physical id` values, or 1 when the machine does not say.
    pub sockets: usize,
    /// Level and total size over the whole machine, in bytes, smallest level first:
    /// `("L1", 2228224)`. Data and instruction caches of a level are summed together, the
    /// way both references print them.
    pub caches: Vec<(String, u64)>,
    /// `Yes` when the kernel says it is running under a hypervisor.
    pub virtualized: bool,
    /// What the processor can host, named the way the reference names it:
    /// `KVM / Intel VT-x`. Empty when the flags carry neither `vmx` nor `svm`.
    pub virtualization: String,
    /// `intel_cpufreq`, `acpi-cpufreq`, ... Empty when the kernel has no cpufreq at all,
    /// which is the usual state of a virtual machine.
    pub cpufreq_driver: String,
    /// `schedutil`, `powersave`, ...
    pub cpufreq_governor: String,
    /// Base clock in kHz, where the driver publishes it.
    pub base_khz: Option<u64>,
}

impl Facts {
    #[must_use]
    pub fn load() -> Self {
        let mut out = Self {
            sockets: 1,
            ..Self::default()
        };

        let mut flags = String::new();
        if let Ok(info) = std::fs::read_to_string("/proc/cpuinfo") {
            let mut sockets = BTreeSet::new();
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
                    "flags" if flags.is_empty() => {
                        out.virtualized = value.split_whitespace().any(|f| f == "hypervisor");
                        flags = value.to_owned();
                    }
                    _ => {}
                }
            }
            if !sockets.is_empty() {
                out.sockets = sockets.len();
            }
        }

        out.virtualization = virtualization(&flags);
        out.caches = caches();

        let cpufreq = Path::new(CPU_DIR).join("cpu0/cpufreq");
        out.cpufreq_driver = read_trimmed(&cpufreq.join("scaling_driver")).unwrap_or_default();
        out.cpufreq_governor = read_trimmed(&cpufreq.join("scaling_governor")).unwrap_or_default();
        out.base_khz = read_trimmed(&cpufreq.join("base_frequency")).and_then(|s| s.parse().ok());

        out
    }
}

/// What the processor can host, or an empty string when it can host nothing.
///
/// `vmx` and `svm` say the silicon has the extension; `/dev/kvm` says the kernel is offering
/// it. The reference prints both halves, and so does this.
fn virtualization(flags: &str) -> String {
    let extension = flags.split_whitespace().find_map(|f| match f {
        "vmx" => Some("Intel VT-x"),
        "svm" => Some("AMD-V"),
        _ => None,
    });
    match (Path::new("/dev/kvm").exists(), extension) {
        (true, Some(name)) => format!("KVM / {name}"),
        (false, Some(name)) => name.to_owned(),
        (true, None) => "KVM".to_owned(),
        (false, None) => String::new(),
    }
}

/// Cache totals over the whole machine, by level.
///
/// One cache is shared by several processors and is therefore listed under each of them.
/// `shared_cpu_list` names the set that shares it, so `(level, type, shared_cpu_list)` is the
/// identity of a physical cache and is what the walk deduplicates on - without it a machine
/// with 32 processors reports 32 times its real L1.
///
/// Data and instruction caches of a level are then summed into one row: both references print
/// a single `L1`, and telling a reader that L1d is 896 KiB answers a question nobody asked of
/// a task manager.
fn caches() -> Vec<(String, u64)> {
    let mut seen = BTreeSet::new();
    let mut total: BTreeMap<u32, u64> = BTreeMap::new();

    let Ok(cpus) = std::fs::read_dir(CPU_DIR) else {
        return Vec::new();
    };
    for cpu in cpus.flatten() {
        let Ok(indices) = std::fs::read_dir(cpu.path().join("cache")) else {
            continue;
        };
        for index in indices.flatten() {
            let path = index.path();
            let read = |name: &str| read_trimmed(&path.join(name));
            let (Some(level), Some(size)) = (read("level"), read("size")) else {
                continue;
            };
            let (Ok(level), Some(size)) = (level.parse::<u32>(), parse_size(&size)) else {
                continue;
            };
            let kind = read("type").unwrap_or_default();
            let shared = read("shared_cpu_list").unwrap_or_default();
            if seen.insert((level, kind, shared)) {
                *total.entry(level).or_default() += size;
            }
        }
    }

    total
        .into_iter()
        .map(|(level, size)| (format!("L{level}"), size))
        .collect()
}

/// `32K`, `4096K`, `36864K` - the only shapes sysfs writes, but the unit is read rather than
/// assumed, because a cache big enough for `M` is not far off.
fn parse_size(text: &str) -> Option<u64> {
    let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let value: u64 = digits.parse().ok()?;
    let scale = match text[digits.len()..].trim() {
        "" => 1,
        "K" | "KiB" => 1024,
        "M" | "MiB" => 1024 * 1024,
        "G" | "GiB" => 1024 * 1024 * 1024,
        _ => return None,
    };
    Some(value * scale)
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The figures of the panel that change while the window is open.
///
/// Kept apart from [`Facts`] because they are read every tick and everything in `Facts` is
/// read once. The paths are collected at start-up so a tick is file reads and nothing else.
pub struct Live {
    /// One per logical processor. Empty when the machine has no cpufreq, which is the usual
    /// state under a hypervisor - and then the speed row is simply not drawn.
    freq: Vec<PathBuf>,
    /// The package temperature input, one per socket. Empty on a machine whose processor
    /// driver registers no sensor - every virtual machine.
    temperature: Vec<PathBuf>,
}

impl Default for Live {
    fn default() -> Self {
        Self::new()
    }
}

impl Live {
    #[must_use]
    pub fn new() -> Self {
        let mut freq = Vec::new();
        if let Ok(cpus) = std::fs::read_dir(CPU_DIR) {
            for cpu in cpus.flatten() {
                // Only `cpuN`, never `cpufreq` or `cpuidle`, which sit in the same directory.
                let name = cpu.file_name();
                let Some(name) = name.to_str() else { continue };
                if !name.starts_with("cpu") || !name[3..].bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
                let path = cpu.path().join("cpufreq/scaling_cur_freq");
                if path.exists() {
                    freq.push(path);
                }
            }
        }
        freq.sort();
        Self {
            freq,
            temperature: cpu_sensors(Path::new(HWMON_DIR)),
        }
    }

    /// The processor's temperature in degrees Celsius: the hottest package, where there are
    /// several sockets.
    #[must_use]
    pub fn temperature(&self) -> Option<f32> {
        self.temperature
            .iter()
            .filter_map(|path| read_trimmed(path)?.parse::<i64>().ok())
            .max()
            .map(|millidegrees| millidegrees as f32 / 1000.0)
    }

    /// Current clock in kHz, averaged over the processors that answer.
    ///
    /// Averaged rather than taken from cpu0: on a machine that parks its cores, cpu0 alone
    /// says 800 MHz while the machine is at four gigahertz on the cores doing the work.
    #[must_use]
    pub fn speed_khz(&self) -> Option<u64> {
        let mut sum = 0u64;
        let mut count = 0u64;
        for path in &self.freq {
            if let Some(khz) = read_trimmed(path).and_then(|s| s.parse::<u64>().ok()) {
                sum += khz;
                count += 1;
            }
        }
        (count > 0).then(|| sum / count)
    }

    /// Open file descriptors, machine-wide - the reference's "Handles".
    ///
    /// Field one of `/proc/sys/fs/file-nr` is the allocated count; field two has been a
    /// constant zero since the slab allocator took over, and field three is the ceiling.
    #[must_use]
    pub fn handles() -> Option<u64> {
        std::fs::read_to_string("/proc/sys/fs/file-nr")
            .ok()?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }
}

/// The inputs that read the whole processor, under a `/sys/class/hwmon` or a test's stand-in.
///
/// `coretemp` (Intel) labels one input per socket `Package id N`, beside one per core.
/// `k10temp` (AMD) labels the die `Tdie` on the parts that report it apart and `Tctl`
/// everywhere; `Tctl` carries an offset on some parts, so `Tdie` is taken where it is there.
/// `zenpower` uses the same labels. Anything else - a board sensor, `acpitz` - is not the
/// processor and is not taken.
fn cpu_sensors(root: &Path) -> Vec<PathBuf> {
    let mut packages = Vec::new();
    let mut amd: Option<(u8, PathBuf)> = None;
    let Ok(entries) = std::fs::read_dir(root) else {
        return packages;
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    dirs.sort();
    for dir in dirs {
        let Some(name) = read_trimmed(&dir.join("name")) else {
            continue;
        };
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut labels: Vec<(String, PathBuf)> = files
            .flatten()
            .filter_map(|file| {
                let file = file.file_name();
                let stem = file.to_str()?.strip_suffix("_label")?;
                stem.starts_with("temp").then(|| stem.to_owned())
            })
            .filter_map(|stem| {
                let label = read_trimmed(&dir.join(format!("{stem}_label")))?;
                Some((label, dir.join(format!("{stem}_input"))))
            })
            .collect();
        labels.sort();
        match name.as_str() {
            "coretemp" => packages.extend(
                labels
                    .into_iter()
                    .filter(|(label, _)| label.starts_with("Package id"))
                    .map(|(_, input)| input),
            ),
            "k10temp" | "zenpower" => {
                for (label, input) in labels {
                    let rank = match label.as_str() {
                        "Tdie" => 0,
                        "Tctl" => 1,
                        _ => continue,
                    };
                    if amd.as_ref().is_none_or(|(best, _)| rank < *best) {
                        amd = Some((rank, input));
                    }
                }
            }
            _ => {}
        }
    }
    packages.extend(amd.map(|(_, input)| input));
    packages
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in `hwmon` tree: each sensor a directory with its name and labelled inputs.
    fn hwmon(tag: &str, sensors: &[(&str, &[(&str, &str)])]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("wmde-taskmgr-hwmon-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (n, (name, inputs)) in sensors.iter().enumerate() {
            let dir = root.join(format!("hwmon{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("name"), format!("{name}\n")).unwrap();
            for (i, (label, value)) in inputs.iter().enumerate() {
                std::fs::write(
                    dir.join(format!("temp{}_label", i + 1)),
                    format!("{label}\n"),
                )
                .unwrap();
                std::fs::write(
                    dir.join(format!("temp{}_input", i + 1)),
                    format!("{value}\n"),
                )
                .unwrap();
            }
        }
        root
    }

    /// Read off `.131`: `coretemp` with the package first and a line per core.
    #[test]
    fn intel_reads_the_package_not_a_core() {
        let root = hwmon(
            "intel",
            &[
                ("acpitz", &[]),
                ("nvme", &[("Composite", "40850")]),
                (
                    "coretemp",
                    &[
                        ("Package id 0", "52000"),
                        ("Core 0", "49000"),
                        ("Core 1", "55000"),
                    ],
                ),
            ],
        );
        let sensors = cpu_sensors(&root);
        assert_eq!(sensors.len(), 1);
        let live = Live {
            freq: Vec::new(),
            temperature: sensors,
        };
        assert_eq!(live.temperature(), Some(52.0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn amd_prefers_the_die_to_the_control_reading() {
        let root = hwmon(
            "amd",
            &[(
                "k10temp",
                &[("Tctl", "61000"), ("Tdie", "51000"), ("Tccd1", "48000")],
            )],
        );
        let live = Live {
            freq: Vec::new(),
            temperature: cpu_sensors(&root),
        };
        assert_eq!(live.temperature(), Some(51.0));
        let _ = std::fs::remove_dir_all(&root);

        let root = hwmon("amd-tctl", &[("k10temp", &[("Tctl", "61000")])]);
        assert_eq!(cpu_sensors(&root).len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A virtual machine: board sensors only, and no reading rather than a wrong one.
    #[test]
    fn no_processor_sensor_means_no_temperature() {
        let root = hwmon(
            "none",
            &[("acpitz", &[]), ("nvme", &[("Composite", "40850")])],
        );
        assert!(cpu_sensors(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_running_machine_describes_itself() {
        let facts = Facts::load();
        assert!(facts.cpus > 0, "a machine has at least one processor");
        assert!(facts.sockets >= 1);
        // A model name and cache geometry are not guaranteed - a container or a foreign
        // architecture may say nothing - so neither is asserted, only that the read is safe.
        let _ = facts.model;
        let _ = facts.caches;
        let _ = facts.virtualization;
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

    /// The reason the walk deduplicates: an L3 shared by every processor would otherwise be
    /// counted once per processor, and a 36 MiB cache would read as better than a gigabyte.
    #[test]
    fn a_shared_cache_is_counted_once() {
        let facts = Facts::load();
        for (level, size) in &facts.caches {
            assert!(*size > 0, "{level} is empty");
            assert!(
                *size < 4 * 1024 * 1024 * 1024,
                "{level} is {size} bytes - the shared instances were summed"
            );
        }
    }

    #[test]
    fn sysfs_cache_sizes_carry_their_unit() {
        assert_eq!(parse_size("32K"), Some(32 * 1024));
        assert_eq!(parse_size("36864K"), Some(36864 * 1024));
        assert_eq!(parse_size("12M"), Some(12 * 1024 * 1024));
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("what"), None);
    }

    /// Both halves are named, and the extension alone is enough when the module is not there.
    #[test]
    fn virtualization_names_what_it_finds() {
        assert!(virtualization("fpu vme de vmx pse").contains("Intel VT-x"));
        assert!(virtualization("fpu svm pse").contains("AMD-V"));
        assert_eq!(virtualization("fpu vme de pse"), {
            if Path::new("/dev/kvm").exists() {
                "KVM".to_owned()
            } else {
                String::new()
            }
        });
    }

    /// Reading the live half must be safe wherever it runs, including a machine with no
    /// cpufreq at all - the answer is then `None`, never a zero pretending to be a reading.
    #[test]
    fn the_live_half_answers_or_stays_silent() {
        let live = Live::new();
        match live.speed_khz() {
            Some(khz) => assert!(khz > 0, "a processor that answers is running"),
            None => assert!(live.freq.is_empty()),
        }
        if let Some(handles) = Live::handles() {
            assert!(handles > 0, "a running system holds descriptors open");
        }
    }
}
