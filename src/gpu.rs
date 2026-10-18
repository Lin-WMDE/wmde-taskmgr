// SPDX-License-Identifier: GPL-3.0-only

//! The graphics cards: which there are, and how busy each one is.
//!
//! Cards are found once, at start-up, under `/sys/class/drm`: a card does not come and go
//! while the window is open. How busy a card is has no common source - each driver publishes
//! its own, or none:
//!
//! | Driver | Utilization | Video memory | Shared memory |
//! |---|---|---|---|
//! | `nvidia` | NVML, `libnvidia-ml.so.1` opened at run time | NVML | none |
//! | `amdgpu` | `device/gpu_busy_percent` | `device/mem_info_vram_*` | `device/mem_info_gtt_*` |
//! | `i915` | idle residency, `gt/gt0/rc6_residency_ms` | none | DRM clients' `fdinfo` |
//! | `xe` | idle residency, `device/tile0/gt0/gtidle/idle_residency_ms` | none | as `i915` |
//!
//! NVML and `amdgpu` also say how busy the memory controller is. Temperature comes from NVML
//! for `nvidia` and from the card's own `hwmon` sensor for the drivers that register one
//! (`amdgpu`, `nouveau`); an integrated Intel card has none. Any other card - a virtual
//! one - is listed with its name and no reading. Nothing here needs root or starts a
//! process: `nvidia-smi` would cost a process every tick.

use std::ffi::{CString, OsStr, c_char, c_int, c_uint, c_void};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

const DRM_DIR: &str = "/sys/class/drm";
const PROC_DIR: &str = "/proc";

/// PCI class `0x03xxxx`: a display controller.
const DISPLAY_CLASS: &str = "0x03";

/// One graphics card.
pub struct Card {
    /// `card0`.
    pub node: Box<str>,
    /// `nvidia`, `amdgpu`, `vmwgfx`; empty when no driver is bound.
    pub driver: Box<str>,
    /// The PCI address, `0000:01:00.0`.
    pub address: Box<str>,
    /// The card's device node in sysfs, for the name lookup.
    pub device: PathBuf,
    /// Share of the last interval the card was busy, `0.0..=1.0`. `None` when the driver
    /// publishes nothing, and for an idle-residency card until its second sample.
    pub busy: Option<f32>,
    /// Video memory in use and in all, in bytes. `None` where the card has none of its own
    /// or the driver does not say.
    pub memory: Option<(u64, u64)>,
    /// Share of the last interval the memory controller was busy, `0.0..=1.0`.
    pub memory_busy: Option<f32>,
    /// System memory the card holds, in bytes, and the most it may hold where the driver says.
    /// `amdgpu` reports both (GTT); for an Intel card it is the sum over the DRM clients this
    /// user can see, and has no limit but the machine's memory.
    pub shared: Option<(u64, Option<u64>)>,
    /// Degrees Celsius. `None` for a card without a sensor of its own.
    pub temperature: Option<f32>,
    source: Source,
    /// The card's `hwmon` temperature input, found once at start-up.
    thermal: Option<PathBuf>,
}

enum Source {
    None,
    /// An `nvmlDevice_t`, kept as an address so the card stays `Send`.
    Nvml(usize),
    Amdgpu {
        busy: PathBuf,
        used: PathBuf,
        total: PathBuf,
        memory_busy: PathBuf,
        gtt_used: PathBuf,
        gtt_total: PathBuf,
    },
    /// A cumulative idle counter in milliseconds, and the last reading of it.
    Idle {
        path: PathBuf,
        last: Option<(u64, Instant)>,
    },
}

pub struct Gpus {
    cards: Vec<Card>,
    nvml: Option<Nvml>,
    /// One read buffer for every counter file, so a tick does not allocate.
    buf: Vec<u8>,
    /// Where the DRM clients' `fdinfo` is read from: `/proc`, or a test's stand-in for it.
    proc: PathBuf,
}

impl Gpus {
    pub fn new() -> Self {
        Self::scan(Path::new(DRM_DIR), Nvml::open)
    }

    pub fn cards(&self) -> &[Card] {
        &self.cards
    }

    /// The cards under `root`, a `/sys/class/drm` or a test's stand-in for it. NVML is opened
    /// only when there is an NVIDIA card to ask about.
    fn scan(root: &Path, open_nvml: impl FnOnce() -> Option<Nvml>) -> Self {
        let mut cards: Vec<Card> = Vec::new();
        for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if card_number(name).is_none() {
                continue;
            }
            let card_dir = root.join(name);
            let link = card_dir.join("device");
            let Some(device) = display_function(&link) else {
                continue;
            };
            let Some(address) = device
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            if cards.iter().any(|card| *card.address == *address) {
                continue;
            }
            // The driver of the device the DRM node hangs off: `virtio_gpu` for a virtio card,
            // not the `virtio-pci` transport bound to the function above it.
            let driver = link_basename(&link.join("driver")).unwrap_or_default();
            let source = source_for(&driver, &card_dir, &device);
            let thermal = hwmon_temperature(&device);
            cards.push(Card {
                node: name.into(),
                driver: driver.into(),
                address: address.into(),
                device,
                busy: None,
                memory: None,
                memory_busy: None,
                shared: None,
                temperature: None,
                source,
                thermal,
            });
        }
        cards.sort_by_key(|card| card_number(&card.node));

