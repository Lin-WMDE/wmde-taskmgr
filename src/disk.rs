// SPDX-License-Identifier: GPL-3.0-only

//! What a disk is doing beyond its rate, and what lives on it.
//!
//! Two halves. [`DiskStats`] reads `/proc/diskstats` every tick for every disk in the rail and
//! turns two of its counters into the page's active time and response time - counters the
//! layer in `wmde-sysmon` passes over, as it reads only the sectors. [`DiskFacts`] is read every tick for
//! the disk on screen only: its mount points, the space its filesystems hold, and whether the
//! system and the swap sit on it.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Instant;

const DISKSTATS: &str = "/proc/diskstats";
const BLOCK_DIR: &str = "/sys/block";
const MOUNTINFO: &str = "/proc/self/mountinfo";
const SWAPS: &str = "/proc/swaps";

/// The counters of one disk that the page turns into time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Counters {
    reads: u64,
    ms_reading: u64,
    writes: u64,
    ms_writing: u64,
    /// Milliseconds the disk had at least one request in flight.
    io_ticks: u64,
}

/// One disk over the last interval.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Activity {
    /// Share of the interval the disk was busy, `0.0..=1.0`.
    pub active: f32,
    /// Milliseconds a request took on average; zero over an interval with none.
    pub response_ms: f32,
}

/// The previous reading of every disk, to take the next one against.
#[derive(Default)]
pub struct DiskStats {
    prev: HashMap<Box<str>, (Counters, Instant)>,
}

impl DiskStats {
    /// The activity of each of `names`, in their order. `None` on a disk's first tick and for
    /// a disk the table does not list.
    pub fn sample(&mut self, names: &[&str], now: Instant) -> Vec<Option<Activity>> {
        let table = std::fs::read_to_string(DISKSTATS).unwrap_or_default();
        self.sample_from(&table, names, now)
    }

    fn sample_from(&mut self, table: &str, names: &[&str], now: Instant) -> Vec<Option<Activity>> {
        let read = counters(table);
        let out = names
            .iter()
            .map(|name| {
                let current = *read.get(*name)?;
                let (prev, at) = self.prev.get(*name).copied()?;
                activity(prev, current, now.saturating_duration_since(at).as_millis())
            })
            .collect();
        self.prev = names
            .iter()
            .filter_map(|name| Some(((*name).into(), (*read.get(*name)?, now))))
            .collect();
        out
    }
}

/// The counters of every line of `/proc/diskstats`, by device name.
///
/// After major, minor and name: reads completed, merged, sectors, milliseconds reading,
/// writes completed, merged, sectors, milliseconds writing, in flight, milliseconds doing
/// I/O. Lines from kernels that print fewer fields are skipped.
fn counters(table: &str) -> HashMap<&str, Counters> {
    table
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let number = |i: usize| fields.get(i)?.parse::<u64>().ok();
            Some((
                *fields.get(2)?,
                Counters {
                    reads: number(3)?,
                    ms_reading: number(6)?,
                    writes: number(7)?,
                    ms_writing: number(10)?,
                    io_ticks: number(12)?,
                },
            ))
        })
        .collect()
}

/// Active time and response time over an interval of `elapsed_ms`. `None` for an empty
/// interval and for counters that went backwards - a disk that was replaced under the name.
fn activity(prev: Counters, now: Counters, elapsed_ms: u128) -> Option<Activity> {
    if elapsed_ms == 0 {
        return None;
    }
    let delta = |a: u64, b: u64| b.checked_sub(a);
    let busy = delta(prev.io_ticks, now.io_ticks)?;
    let requests = delta(prev.reads, now.reads)? + delta(prev.writes, now.writes)?;
    let waited = delta(prev.ms_reading, now.ms_reading)? + delta(prev.ms_writing, now.ms_writing)?;
    Some(Activity {
        active: (busy as f64 / elapsed_ms as f64).clamp(0.0, 1.0) as f32,
        response_ms: if requests == 0 {
            0.0
        } else {
            waited as f32 / requests as f32
        },
    })
}

