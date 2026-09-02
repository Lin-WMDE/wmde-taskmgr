// SPDX-License-Identifier: GPL-3.0-only

//! Sampling layer: one walk of `/proc` per tick, turned into one [`Process`] per process.
//!
//! The rules are the ones the system monitor applet's collector follows
//! (`.doc/sysmon/03-data-sources.md`), adapted to the different scale - hundreds of small
//! files instead of five aggregate ones:
//!
//! - No `sysinfo`, no `procfs`. Both walk the whole of `/proc` and allocate per call.
//! - No toolkit types, no localization. Nothing here knows what a widget is.
//! - Nothing allocates in the steady state. A process's name and command line are read once
//!   and kept; only the numbers are rewritten each tick. Buffers are reused.
//! - An unreadable file gives a missing value, never a zero: a zero would read as "this
//!   process did no I/O", which is a different claim from "we were not allowed to look".
//!
//! Four files per process. What each one costs, measured over 1657 processes as an ordinary
//! user: `stat` 16.9 ms, `status` 29.5 ms, `cmdline` 16.7 ms, `io` 11.2 ms. `io` was refused
//! for 1044 of them, hence [`Process::disk`] being an `Option`.
//!
//! `smaps_rollup` is deliberately absent. It is the only source of PSS and of private memory,
//! and it costs 1725 ms for the same walk while refusing 1063 of 1657 processes without root.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::time::Instant;

/// Fields of `/proc/PID/stat` after the closing parenthesis of `comm`, zero-based.
/// `stat(5)` numbers them from 1 including `pid` and `comm`, so each is three lower here.
const STAT_PPID: usize = 1;
const STAT_UTIME: usize = 11;
const STAT_STIME: usize = 12;
const STAT_THREADS: usize = 17;
const STAT_STARTTIME: usize = 19;

/// One process, as the list shows it.
///
/// The identity half - pid, start time, name, command line - is read once. The numbers are
/// rewritten every tick.
#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    /// Start time in clock ticks since boot. Together with the pid it identifies the process:
    /// a recycled pid gets a different start time, and the cached name must not follow it.
    pub starttime: u64,
    pub name: Box<str>,
    /// Full command line, arguments separated by spaces. Empty for a kernel thread.
    pub cmdline: Box<str>,

    pub threads: u32,
    /// Resident set, KiB. Counts a shared page once per process holding it, so the column
    /// sums to more than the machine has.
    pub rss: u64,
    /// Resident memory that is not program text, KiB: `RssAnon + max(0, RssFile - VmExe -
    /// VmLib)`. Subtracting virtual text sizes from a resident figure takes away at least as
    /// much as the resident text really is, which makes this a lower bound and never an
    /// overstatement.
    pub data: u64,
    /// Swapped out, KiB. Never part of `rss` or `data`.
    pub swap: u64,
    /// Share of the machine's whole capacity over the last tick, 0.0 to 1.0. One core fully
    /// busy on a four-core machine is 0.25, the way the reference counts it.
    pub cpu: f32,
    /// Bytes per second actually moved to and from storage, or `None` when `/proc/PID/io`
    /// was refused - which is every process of another user.
    pub disk: Option<[f64; 2]>,

    /// `utime + stime` at the previous tick.
    prev_cpu: u64,
    /// `read_bytes`, `write_bytes` at the previous tick.
    prev_io: Option<[u64; 2]>,
    /// Tick counter of the last walk that saw this process; anything older is gone.
    seen: u64,
}

impl Process {
    /// A kernel thread has no command line of its own.
    #[must_use]
    pub fn is_kernel_thread(&self) -> bool {
        self.cmdline.is_empty()
    }

    /// A process that no walk produced, for the grouping tests. The bookkeeping fields stay
    /// private so that nothing outside this module can pretend to have sampled anything.
    #[cfg(test)]
    #[must_use]
    pub fn stub(pid: u32, ppid: u32, name: &str) -> Self {
        Self {
            pid,
            ppid,
            uid: 1000,
            starttime: 0,
            name: name.into(),
            cmdline: format!("/usr/bin/{name}").into_boxed_str(),
            threads: 1,
            rss: 0,
            data: 0,
            swap: 0,
            cpu: 0.0,
            disk: None,
            prev_cpu: 0,
            prev_io: None,
            seen: 0,
        }
    }
}

/// The walk. Holds every process it has seen, so names and command lines are read once.
pub struct Probe {
    procs: HashMap<u32, Process>,
    /// One buffer per file. They are read in the same pass and parsed side by side, so a
    /// single shared buffer would mean copying one of them back out again.
    buf_stat: Vec<u8>,
    buf_status: Vec<u8>,
    buf_io: Vec<u8>,
    buf_cmdline: Vec<u8>,
    path: String,
    /// Sum of every field of the aggregate `cpu` line, previous tick.
    prev_total_cpu: u64,
    last: Option<Instant>,
    tick: u64,
    /// True once a `/proc/PID/io` read has been refused, so the warning is logged once.
    warned_io: bool,
}

impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe {
    #[must_use]
    pub fn new() -> Self {
        Self {
            procs: HashMap::new(),
            buf_stat: Vec::with_capacity(512),
            buf_status: Vec::with_capacity(2048),
            buf_io: Vec::with_capacity(512),
            buf_cmdline: Vec::with_capacity(1024),
            path: String::with_capacity(64),
            prev_total_cpu: 0,
            last: None,
            tick: 0,
            warned_io: false,
        }
    }

    /// Walk `/proc` once. Processes that have gone are dropped; the rest are updated in place.
    pub fn sample(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        let now = Instant::now();
        let dt = self.last.replace(now).map_or(0.0, |prev| {
            let secs = now.duration_since(prev).as_secs_f64();
            // A suspended machine would otherwise multiply every rate by the time it slept.
            if secs > 0.0 && secs < 60.0 { secs } else { 0.0 }
        });

        // CPU share is measured against the machine's whole capacity, so the denominator is
        // the aggregate line's own delta. That already sums every core, which is why no core
        // count and no USER_HZ appear anywhere here.
        let total_cpu = self.read_total_cpu();
        let cpu_delta = total_cpu.saturating_sub(self.prev_total_cpu);
        self.prev_total_cpu = total_cpu;

        let Ok(dir) = std::fs::read_dir("/proc") else {
            tracing::warn!("cannot read /proc; the process list stays as it was");
            return;
        };

        for entry in dir.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            self.update(pid, dt, cpu_delta);
        }

        let tick = self.tick;
        self.procs.retain(|_, p| p.seen == tick);
    }

    /// Every process the last walk found, in no particular order.
    pub fn processes(&self) -> impl Iterator<Item = &Process> {
        self.procs.values()
    }

    #[must_use]
    pub fn get(&self, pid: u32) -> Option<&Process> {
        self.procs.get(&pid)
    }

    fn update(&mut self, pid: u32, dt: f64, cpu_delta: u64) {
        if !read_into(&mut self.path, &mut self.buf_stat, pid, "stat") {
            return;
        }
        let Some(stat) = parse_stat(&self.buf_stat) else {
            return;
        };

        // A pid can be handed to a new process between two walks. The start time is what
        // tells the two apart, and the cached name and command line must not survive that.
        let fresh = match self.procs.get(&pid) {
            Some(p) => p.starttime != stat.starttime,
            None => true,
        };
        if fresh {
            let name = stat.comm.to_owned().into_boxed_str();
            let cmdline = if read_into(&mut self.path, &mut self.buf_cmdline, pid, "cmdline") {
                parse_cmdline(&self.buf_cmdline)
            } else {
                Box::default()
            };
            self.procs.insert(
                pid,
                Process {
                    pid,
                    ppid: stat.ppid,
                    uid: 0,
                    starttime: stat.starttime,
                    name,
                    cmdline,
                    threads: 0,
                    rss: 0,
                    data: 0,
                    swap: 0,
                    cpu: 0.0,
                    disk: None,
                    prev_cpu: stat.cpu_ticks,
                    prev_io: None,
                    seen: 0,
                },
            );
        }

        let status = read_into(&mut self.path, &mut self.buf_status, pid, "status")
            .then(|| parse_status(&self.buf_status));
        let io = read_into(&mut self.path, &mut self.buf_io, pid, "io")
            .then(|| parse_io(&self.buf_io));
        if io.is_none() && !self.warned_io {
            self.warned_io = true;
            tracing::warn!(
                "/proc/PID/io is not readable for other users' processes; \
                 the disk column stays empty for them"
            );
        }

        let tick = self.tick;
        let Some(p) = self.procs.get_mut(&pid) else {
            return;
        };
        p.seen = tick;
        p.ppid = stat.ppid;
        p.threads = stat.threads;

        p.cpu = if cpu_delta > 0 {
            let used = stat.cpu_ticks.saturating_sub(p.prev_cpu);
            (used as f64 / cpu_delta as f64) as f32
        } else {
            0.0
        };
        p.prev_cpu = stat.cpu_ticks;

        if let Some(s) = status {
            p.uid = s.uid;
            p.rss = s.rss;
            p.swap = s.swap;
            p.data = s.anon + s.file.saturating_sub(s.exe + s.lib);
        }

        p.disk = match (io, p.prev_io, dt > 0.0) {
            (Some(cur), Some(prev), true) => {
                p.prev_io = Some(cur);
                Some([
                    cur[0].saturating_sub(prev[0]) as f64 / dt,
                    cur[1].saturating_sub(prev[1]) as f64 / dt,
                ])
            }
            (Some(cur), _, _) => {
                p.prev_io = Some(cur);
                Some([0.0, 0.0])
            }
            (None, _, _) => None,
        };
    }

    /// Sum of every field of the aggregate `cpu` line: the machine's whole capacity per tick.
    ///
    /// Read 256 bytes deep on purpose. The aggregate line fits, and the kernel does not
    /// format the per-core lines that follow it unless they are asked for.
    fn read_total_cpu(&mut self) -> u64 {
        let Ok(mut f) = File::open("/proc/stat") else {
            tracing::warn!("cannot read /proc/stat; the CPU column stays empty");
            return 0;
        };
        let mut head = [0u8; 256];
        let Ok(n) = f.read(&mut head) else {
            return 0;
        };
        let Some(line) = head[..n].split(|b| *b == b'\n').next() else {
            return 0;
        };
        let Some(rest) = line.strip_prefix(b"cpu ") else {
            return 0;
        };
        fields(rest).map(atou64).sum()
    }
}