        let nvml = cards
            .iter()
            .any(|card| &*card.driver == "nvidia")
            .then(open_nvml)
            .flatten();
        if let Some(nvml) = &nvml {
            for card in cards.iter_mut().filter(|card| &*card.driver == "nvidia") {
                if let Some(handle) = nvml.handle(&card.address) {
                    card.source = Source::Nvml(handle);
                }
            }
        }

        Self {
            cards,
            nvml,
            buf: Vec::with_capacity(32),
            proc: PathBuf::from(PROC_DIR),
        }
    }

    /// Reads every card's counters. `now` is taken by the caller, so a test can drive the
    /// idle-residency arithmetic of an Intel card with a clock of its own.
    pub fn sample(&mut self, now: Instant) {
        let Self {
            cards,
            nvml,
            buf,
            proc,
        } = self;
        for card in cards.iter_mut() {
            match &mut card.source {
                Source::None => {}
                Source::Nvml(handle) => {
                    if let Some(nvml) = nvml {
                        let rates = nvml.utilization(*handle);
                        card.busy = rates.map(|(gpu, _)| gpu);
                        card.memory_busy = rates.map(|(_, memory)| memory);
                        card.memory = nvml.memory(*handle);
                    }
                }
                Source::Amdgpu {
                    busy,
                    used,
                    total,
                    memory_busy,
                    gtt_used,
                    gtt_total,
                } => {
                    card.busy = read_percent(busy, buf);
                    card.memory_busy = read_percent(memory_busy, buf);
                    card.memory = read_pair(used, total, buf);
                    card.shared = read_pair(gtt_used, gtt_total, buf)
                        .map(|(used, total)| (used, Some(total)));
                }
                Source::Idle { path, last } => {
                    let idle = read_u64(path, buf);
                    card.busy = match (idle, *last) {
                        (Some(idle), Some((prev, at))) => {
                            let elapsed = now.saturating_duration_since(at).as_millis();
                            busy_from_idle(prev, idle, u64::try_from(elapsed).unwrap_or(u64::MAX))
                        }
                        _ => None,
                    };
                    *last = idle.map(|idle| (idle, now));
                }
            }
            card.temperature = match (&card.source, nvml.as_ref()) {
                (Source::Nvml(handle), Some(nvml)) => nvml.temperature(*handle),
                _ => card
                    .thermal
                    .as_deref()
                    .and_then(|path| read_u64(path, buf))
                    .map(|millidegrees| millidegrees as f32 / 1000.0),
            };
        }

        // An Intel card's share of the machine's memory has no counter of its own; it is
        // what its clients hold. Walked only when there is such a card to ask about.
        if cards.iter().any(is_intel) {
            let held = drm_memory(proc, buf);
            for card in cards.iter_mut().filter(|card| is_intel(card)) {
                card.shared = held
                    .iter()
                    .find(|(pdev, _)| **pdev == *card.address)
                    .map(|(_, bytes)| (*bytes, None));
            }
        }
    }
}

#[cfg(test)]
impl Card {
    /// A card with no source and no reading, for tests outside this module to fill in.
    pub fn bare() -> Self {
        Self {
            node: "card0".into(),
            driver: "".into(),
            address: "0000:00:02.0".into(),
            device: PathBuf::new(),
            busy: None,
            memory: None,
            memory_busy: None,
            shared: None,
            temperature: None,
            source: Source::None,
            thermal: None,
        }
    }
}

fn is_intel(card: &Card) -> bool {
    matches!(&*card.driver, "i915" | "xe")
}

