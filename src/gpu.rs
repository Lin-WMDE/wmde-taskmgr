// SPDX-License-Identifier: GPL-3.0-only

//! The graphics cards: which there are, and how busy each one is.
//!
//! Cards are found once, at start-up, under `/sys/class/drm`: a card does not come and go
//! while the window is open. How busy a card is has no common source - each driver publishes
//! its own, or none:
//!
//! | Driver | Utilization | Video memory |
//! |---|---|---|
//! | `nvidia` | NVML, `libnvidia-ml.so.1` opened at run time | NVML |
//! | `amdgpu` | `device/gpu_busy_percent` | `device/mem_info_vram_used`, `mem_info_vram_total` |
//! | `i915` | idle residency, `gt/gt0/rc6_residency_ms` | none: the memory is the machine's |
//! | `xe` | idle residency, `device/tile0/gt0/gtidle/idle_residency_ms` | none |
//!
//! Any other card - a virtual one, nouveau - is listed with its name and no reading. Nothing
//! here needs root or starts a process: `nvidia-smi` would cost a process every tick.

use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

const DRM_DIR: &str = "/sys/class/drm";

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
    source: Source,
}

enum Source {
    None,
    /// An `nvmlDevice_t`, kept as an address so the card stays `Send`.
    Nvml(usize),
    Amdgpu {
        busy: PathBuf,
        used: PathBuf,
        total: PathBuf,
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
            cards.push(Card {
                node: name.into(),
                driver: driver.into(),
                address: address.into(),
                device,
                busy: None,
                memory: None,
                source,
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
        }
    }

    /// Reads every card's counters. `now` is taken by the caller, so a test can drive the
    /// idle-residency arithmetic of an Intel card with a clock of its own.
    pub fn sample(&mut self, now: Instant) {
        let Self { cards, nvml, buf } = self;
        for card in cards.iter_mut() {
            match &mut card.source {
                Source::None => {}
                Source::Nvml(handle) => {
                    if let Some(nvml) = nvml {
                        card.busy = nvml.utilization(*handle);
                        card.memory = nvml.memory(*handle);
                    }
                }
                Source::Amdgpu { busy, used, total } => {
                    card.busy = read_u64(busy, buf).map(|percent| percent.min(100) as f32 / 100.0);
                    card.memory = match (read_u64(used, buf), read_u64(total, buf)) {
                        (Some(used), Some(total)) if total > 0 => Some((used, total)),
                        _ => None,
                    };
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
        }
    }
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

/// NVIDIA's management library, opened at run time: the package does not depend on the
/// driver, and a machine without an NVIDIA card never loads it.
struct Nvml {
    shutdown: unsafe extern "C" fn() -> c_int,
    handle_by_bus_id: unsafe extern "C" fn(*const c_char, *mut NvmlDevice) -> c_int,
    utilization: unsafe extern "C" fn(NvmlDevice, *mut NvmlUtilization) -> c_int,
    memory_info: unsafe extern "C" fn(NvmlDevice, *mut NvmlMemory) -> c_int,
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

    fn utilization(&self, device: usize) -> Option<f32> {
        let mut rates = NvmlUtilization::default();
        // SAFETY: a handle NVML gave out, and a struct of the layout it fills.
        let rc = unsafe { (self.utilization)(device as NvmlDevice, &mut rates) };
        (rc == NVML_SUCCESS).then(|| rates.gpu.min(100) as f32 / 100.0)
    }

    fn memory(&self, device: usize) -> Option<(u64, u64)> {
        let mut memory = NvmlMemory::default();
        // SAFETY: as for `utilization`.
        let rc = unsafe { (self.memory_info)(device as NvmlDevice, &mut memory) };
        (rc == NVML_SUCCESS && memory.total > 0).then_some((memory.used, memory.total))
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
    fn nvml_is_not_opened_without_an_nvidia_card() {
        let sys = Sysfs::new("no-nvidia");
        sys.card("card0", "0000:00:02.0", "0x030000", "vmwgfx");
        let gpus = Gpus::scan(&sys.drm(), || {
            panic!("NVML opened for a machine without NVIDIA")
        });
        assert_eq!(gpus.cards().len(), 1);
    }
}