/// Read one `/proc/<pid>/<file>` into `buf`, replacing what was there.
///
/// A free function so that each buffer is borrowed on its own: the four files are parsed side
/// by side, and a method on `&mut self` would lock all of them at once.
fn read_into(path: &mut String, buf: &mut Vec<u8>, pid: u32, file: &str) -> bool {
    use std::fmt::Write as _;
    path.clear();
    let _ = write!(path, "/proc/{pid}/{file}");
    let Ok(mut f) = File::open(&*path) else {
        return false;
    };
    buf.clear();
    // A process can exit between the open and the read. That is the ordinary case for a list
    // that walks a live system, not something to log.
    f.read_to_end(buf).is_ok()
}

struct Stat<'a> {
    comm: &'a str,
    ppid: u32,
    threads: u32,
    starttime: u64,
    cpu_ticks: u64,
}

/// `comm` is the process's own name and may hold spaces and parentheses, so the split point
/// is the LAST `)` in the line, never the first.
fn parse_stat(raw: &[u8]) -> Option<Stat<'_>> {
    let open = raw.iter().position(|b| *b == b'(')?;
    let close = raw.iter().rposition(|b| *b == b')')?;
    if close <= open {
        return None;
    }
    let comm = std::str::from_utf8(&raw[open + 1..close]).ok()?;
    let rest = raw.get(close + 2..)?;
    // Walked forward with `nth` rather than collected: the indices only ever increase, and a
    // Vec here would be one allocation per process per tick.
    let mut f = fields(rest);
    let ppid = atou64(f.nth(STAT_PPID)?) as u32;
    let utime = atou64(f.nth(STAT_UTIME - STAT_PPID - 1)?);
    let stime = atou64(f.next()?);
    let threads = atou64(f.nth(STAT_THREADS - STAT_STIME - 1)?) as u32;
    let starttime = atou64(f.nth(STAT_STARTTIME - STAT_THREADS - 1)?);
    Some(Stat {
        comm,
        ppid,
        threads,
        starttime,
        cpu_ticks: utime + stime,
    })
}

#[derive(Default)]
struct Status {
    uid: u32,
    rss: u64,
    anon: u64,
    file: u64,
    exe: u64,
    lib: u64,
    swap: u64,
}

/// All seven numbers come out of one read. `VmExe` and `VmLib` are virtual sizes, not
/// resident ones; they are here only as the ceiling subtracted from `RssFile`.
fn parse_status(raw: &[u8]) -> Status {
    let mut out = Status::default();
    let mut found = 0;
    for line in raw.split(|b| *b == b'\n') {
        let Some((key, value)) = split_key(line) else {
            continue;
        };
        let slot = match key {
            b"Uid" => {
                // Real, effective, saved, filesystem. The first is the one that owns it.
                out.uid = fields(value).next().map_or(0, atou64) as u32;
                found += 1;
                continue;
            }
            b"VmRSS" => &mut out.rss,
            b"RssAnon" => &mut out.anon,
            b"RssFile" => &mut out.file,
            b"VmExe" => &mut out.exe,
            b"VmLib" => &mut out.lib,
            b"VmSwap" => &mut out.swap,
            _ => continue,
        };
        *slot = fields(value).next().map_or(0, atou64);
        found += 1;
        if found == 7 {
            break;
        }
    }
    out
}