/// `card0` -> `0`. `None` for a connector (`card0-HDMI-A-1`) or a render node (`renderD128`).
fn card_number(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("card")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The display-class PCI function behind a card's `device` link: the device itself, or the
/// function above it for a card that sits on a bus of its own - virtio-gpu hangs its node off
/// `0000:00:02.0/virtio0`. `None` for a platform device, simpledrm or a firmware framebuffer,
/// which has no PCI class anywhere.
fn display_function(link: &Path) -> Option<PathBuf> {
    let device = std::fs::canonicalize(link).ok()?;
    [Some(device.as_path()), device.parent()]
        .into_iter()
        .flatten()
        .find(|dir| read_text(&dir.join("class")).is_some_and(|c| c.starts_with(DISPLAY_CLASS)))
        .map(Path::to_path_buf)
}

/// Where the driver publishes how busy the card is, if it does.
fn source_for(driver: &str, card_dir: &Path, device: &Path) -> Source {
    match driver {
        "amdgpu" if device.join("gpu_busy_percent").exists() => Source::Amdgpu {
            busy: device.join("gpu_busy_percent"),
            used: device.join("mem_info_vram_used"),
            total: device.join("mem_info_vram_total"),
            memory_busy: device.join("mem_busy_percent"),
            gtt_used: device.join("mem_info_gtt_used"),
            gtt_total: device.join("mem_info_gtt_total"),
        },
        "i915" => idle_source(card_dir.join("gt/gt0/rc6_residency_ms")),
        "xe" => idle_source(device.join("tile0/gt0/gtidle/idle_residency_ms")),
        // `nvidia` is filled in once NVML is open; everything else publishes nothing.
        _ => Source::None,
    }
}

fn idle_source(path: PathBuf) -> Source {
    if path.exists() {
        Source::Idle { path, last: None }
    } else {
        Source::None
    }
}

/// The card's own temperature sensor, if its driver registers one under the PCI function:
/// the input labelled `edge` where the inputs are labelled (`amdgpu`: edge, junction,
/// memory - and edge is what the reference shows), otherwise the lowest-numbered one
/// (`nouveau` has a single, unlabelled input).
fn hwmon_temperature(device: &Path) -> Option<PathBuf> {
    let mut inputs: Vec<(u32, PathBuf)> = Vec::new();
    for hwmon in std::fs::read_dir(device.join("hwmon"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let dir = hwmon.path();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let Some(number) = name
                .to_str()
                .and_then(|name| name.strip_prefix("temp"))
                .and_then(|rest| rest.strip_suffix("_input"))
                .and_then(|number| number.parse().ok())
            else {
                continue;
            };
            if read_text(&dir.join(format!("temp{number}_label"))).as_deref() == Some("edge") {
                return Some(entry.path());
            }
            inputs.push((number, entry.path()));
        }
    }
    inputs
        .into_iter()
        .min_by_key(|(number, _)| *number)
        .map(|(_, path)| path)
}

/// Busy share of an interval from a cumulative idle counter: everything the card did not
/// spend idle. `None` for an empty interval and for a counter that went backwards (a reset).
fn busy_from_idle(prev_idle_ms: u64, idle_ms: u64, elapsed_ms: u64) -> Option<f32> {
    if elapsed_ms == 0 || idle_ms < prev_idle_ms {
        return None;
    }
    let idle = (idle_ms - prev_idle_ms) as f64 / elapsed_ms as f64;
    Some((1.0 - idle).clamp(0.0, 1.0) as f32)
}

/// A small counter file, read into the shared buffer.
fn read_u64(path: &Path, buf: &mut Vec<u8>) -> Option<u64> {
    buf.clear();
    std::fs::File::open(path).ok()?.read_to_end(buf).ok()?;
    std::str::from_utf8(buf).ok()?.trim().parse().ok()
}

/// A whole-percent counter as a share, `0.0..=1.0`.
fn read_percent(path: &Path, buf: &mut Vec<u8>) -> Option<f32> {
    read_u64(path, buf).map(|percent| percent.min(100) as f32 / 100.0)
}

/// A used and a total counter, kept only when the total is not zero.
fn read_pair(used: &Path, total: &Path, buf: &mut Vec<u8>) -> Option<(u64, u64)> {
    match (read_u64(used, buf), read_u64(total, buf)) {
        (Some(used), Some(total)) if total > 0 => Some((used, total)),
        _ => None,
    }
}

/// What one DRM client's `fdinfo` says about it.
#[derive(Debug, PartialEq)]
struct Client {
    /// `drm-pdev`, the PCI address of the card.
    pdev: String,
    /// `drm-client-id`, unique per open of the device; several descriptors may share it.
    id: u64,
    /// System memory held, in bytes, with a buffer shared with another client counted at
    /// half. `None` when the kernel reports no memory regions.
    memory: Option<u64>,
}

/// Reads a DRM client's `fdinfo`. `None` for a descriptor that is not a DRM client.
///
/// Every region of system memory counts - `system0` and `stolen-system0` on `i915`, `system`
/// and `gtt` on `xe` - and only memory on the card (`vram*`, `local*`) is left out. A shared
/// buffer is reported by every client that holds it; it is nearly always two, an application
/// and the compositor, so each counts half of it.
fn parse_fdinfo(text: &str) -> Option<Client> {
    let mut pdev = None;
    let mut id = None;
    let (mut resident, mut shared, mut reported) = (0_u64, 0_u64, false);
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "drm-pdev" => pdev = Some(value.to_owned()),
            "drm-client-id" => id = value.parse().ok(),
            _ => {
                let (into, region) = if let Some(region) = key.strip_prefix("drm-resident-") {
                    (&mut resident, region)
                } else if let Some(region) = key.strip_prefix("drm-shared-") {
                    (&mut shared, region)
                } else {
                    continue;
                };
                if region.starts_with("vram") || region.starts_with("local") {
                    continue;
                }
                if let Some(bytes) = parse_size(value) {
                    *into += bytes;
                    reported = true;
                }
            }
        }
    }
    Some(Client {
        pdev: pdev?,
        id: id?,
        memory: reported.then(|| resident.saturating_sub(shared / 2)),
    })
}