/// What lives on the disk on screen.
#[derive(Debug, Default, PartialEq)]
pub struct DiskFacts {
    /// Where its filesystems are mounted, in the order of the mount table, each once.
    pub mounts: Vec<String>,
    /// Bytes its mounted filesystems hold in all, each filesystem counted once. `None` when
    /// nothing on it is mounted.
    pub formatted: Option<u64>,
    /// `/` is on it.
    pub system: bool,
    /// A swap partition or swap file is on it.
    pub swap: bool,
}

impl DiskFacts {
    pub fn read(disk: &str) -> Self {
        let mountinfo = std::fs::read_to_string(MOUNTINFO).unwrap_or_default();
        let swaps = std::fs::read_to_string(SWAPS).unwrap_or_default();
        Self::read_at(
            Path::new(BLOCK_DIR),
            disk,
            &mountinfo,
            &swaps,
            &|path| {
                std::fs::metadata(path)
                    .ok()
                    .map(|meta| split_dev(meta.rdev()))
            },
            &statvfs_bytes,
        )
    }

    /// The same against stand-ins: a `/sys/block`, the two tables, how a device path resolves
    /// to its `major:minor`, and how big the filesystem mounted at a path is.
    fn read_at(
        block: &Path,
        disk: &str,
        mountinfo: &str,
        swaps: &str,
        device_of: &dyn Fn(&str) -> Option<(u32, u32)>,
        size_of: &dyn Fn(&str) -> Option<u64>,
    ) -> Self {
        let members = members(block, disk);
        let mounts = mount_table(mountinfo);

        let mut facts = Self::default();
        let mut counted = HashSet::new();
        for (point, source) in &mounts {
            let Some(device) = source.strip_prefix("/dev/").and_then(|_| device_of(source)) else {
                continue;
            };
            if !members.contains(&device) {
                continue;
            }
            if !facts.mounts.contains(point) {
                facts.mounts.push(point.clone());
            }
            facts.system |= point == "/";
            // Subvolumes of one btrfs share its device and its size: one count.
            if counted.insert(device)
                && let Some(bytes) = size_of(point)
            {
                *facts.formatted.get_or_insert(0) += bytes;
            }
        }

        facts.swap = swaps.lines().skip(1).any(|line| {
            let mut fields = line.split_whitespace();
            let (Some(name), Some(kind)) = (fields.next(), fields.next()) else {
                return false;
            };
            let name = unescape(name);
            let device = if kind == "partition" {
                device_of(&name)
            } else {
                // A swap file lives on whatever filesystem holds the path: the mount with
                // the longest mount point that is a prefix of it.
                mounts
                    .iter()
                    .filter(|(point, _)| under(&name, point))
                    .max_by_key(|(point, _)| point.len())
                    .and_then(|(_, source)| {
                        source.strip_prefix("/dev/").and_then(|_| device_of(source))
                    })
            };
            device.is_some_and(|device| members.contains(&device))
        });
        facts
    }
}

/// `path` is `point` or inside it.
fn under(path: &str, point: &str) -> bool {
    point == "/"
        || path == point
        || path
            .strip_prefix(point)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The disk's own `major:minor`, its partitions', and those of every device-mapper device
/// built on any of them - LUKS on a partition, LVM on LUKS - however deep.
fn members(block: &Path, disk: &str) -> HashSet<(u32, u32)> {
    let dev = |dir: &Path| {
        let text = std::fs::read_to_string(dir.join("dev")).ok()?;
        let (major, minor) = text.trim().split_once(':')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
    };
    let dir = block.join(disk);
    let mut names: HashSet<String> = HashSet::from([disk.to_owned()]);
    let mut out: HashSet<(u32, u32)> = dev(&dir).into_iter().collect();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if entry.path().join("partition").exists() {
            names.insert(entry.file_name().to_string_lossy().into_owned());
            out.extend(dev(&entry.path()));
        }
    }

    // Device-mapper devices are whole devices in /sys/block whose slaves name what they sit
    // on. Taken in rounds until a round adds nothing.
    let mapped: Vec<_> = std::fs::read_dir(block)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("dm-"))
        .map(|entry| entry.path())
        .collect();
    loop {
        let mut grew = false;
        for path in &mapped {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            let Some(name) = name.filter(|name| !names.contains(name)) else {
                continue;
            };
            let on_us = std::fs::read_dir(path.join("slaves"))
                .into_iter()
                .flatten()
                .flatten()
                .any(|slave| names.contains(&*slave.file_name().to_string_lossy()));
            if on_us {
                names.insert(name);
                out.extend(dev(path));
                grew = true;
            }
        }
        if !grew {
            return out;
        }
    }
}