/// `read_bytes` and `write_bytes` - what actually reached storage, not what the process asked
/// for. `rchar` and `wchar` would count reads served from cache as disk traffic.
fn parse_io(raw: &[u8]) -> [u64; 2] {
    let mut out = [0u64; 2];
    for line in raw.split(|b| *b == b'\n') {
        let Some((key, value)) = split_key(line) else {
            continue;
        };
        match key {
            b"read_bytes" => out[0] = fields(value).next().map_or(0, atou64),
            b"write_bytes" => out[1] = fields(value).next().map_or(0, atou64),
            _ => {}
        }
    }
    out
}

/// Arguments are separated by NUL and the line usually ends with one; a trailing empty
/// argument would otherwise leave a space at the end of every command line.
fn parse_cmdline(raw: &[u8]) -> Box<str> {
    let trimmed = raw.strip_suffix(b"\0").unwrap_or(raw);
    let mut out = String::with_capacity(trimmed.len());
    for (i, part) in trimmed.split(|b| *b == 0).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&String::from_utf8_lossy(part));
    }
    out.into_boxed_str()
}

/// Split a `Key:\tvalue` line. Returns nothing for a line without a colon.
fn split_key(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let colon = line.iter().position(|b| *b == b':')?;
    Some((&line[..colon], &line[colon + 1..]))
}

/// Whitespace-separated fields, without the empty ones a plain split would produce.
fn fields(line: &[u8]) -> impl Iterator<Item = &[u8]> {
    line.split(|b| b.is_ascii_whitespace())
        .filter(|f| !f.is_empty())
}

/// Leading digits as a number. Anything else in the field ends the number rather than
/// poisoning it, so a unit suffix such as `kB` is simply not read.
fn atou64(field: &[u8]) -> u64 {
    let mut out = 0u64;
    for b in field {
        if !b.is_ascii_digit() {
            break;
        }
        out = out.wrapping_mul(10).wrapping_add(u64::from(b - b'0'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_splits_on_the_last_parenthesis() {
        // A process free to name itself "(ugly) name)" is the case a first-parenthesis split
        // gets wrong.
        let raw = b"42 ((ugly) name)) S 7 42 42 0 -1 4194304 100 0 0 0 11 22 0 0 20 0 3 0 999 0";
        let stat = parse_stat(raw).expect("parses");
        assert_eq!(stat.comm, "(ugly) name)");
        assert_eq!(stat.ppid, 7);
        assert_eq!(stat.threads, 3);
        assert_eq!(stat.starttime, 999);
        assert_eq!(stat.cpu_ticks, 33);
    }

    #[test]
    fn status_yields_the_data_metric() {
        let raw = b"Name:\tfoo\nUid:\t1000\t1000\t1000\t1000\nVmExe:\t   100 kB\n\
                    VmLib:\t   200 kB\nVmRSS:\t  5000 kB\nRssAnon:\t  1000 kB\n\
                    RssFile:\t  4000 kB\nVmSwap:\t    50 kB\n";
        let s = parse_status(raw);
        assert_eq!(s.uid, 1000);
        assert_eq!(s.rss, 5000);
        assert_eq!(s.swap, 50);
        // 1000 + (4000 - 100 - 200)
        assert_eq!(s.anon + s.file.saturating_sub(s.exe + s.lib), 4700);
    }

    #[test]
    fn data_never_goes_below_zero() {
        // VmExe and VmLib are virtual, so on a process whose file pages are mostly paged out
        // they exceed RssFile. The metric must floor at the anonymous part, not wrap.
        let raw = b"RssAnon:\t  1000 kB\nRssFile:\t   100 kB\nVmExe:\t  9000 kB\nVmLib:\t 9000 kB\n";
        let s = parse_status(raw);
        assert_eq!(s.anon + s.file.saturating_sub(s.exe + s.lib), 1000);
    }

    #[test]
    fn cmdline_joins_arguments_without_a_trailing_space() {
        assert_eq!(&*parse_cmdline(b"/usr/bin/foo\0--bar\0baz\0"), "/usr/bin/foo --bar baz");
        assert_eq!(&*parse_cmdline(b""), "");
    }

    #[test]
    fn atou64_stops_at_the_unit() {
        assert_eq!(atou64(b"1234"), 1234);
        assert_eq!(atou64(b"kB"), 0);
        assert_eq!(atou64(b"12kB"), 12);
    }

    #[test]
    fn walks_the_running_system() {
        let mut probe = Probe::new();
        probe.sample();
        assert!(probe.processes().count() > 1, "a running system has more than one process");

        let me = std::process::id();
        let this = probe.get(me).expect("the test process is in the list");
        assert_eq!(this.pid, me);
        assert!(this.rss > 0, "a running process holds resident memory");
        assert!(this.data > 0, "and some of it is not program text");
        assert!(this.data <= this.rss, "data is a part of the resident set");
        assert!(!this.is_kernel_thread());
        assert!(this.disk.is_some(), "our own io counters are readable");
    }
}