/// `60268 KiB`, `1 MiB`, `4096` (bytes) -> bytes.
fn parse_size(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let number: u64 = parts.next()?.parse().ok()?;
    let scale = match parts.next() {
        None => 1,
        Some("KiB") => 1 << 10,
        Some("MiB") => 1 << 20,
        Some("GiB") => 1 << 30,
        Some(_) => return None,
    };
    Some(number.saturating_mul(scale))
}

/// System memory DRM clients hold, per card address, summed over every process whose
/// descriptors this user may read.
///
/// Only descriptors that point into `/dev/dri/` have their `fdinfo` read; the rest cost one
/// `readlink`. A client is counted once however many descriptors it has open. A card none of
/// whose clients reports memory is left out, so it reads as having no source rather than as
/// holding nothing.
fn drm_memory(proc: &Path, buf: &mut Vec<u8>) -> Vec<(String, u64)> {
    let mut seen: Vec<(String, u64)> = Vec::new();
    let mut held: Vec<(String, u64)> = Vec::new();
    for process in std::fs::read_dir(proc).into_iter().flatten().flatten() {
        let pid = process.file_name();
        if !pid.as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let dir = process.path();
        let Ok(fds) = std::fs::read_dir(dir.join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            if !target.starts_with("/dev/dri") {
                continue;
            }
            let Some(client) = read_client(&dir, &fd.file_name(), buf) else {
                continue;
            };
            let key = (client.pdev, client.id);
            if seen.contains(&key) {
                continue;
            }
            let (pdev, _) = &key;
            if let Some(bytes) = client.memory {
                match held.iter_mut().find(|(at, _)| at == pdev) {
                    Some((_, sum)) => *sum += bytes,
                    None => held.push((pdev.clone(), bytes)),
                }
            }
            seen.push(key);
        }
    }
    held
}

fn read_client(process: &Path, fd: &OsStr, buf: &mut Vec<u8>) -> Option<Client> {
    buf.clear();
    std::fs::File::open(process.join("fdinfo").join(fd))
        .ok()?
        .read_to_end(buf)
        .ok()?;
    parse_fdinfo(std::str::from_utf8(buf).ok()?)
}