/// Mount point and source of every line of a `mountinfo` table, in its order.
///
/// The fields before ` - ` are the mount's own, the mount point fifth; after it come the
/// filesystem type and the source. Spaces and the like are written as octal escapes.
fn mount_table(mountinfo: &str) -> Vec<(String, String)> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (own, rest) = line.split_once(" - ")?;
            let point = own.split_whitespace().nth(4)?;
            let source = rest.split_whitespace().nth(1)?;
            Some((unescape(point), unescape(source)))
        })
        .collect()
}

/// Undoes the kernel's octal escapes: `\040` is a space.
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let value =
                (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(value);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `major:minor` of a Linux `dev_t`.
fn split_dev(dev: u64) -> (u32, u32) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major as u32, minor as u32)
}

/// Bytes the filesystem mounted at `point` holds in all.
fn statvfs_bytes(point: &str) -> Option<u64> {
    let path = CString::new(point).ok()?;
    // SAFETY: `statvfs` is all integers, and zero is a valid value for each.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a struct for the call to fill.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some(stat.f_blocks.saturating_mul(stat.f_frsize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn active_time_and_response_come_from_two_readings() {
        let table = |reads: u64, ms_r: u64, writes: u64, ms_w: u64, ticks: u64| {
            format!(
                " 259       0 nvme0n1 {reads} 0 0 {ms_r} {writes} 0 0 {ms_w} 0 {ticks} 0 0 0 0 0 0 0\n"
            )
        };
        let mut stats = DiskStats::default();
        let start = Instant::now();
        assert_eq!(
            stats.sample_from(&table(100, 50, 40, 30, 1000), &["nvme0n1"], start),
            [None],
            "one reading is not an interval"
        );
        // 250 ms busy out of a second, 20 requests that waited 10 ms between them.
        let next = stats.sample_from(
            &table(110, 55, 50, 35, 1250),
            &["nvme0n1", "sda"],
            start + Duration::from_secs(1),
        );
        assert_eq!(
            next[0],
            Some(Activity {
                active: 0.25,
                response_ms: 0.5
            })
        );
        assert_eq!(next[1], None, "a disk the table does not list");
    }

    #[test]
    fn an_idle_disk_answers_in_no_time() {
        let a = Counters {
            reads: 10,
            ..Counters::default()
        };
        let idle = activity(a, a, 1000).unwrap();
        assert_eq!(idle.active, 0.0);
        assert_eq!(idle.response_ms, 0.0);
        // A counter that went back is a different disk under the same name.
        let back = Counters::default();
        assert_eq!(activity(a, back, 1000), None);
    }

    /// `/sys/block` with `nvme0n1` (partitions p1, p2, p3), LUKS on p2 and LVM on that.
    fn block(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("wmde-taskmgr-disk-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dev = |path: &Path, dev: &str| {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("dev"), format!("{dev}\n")).unwrap();
        };
        dev(&root.join("nvme0n1"), "259:0");
        for (part, n) in [("nvme0n1p1", 1), ("nvme0n1p2", 2), ("nvme0n1p3", 3)] {
            let dir = root.join("nvme0n1").join(part);
            dev(&dir, &format!("259:{n}"));
            fs::write(dir.join("partition"), format!("{n}\n")).unwrap();
        }
        dev(&root.join("sda"), "8:0");
        dev(&root.join("sda/sda1"), "8:1");
        fs::write(root.join("sda/sda1/partition"), "1\n").unwrap();
        dev(&root.join("dm-0"), "254:0");
        fs::create_dir_all(root.join("dm-0/slaves")).unwrap();
        symlink(
            "../../nvme0n1/nvme0n1p2",
            root.join("dm-0/slaves/nvme0n1p2"),
        )
        .unwrap();
        dev(&root.join("dm-1"), "254:1");
        fs::create_dir_all(root.join("dm-1/slaves")).unwrap();
        symlink("../../dm-0", root.join("dm-1/slaves/dm-0")).unwrap();
        root
    }

    #[test]
    fn a_disk_holds_its_partitions_and_what_is_mapped_on_them() {
        let root = block("members");
        let members = members(&root, "nvme0n1");
        for dev in [(259, 0), (259, 1), (259, 2), (259, 3), (254, 0), (254, 1)] {
            assert!(members.contains(&dev), "{dev:?}");
        }
        assert!(!members.contains(&(8, 0)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn mounts_size_system_and_swap_are_the_disks() {
        let root = block("facts");
        // `/` and `/home` are subvolumes of one btrfs on LVM on LUKS; `/boot` the first
        // partition; a USB stick elsewhere; a swap partition and a swap file.
        let mountinfo = "\
            25 1 0:24 /@ / rw,noatime shared:1 - btrfs /dev/mapper/vg-root rw\n\
            26 25 0:24 /@home /home rw,noatime shared:2 - btrfs /dev/mapper/vg-root rw\n\
            27 25 259:1 / /boot rw,relatime shared:3 - vfat /dev/nvme0n1p1 rw\n\
            28 25 8:1 / /run/media/user/My\\040Stick rw shared:4 - vfat /dev/sda1 rw\n\
            29 25 0:5 / /proc rw - proc proc rw\n";
        let swaps = "Filename\tType\tSize\tUsed\tPriority\n\
                     /dev/nvme0n1p3\tpartition\t8388604\t0\t-2\n";
        let device_of = |path: &str| match path {
            "/dev/mapper/vg-root" => Some((254, 1)),
            "/dev/nvme0n1p1" => Some((259, 1)),
            "/dev/nvme0n1p3" => Some((259, 3)),
            "/dev/sda1" => Some((8, 1)),
            _ => None,
        };
        let size_of = |point: &str| match point {
            "/" | "/home" => Some(400 << 30),
            "/boot" => Some(1 << 30),
            _ => Some(16 << 30),
        };
        let facts = DiskFacts::read_at(&root, "nvme0n1", mountinfo, swaps, &device_of, &size_of);
        assert_eq!(facts.mounts, ["/", "/home", "/boot"]);
        assert_eq!(facts.formatted, Some((400 << 30) + (1 << 30)));
        assert!(facts.system);
        assert!(facts.swap);

        // The stick: its own mount, the space in its name unescaped, and neither system nor
        // swap.
        let stick = DiskFacts::read_at(&root, "sda", mountinfo, swaps, &device_of, &size_of);
        assert_eq!(stick.mounts, ["/run/media/user/My Stick"]);
        assert!(!stick.system);
        assert!(!stick.swap);
        let _ = fs::remove_dir_all(&root);
    }

    /// A swap file is on the disk of the filesystem that holds it.
    #[test]
    fn a_swap_file_is_on_the_disk_of_its_filesystem() {
        let root = block("swapfile");
        let mountinfo = "\
            25 1 0:24 /@ / rw shared:1 - btrfs /dev/mapper/vg-root rw\n\
            30 25 0:31 /@swap /swap rw shared:5 - btrfs /dev/mapper/vg-root rw\n";
        let swaps = "Filename\tType\tSize\tUsed\tPriority\n/swap/swapfile\tfile\t4194300\t0\t-2\n";
        let device_of = |path: &str| (path == "/dev/mapper/vg-root").then_some((254, 1));
        let facts = DiskFacts::read_at(&root, "nvme0n1", mountinfo, swaps, &device_of, &|_| None);
        assert!(facts.swap);
        assert_eq!(facts.formatted, None, "no size answered");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dev_t_splits_into_major_and_minor() {
        // 259:2 as glibc's makedev builds it.
        let dev = (259_u64 << 8) | 2;
        assert_eq!(split_dev(dev), (259, 2));
        assert_eq!(split_dev((8 << 8) | 1), (8, 1));
    }
}