fn read_text(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The last component of what a symlink points at: `../../../0000:01:00.0` -> `0000:01:00.0`.
fn link_basename(path: &Path) -> Option<String> {
    let target = std::fs::read_link(path).ok()?;
    Some(target.file_name()?.to_str()?.to_owned())
}

const NVML_SUCCESS: c_int = 0;

type NvmlDevice = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct NvmlUtilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
#[derive(Default)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

/// `nvmlMemory_v2_t`: `used` here leaves out what the driver reserves for itself, which the
/// first version counts as used - a GTX 1660 reads 391 MiB fuller than `nvidia-smi` says.
#[repr(C)]
#[derive(Default)]
struct NvmlMemoryV2 {
    version: c_uint,
    total: u64,
    reserved: u64,
    free: u64,
    used: u64,
}

/// `nvmlMemory_v2`, NVML's `NVML_STRUCT_VERSION(Memory, 2)`: the struct's size with the
/// version in the top byte. A call with any other value is refused.
const NVML_MEMORY_V2: c_uint = std::mem::size_of::<NvmlMemoryV2>() as c_uint | 2 << 24;

/// `NVML_TEMPERATURE_GPU`: the die.
const NVML_TEMPERATURE_GPU: c_uint = 0;

/// NVIDIA's management library, opened at run time: the package does not depend on the
/// driver, and a machine without an NVIDIA card never loads it.
struct Nvml {
    shutdown: unsafe extern "C" fn() -> c_int,
    handle_by_bus_id: unsafe extern "C" fn(*const c_char, *mut NvmlDevice) -> c_int,
    utilization: unsafe extern "C" fn(NvmlDevice, *mut NvmlUtilization) -> c_int,
    memory_info: unsafe extern "C" fn(NvmlDevice, *mut NvmlMemory) -> c_int,
    /// Drivers from 510 on; an older one has only the first version.
    memory_info_v2: Option<unsafe extern "C" fn(NvmlDevice, *mut NvmlMemoryV2) -> c_int>,
    temperature: Option<unsafe extern "C" fn(NvmlDevice, c_uint, *mut c_uint) -> c_int>,
    /// Last, so it is dropped after the functions taken from it.
    _lib: libloading::Library,
}

impl Nvml {
    fn open() -> Option<Self> {
        // SAFETY: loading the library runs its initializers; it is NVIDIA's own management
        // library, shipped with the driver for exactly this use.
        let lib = match unsafe { libloading::Library::new("libnvidia-ml.so.1") } {
            Ok(lib) => lib,
            Err(err) => {
                tracing::info!(?err, "no NVML; NVIDIA cards are shown without a reading");
                return None;
            }
        };
        // SAFETY: the signatures are NVML's, as declared in nvml.h for these versioned names.
        unsafe {
            let init: unsafe extern "C" fn() -> c_int = *lib.get(b"nvmlInit_v2\0").ok()?;
            let shutdown = *lib.get(b"nvmlShutdown\0").ok()?;
            let handle_by_bus_id = *lib.get(b"nvmlDeviceGetHandleByPciBusId_v2\0").ok()?;
            let utilization = *lib.get(b"nvmlDeviceGetUtilizationRates\0").ok()?;
            let memory_info = *lib.get(b"nvmlDeviceGetMemoryInfo\0").ok()?;
            let memory_info_v2 = lib
                .get(b"nvmlDeviceGetMemoryInfo_v2\0")
                .ok()
                .map(|symbol| *symbol);
            let temperature = lib
                .get(b"nvmlDeviceGetTemperature\0")
                .ok()
                .map(|symbol| *symbol);
            let rc = init();
            if rc != NVML_SUCCESS {
                tracing::warn!(
                    rc,
                    "nvmlInit_v2 failed; NVIDIA cards are shown without a reading"
                );
                return None;
            }
            Some(Self {
                shutdown,
                handle_by_bus_id,
                utilization,
                memory_info,
                memory_info_v2,
                temperature,
                _lib: lib,
            })
        }
    }

    fn handle(&self, address: &str) -> Option<usize> {
        let id = CString::new(address).ok()?;
        let mut device: NvmlDevice = std::ptr::null_mut();
        // SAFETY: a valid C string and a place for the handle, as the call requires.
        let rc = unsafe { (self.handle_by_bus_id)(id.as_ptr(), &mut device) };
        (rc == NVML_SUCCESS && !device.is_null()).then_some(device as usize)
    }

    /// How busy the card and its memory controller were, as shares, from one call.
    fn utilization(&self, device: usize) -> Option<(f32, f32)> {
        let mut rates = NvmlUtilization::default();
        // SAFETY: a handle NVML gave out, and a struct of the layout it fills.
        let rc = unsafe { (self.utilization)(device as NvmlDevice, &mut rates) };
        let share = |percent: c_uint| percent.min(100) as f32 / 100.0;
        (rc == NVML_SUCCESS).then(|| (share(rates.gpu), share(rates.memory)))
    }

    /// Video memory in use, without the driver's reserve where the driver can say, and in all.
    fn memory(&self, device: usize) -> Option<(u64, u64)> {
        if let Some(memory_info_v2) = self.memory_info_v2 {
            let mut memory = NvmlMemoryV2 {
                version: NVML_MEMORY_V2,
                ..NvmlMemoryV2::default()
            };
            // SAFETY: as for `utilization`, with the version field the call checks.
            let rc = unsafe { memory_info_v2(device as NvmlDevice, &mut memory) };
            if rc == NVML_SUCCESS && memory.total > 0 {
                return Some((memory.used, memory.total));
            }
        }
        let mut memory = NvmlMemory::default();
        // SAFETY: as for `utilization`.
        let rc = unsafe { (self.memory_info)(device as NvmlDevice, &mut memory) };
        (rc == NVML_SUCCESS && memory.total > 0).then_some((memory.used, memory.total))
    }

    /// The die temperature in degrees Celsius.
    fn temperature(&self, device: usize) -> Option<f32> {
        let call = self.temperature?;
        let mut celsius: c_uint = 0;
        // SAFETY: a handle NVML gave out, a sensor it defines, and a place for the reading.
        let rc = unsafe { call(device as NvmlDevice, NVML_TEMPERATURE_GPU, &mut celsius) };
        (rc == NVML_SUCCESS).then_some(celsius as f32)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: paired with the successful `nvmlInit_v2` in `open`.
        unsafe {
            (self.shutdown)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    /// A stand-in for `/sys`: `class/drm` with cards whose `device` links point into
    /// `devices`, as the real tree does. Removed when dropped.
    struct Sysfs(PathBuf);

    impl Sysfs {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("wmde-taskmgr-gpu-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("class/drm")).unwrap();
            Self(dir)
        }

        fn drm(&self) -> PathBuf {
            self.0.join("class/drm")
        }

        /// The stand-in for `/proc`, next to the one for `/sys`.
        fn proc(&self) -> PathBuf {
            self.0.join("proc")
        }

        /// Descriptor `fd` of process `pid`, pointing at `target`, with `fdinfo` beside it.
        /// The link dangles, as nothing here opens it: only its target is read.
        fn descriptor(&self, pid: u32, fd: u32, target: &str, fdinfo: &str) {
            let dir = self.proc().join(pid.to_string());
            fs::create_dir_all(dir.join("fd")).unwrap();
            fs::create_dir_all(dir.join("fdinfo")).unwrap();
            symlink(target, dir.join("fd").join(fd.to_string())).unwrap();
            fs::write(dir.join("fdinfo").join(fd.to_string()), fdinfo).unwrap();
        }

        /// `class/drm/<node>` with a `device` link to `devices/<address>`, which has the
        /// class and a `driver` link.
        fn card(&self, node: &str, address: &str, class: &str, driver: &str) -> PathBuf {
            let device = self.0.join("devices").join(address);
            fs::create_dir_all(&device).unwrap();
            fs::write(device.join("class"), format!("{class}\n")).unwrap();
            let drivers = self.0.join("drivers").join(driver);
            fs::create_dir_all(&drivers).unwrap();
            let _ = symlink(&drivers, device.join("driver"));
            let card = self.drm().join(node);
            fs::create_dir_all(&card).unwrap();
            symlink(&device, card.join("device")).unwrap();
            device
        }
    }

    impl Drop for Sysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn only_display_cards_are_listed() {
        let sys = Sysfs::new("display");
        sys.card("card0", "0000:00:02.0", "0x030000", "vmwgfx");
        sys.card("card1", "0000:00:03.0", "0x020000", "e1000");
        fs::create_dir_all(sys.drm().join("card0-Virtual-1")).unwrap();
        fs::create_dir_all(sys.drm().join("renderD128")).unwrap();

        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.sample(Instant::now());
        let cards = gpus.cards();
        assert_eq!(cards.len(), 1);
        assert_eq!(&*cards[0].node, "card0");
        assert_eq!(&*cards[0].driver, "vmwgfx");
        assert_eq!(&*cards[0].address, "0000:00:02.0");
        assert_eq!(cards[0].busy, None);
        assert_eq!(cards[0].memory, None);
    }

    /// virtio-gpu hangs its node off the virtio bus; the card is the PCI function above it,
    /// and the driver is the one on the virtio device.
    #[test]
    fn a_virtio_card_is_its_pci_function() {
        let sys = Sysfs::new("virtio");
        let function = sys.0.join("devices/0000:00:02.0");
        let virtio = function.join("virtio0");
        fs::create_dir_all(&virtio).unwrap();
        fs::write(function.join("class"), "0x030000\n").unwrap();
        let drivers = sys.0.join("drivers/virtio_gpu");
        fs::create_dir_all(&drivers).unwrap();
        symlink(&drivers, virtio.join("driver")).unwrap();
        let card = sys.drm().join("card0");
        fs::create_dir_all(&card).unwrap();
        symlink(&virtio, card.join("device")).unwrap();

        let gpus = Gpus::scan(&sys.drm(), || None);
        let cards = gpus.cards();
        assert_eq!(cards.len(), 1);
        assert_eq!(&*cards[0].address, "0000:00:02.0");
        assert_eq!(&*cards[0].driver, "virtio_gpu");
        assert_eq!(cards[0].busy, None);
    }

    /// A platform framebuffer has no PCI class on itself or above it, and is not a card.
    #[test]
    fn a_platform_framebuffer_is_not_a_card() {
        let sys = Sysfs::new("simpledrm");
        let platform = sys.0.join("devices/platform/simple-framebuffer.0");
        fs::create_dir_all(&platform).unwrap();
        let card = sys.drm().join("card0");
        fs::create_dir_all(&card).unwrap();
        symlink(&platform, card.join("device")).unwrap();

        assert!(Gpus::scan(&sys.drm(), || None).cards().is_empty());
    }

    #[test]
    fn cards_come_in_node_order() {
        let sys = Sysfs::new("order");
        sys.card("card10", "0000:0a:00.0", "0x030000", "amdgpu");
        sys.card("card2", "0000:02:00.0", "0x030000", "amdgpu");
        let gpus = Gpus::scan(&sys.drm(), || None);
        let nodes: Vec<&str> = gpus.cards().iter().map(|card| &*card.node).collect();
        assert_eq!(nodes, ["card2", "card10"]);
    }

    #[test]
    fn amdgpu_reads_busy_share_and_video_memory() {
        let sys = Sysfs::new("amdgpu");
        let device = sys.card("card0", "0000:03:00.0", "0x030000", "amdgpu");
        fs::write(device.join("gpu_busy_percent"), "37\n").unwrap();
        fs::write(device.join("mem_info_vram_used"), "1073741824\n").unwrap();
        fs::write(device.join("mem_info_vram_total"), "8589934592\n").unwrap();

        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.sample(Instant::now());
        let card = &gpus.cards()[0];
        assert_eq!(card.busy, Some(0.37));
        assert_eq!(card.memory, Some((1 << 30, 8 << 30)));
    }

    #[test]
    fn intel_busy_is_what_was_not_idle() {
        let sys = Sysfs::new("i915");
        sys.card("card0", "0000:00:02.0", "0x030000", "i915");
        let gt = sys.drm().join("card0/gt/gt0");
        fs::create_dir_all(&gt).unwrap();
        fs::write(gt.join("rc6_residency_ms"), "1000\n").unwrap();

        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.proc = sys.proc();
        let start = Instant::now();
        gpus.sample(start);
        assert_eq!(gpus.cards()[0].busy, None, "one reading is not an interval");

        fs::write(gt.join("rc6_residency_ms"), "1250\n").unwrap();
        gpus.sample(start + Duration::from_secs(1));
        assert_eq!(gpus.cards()[0].busy, Some(0.75));
        assert_eq!(gpus.cards()[0].memory, None);
    }

    #[test]
    fn idle_counter_arithmetic() {
        assert_eq!(busy_from_idle(1000, 1250, 1000), Some(0.75));
        assert_eq!(busy_from_idle(0, 0, 1000), Some(1.0));
        assert_eq!(
            busy_from_idle(0, 1500, 1000),
            Some(0.0),
            "clamped, not negative"
        );
        assert_eq!(busy_from_idle(1000, 900, 1000), None, "a reset counter");
        assert_eq!(busy_from_idle(1000, 1000, 0), None, "an empty interval");
    }

    #[test]
    fn nvidia_without_nvml_is_listed_without_a_reading() {
        let sys = Sysfs::new("nvidia");
        sys.card("card1", "0000:01:00.0", "0x030000", "nvidia");
        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.sample(Instant::now());
        assert_eq!(gpus.cards().len(), 1);
        assert_eq!(&*gpus.cards()[0].driver, "nvidia");
        assert_eq!(gpus.cards()[0].busy, None);
    }

    #[test]
    fn amdgpu_reads_memory_controller_and_gtt() {
        let sys = Sysfs::new("amdgpu-gtt");
        let device = sys.card("card0", "0000:03:00.0", "0x030000", "amdgpu");
        fs::write(device.join("gpu_busy_percent"), "5\n").unwrap();
        fs::write(device.join("mem_busy_percent"), "12\n").unwrap();
        fs::write(device.join("mem_info_gtt_used"), "268435456\n").unwrap();
        fs::write(device.join("mem_info_gtt_total"), "8589934592\n").unwrap();

        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.sample(Instant::now());
        let card = &gpus.cards()[0];
        assert_eq!(card.memory_busy, Some(0.12));
        assert_eq!(card.shared, Some((256 << 20, Some(8 << 30))));
        // No VRAM counters in the fixture: the card reads as having none, not as empty.
        assert_eq!(card.memory, None);
    }

    #[test]
    fn fdinfo_sizes_come_in_every_unit() {
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("60268 KiB"), Some(60268 << 10));
        assert_eq!(parse_size("1 MiB"), Some(1 << 20));
        assert_eq!(parse_size("2 GiB"), Some(2 << 30));
        assert_eq!(parse_size("1 TiB"), None);
        assert_eq!(parse_size("many"), None);
    }

    /// System regions count, memory on the card does not, and a shared buffer counts half.
    #[test]
    fn a_client_holds_its_system_memory() {
        let client = parse_fdinfo(
            "pos:\t0\nflags:\t02100002\ndrm-driver:\ti915\ndrm-client-id:\t10\n\
             drm-pdev:\t0000:00:02.0\ndrm-total-system0:\t4 MiB\n\
             drm-resident-system0:\t4 MiB\ndrm-shared-system0:\t2 MiB\n\
             drm-resident-stolen-system0:\t1 MiB\ndrm-resident-vram0:\t1 GiB\n",
        )
        .unwrap();
        assert_eq!(client.pdev, "0000:00:02.0");
        assert_eq!(client.id, 10);
        assert_eq!(client.memory, Some((4 + 1 - 1) << 20));
    }

    #[test]
    fn a_client_without_regions_reports_no_memory() {
        let client =
            parse_fdinfo("drm-driver:\ti915\ndrm-client-id:\t3\ndrm-pdev:\t0000:00:02.0\n")
                .unwrap();
        assert_eq!(client.memory, None);
        // Not a DRM client at all.
        assert_eq!(parse_fdinfo("pos:\t0\nflags:\t02\nmnt_id:\t25\n"), None);
    }

    #[test]
    fn intel_shared_memory_is_the_sum_of_its_clients() {
        let sys = Sysfs::new("fdinfo");
        sys.card("card1", "0000:00:02.0", "0x030000", "i915");
        let ours = |id: u32, mib: u32| {
            format!(
                "drm-client-id:\t{id}\ndrm-pdev:\t0000:00:02.0\n\
                 drm-resident-system0:\t{mib} MiB\ndrm-shared-system0:\t0\n"
            )
        };
        // Two descriptors of one client: counted once.
        sys.descriptor(100, 3, "/dev/dri/card1", &ours(10, 60));
        sys.descriptor(100, 4, "/dev/dri/renderD128", &ours(10, 60));
        sys.descriptor(200, 5, "/dev/dri/renderD128", &ours(11, 4));
        // Another card's client.
        sys.descriptor(
            200,
            6,
            "/dev/dri/renderD129",
            "drm-client-id:\t12\ndrm-pdev:\t0000:01:00.0\ndrm-resident-system0:\t1 GiB\n",
        );
        // Not a DRM descriptor: its fdinfo is never read, whatever it says.
        sys.descriptor(200, 7, "/home/user/file", &ours(13, 1000));

        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.proc = sys.proc();
        gpus.sample(Instant::now());
        assert_eq!(gpus.cards()[0].shared, Some(((60 + 4) << 20, None)));
    }

    #[test]
    fn only_an_intel_card_has_its_clients_counted() {
        let sys = Sysfs::new("fdinfo-other");
        sys.card("card0", "0000:00:02.0", "0x030000", "vmwgfx");
        sys.descriptor(
            100,
            3,
            "/dev/dri/card0",
            "drm-client-id:\t1\ndrm-pdev:\t0000:00:02.0\ndrm-resident-system0:\t8 MiB\n",
        );
        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.proc = sys.proc();
        gpus.sample(Instant::now());
        assert_eq!(gpus.cards()[0].shared, None);
    }

    #[test]
    fn an_intel_card_without_reporting_clients_has_no_shared_memory() {
        let sys = Sysfs::new("fdinfo-old");
        sys.card("card0", "0000:00:02.0", "0x030000", "i915");
        sys.descriptor(
            100,
            3,
            "/dev/dri/card0",
            "drm-client-id:\t1\ndrm-pdev:\t0000:00:02.0\n",
        );
        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.proc = sys.proc();
        gpus.sample(Instant::now());
        assert_eq!(gpus.cards()[0].shared, None);
    }

    /// NVML checks the version field against its own idea of the struct: size in the low
    /// bytes, version in the top one.
    #[test]
    fn the_v2_memory_struct_is_the_one_nvml_expects() {
        assert_eq!(std::mem::size_of::<NvmlMemoryV2>(), 40);
        assert_eq!(NVML_MEMORY_V2, 0x0200_0028);
    }

    #[test]
    fn amdgpu_temperature_is_the_edge_sensor() {
        let sys = Sysfs::new("hwmon-amd");
        let device = sys.card("card0", "0000:03:00.0", "0x030000", "amdgpu");
        let hwmon = device.join("hwmon/hwmon3");
        fs::create_dir_all(&hwmon).unwrap();
        for (n, label, milli) in [
            (1, "edge", "45000"),
            (2, "junction", "52000"),
            (3, "mem", "60000"),
        ] {
            fs::write(hwmon.join(format!("temp{n}_input")), format!("{milli}\n")).unwrap();
            fs::write(hwmon.join(format!("temp{n}_label")), format!("{label}\n")).unwrap();
        }
        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.sample(Instant::now());
        assert_eq!(gpus.cards()[0].temperature, Some(45.0));
    }

    #[test]
    fn an_unlabelled_sensor_is_taken_by_its_number() {
        let sys = Sysfs::new("hwmon-nouveau");
        let device = sys.card("card0", "0000:01:00.0", "0x030000", "nouveau");
        let hwmon = device.join("hwmon/hwmon2");
        fs::create_dir_all(&hwmon).unwrap();
        fs::write(hwmon.join("temp1_input"), "38500\n").unwrap();
        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.sample(Instant::now());
        let card = &gpus.cards()[0];
        assert_eq!(card.temperature, Some(38.5));
        // A temperature, and still no utilization: the two are separate sources.
        assert_eq!(card.busy, None);
    }

    #[test]
    fn a_card_without_hwmon_has_no_temperature() {
        let sys = Sysfs::new("hwmon-none");
        sys.card("card0", "0000:00:02.0", "0x030000", "vmwgfx");
        let mut gpus = Gpus::scan(&sys.drm(), || None);
        gpus.sample(Instant::now());
        assert_eq!(gpus.cards()[0].temperature, None);
    }

    #[test]
    fn nvml_is_not_opened_without_an_nvidia_card() {
        let sys = Sysfs::new("no-nvidia");
        sys.card("card0", "0000:00:02.0", "0x030000", "vmwgfx");
        let gpus = Gpus::scan(&sys.drm(), || {
            panic!("NVML opened for a machine without NVIDIA")
        });
        assert_eq!(gpus.cards().len(), 1);
    }
}
