// SPDX-License-Identifier: GPL-3.0-only

//! What the disks and the network cards **are**, beside what they are doing.
//!
//! Same division as [`crate::facts`]: the probe hands over names and rates every tick, and
//! everything here is read once per device and kept. A drive does not change its model while
//! the window is open.
//!
//! Every source is world-readable. A model, a size or a link speed the machine does not
//! publish comes back empty, and the row that would have carried it is not drawn - the same
//! rule the processor facts follow.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

const BLOCK_DIR: &str = "/sys/block";
const NET_DIR: &str = "/sys/class/net";
/// Where `hwdata` leaves the PCI name table. A network card publishes its vendor and device
/// as two hex numbers and nothing else; this file is what turns them into the name the
/// reference prints beside the graph. Missing on a machine without the package, and then the
/// driver name stands in.
const PCI_IDS: &str = "/usr/share/hwdata/pci.ids";
/// `/sys/block/<dev>/size` is in 512-byte units whatever the drive's real sector size is.
const SECTOR_BYTES: u64 = 512;

/// What kind of drive it is. The wording lives in the localization, not here.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiskKind {
    Nvme,
    Ssd,
    Hdd,
    /// A stick or a card reader: `removable` is set and the rest does not matter.
    Removable,
    #[default]
    Unknown,
}

/// What kind of link it is.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LinkKind {
    #[default]
    Ethernet,
    Wireless,
}

/// One drive, as `/sys/block/<name>` describes it.
#[derive(Clone, Debug, Default)]
pub struct Disk {
    pub kind: DiskKind,
    /// `KINGSTON SNV2S500G`. Empty when the driver publishes none.
    pub model: String,
    /// Bytes, or zero when the node does not say.
    pub size: u64,
}

/// One network card, as `/sys/class/net/<name>` describes it.
#[derive(Clone, Debug, Default)]
pub struct Interface {
    pub kind: LinkKind,
    /// `Intel 82540EM Gigabit Ethernet Controller`, or the driver name, or empty.
    pub model: String,
    /// Megabits per second the link negotiated. `None` while the link is down, and for
    /// anything that has no meaningful speed to report.
    pub speed_mbps: Option<u32>,
}

/// Everything read about the devices seen so far, keyed by the kernel's name for them.
///
/// A drive plugged in later is read then and not before: [`Catalog::refresh`] is handed the
/// names the probe found this tick and reads only what it has never seen. The entries of
/// devices that went away are kept - they cost a string each, and a stick pulled out and put
/// back is the common case.
#[derive(Debug, Default)]
pub struct Catalog {
    disks: HashMap<Box<str>, Disk>,
    nets: HashMap<Box<str>, Interface>,
}

impl Catalog {
    /// Reads whatever in these two lists is new. Called every tick; costs nothing on the
    /// ticks where nothing was plugged in or out.
    pub fn refresh<'a>(
        &mut self,
        disks: impl Iterator<Item = &'a str>,
        nets: impl Iterator<Item = &'a str>,
    ) {
        for name in disks {
            if !self.disks.contains_key(name) {
                self.disks.insert(Box::from(name), read_disk(name));
            }
        }
        // The card names are collected first: naming them means walking a megabyte and a
        // half of pci.ids, and one walk answers for all of them.
        let unknown: Vec<&str> = nets.filter(|name| !self.nets.contains_key(*name)).collect();
        if unknown.is_empty() {
            return;
        }
        let models = pci_models(&unknown);
        for (name, model) in unknown.into_iter().zip(models) {
            self.nets
                .insert(Box::from(name), read_interface(name, model));
        }
    }

    #[must_use]
    pub fn disk(&self, name: &str) -> Option<&Disk> {
        self.disks.get(name)
    }

    #[must_use]
    pub fn interface(&self, name: &str) -> Option<&Interface> {
        self.nets.get(name)
    }
}

fn read_disk(name: &str) -> Disk {
    let path = Path::new(BLOCK_DIR).join(name);
    Disk {
        kind: disk_kind(
            name,
            read_bool(&path.join("removable")),
            read_bool(&path.join("queue/rotational")),
        ),
        // The model is right-padded with spaces to a fixed width by both the NVMe and the
        // SCSI drivers, so the trim is required and not cosmetic.
        model: read_trimmed(&path.join("device/model")).unwrap_or_default(),
        size: read_u64(&path.join("size")).unwrap_or(0) * SECTOR_BYTES,
    }
}

/// An NVMe namespace reports `rotational` as 0 like any SSD, and would otherwise be flattened
/// into one. Removable is asked before the platter question because a stick answers it with
/// whatever its bridge chip felt like.
fn disk_kind(name: &str, removable: Option<bool>, rotational: Option<bool>) -> DiskKind {
    if name.starts_with("nvme") {
        DiskKind::Nvme
    } else if removable == Some(true) {
        DiskKind::Removable
    } else {
        match rotational {
            Some(true) => DiskKind::Hdd,
            Some(false) => DiskKind::Ssd,
            None => DiskKind::Unknown,
        }
    }
}

fn read_interface(name: &str, model: Option<String>) -> Interface {
    let path = Path::new(NET_DIR).join(name);
    Interface {
        // A wireless card in managed mode reports ARPHRD_ETHER like any other, so `type` is
        // no help and the wireless directory is the test.
        kind: if path.join("wireless").is_dir() || path.join("phy80211").exists() {
            LinkKind::Wireless
        } else {
            LinkKind::Ethernet
        },
        model: model
            .or_else(|| link_basename(&path.join("device/driver")))
            .unwrap_or_default(),
        // `speed` answers with an error while the link is down and with -1 where the number
        // would be meaningless; both come back as no speed at all.
        speed_mbps: read_trimmed(&path.join("speed"))
            .and_then(|text| text.parse::<i64>().ok())
            .and_then(|mbps| u32::try_from(mbps).ok()),
    }
}

/// Names for these interfaces out of the PCI table, in the order they were given.
///
/// One pass over the file for however many cards are asked about: it is a megabyte and a
/// half, and a machine can hold several cards. `None` for an interface whose ids are missing
/// - a USB adapter publishes `idVendor` instead - or absent from the table.
fn pci_models(names: &[&str]) -> Vec<Option<String>> {
    let wanted: Vec<Option<(String, String)>> = names.iter().map(|name| pci_ids(name)).collect();
    if wanted.iter().all(Option::is_none) {
        return vec![None; names.len()];
    }
    match std::fs::read_to_string(PCI_IDS) {
        Ok(table) => lookup(&table, &wanted),
        Err(err) => {
            tracing::info!(
                path = PCI_IDS,
                ?err,
                "no pci name table; using driver names"
            );
            vec![None; names.len()]
        }
    }
}

/// The walk itself, over a table handed in. Split from the reading so that the shape of the
/// file - vendor at the margin, device one tab in, subsystem two - is under test; whether a
/// machine has the file is not something a test can arrange.
fn lookup(table: &str, wanted: &[Option<(String, String)>]) -> Vec<Option<String>> {
    let mut out = vec![None; wanted.len()];
    let mut vendor_id = "";
    let mut vendor_name = "";
    for line in table.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        // A vendor sits at the left margin, its devices one tab in, and a subsystem two.
        // The class section at the end of the file starts with `C ` and matches neither.
        let depth = line.len() - line.trim_start_matches('\t').len();
        let Some((id, name)) = line.trim_start().split_once("  ") else {
            continue;
        };
        match depth {
            0 => {
                vendor_id = id;
                vendor_name = name;
            }
            1 => {
                for (slot, pair) in out.iter_mut().zip(wanted) {
                    let Some((want_vendor, want_device)) = pair else {
                        continue;
                    };
                    if want_vendor == vendor_id && want_device == id {
                        *slot = Some(format!("{} {name}", short_vendor(vendor_name)));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The `0x8086` and `0x100e` of a card, as pci.ids spells them: four lowercase hex digits.
fn pci_ids(name: &str) -> Option<(String, String)> {
    let path = Path::new(NET_DIR).join(name).join("device");
    let id = |file: &str| {
        read_trimmed(&path.join(file))
            .and_then(|text| u16::from_str_radix(text.trim_start_matches("0x"), 16).ok())
            .map(|id| format!("{id:04x}"))
    };
    Some((id("vendor")?, id("device")?))
}

/// `Intel Corporation` -> `Intel`. The legal suffix is noise in a row eighty pixels wide, and
/// every vendor in the table carries one.
fn short_vendor(name: &str) -> &str {
    const TAILS: [&str; 8] = [
        " Corporation",
        " Corp.",
        ", Inc.",
        " Inc.",
        " Technologies",
        " Technology",
        " Co., Ltd.",
        " Semiconductor",
    ];
    for tail in TAILS {
        if let Some(head) = name.strip_suffix(tail) {
            return head;
        }
    }
    name
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn read_u64(path: &Path) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

fn read_bool(path: &Path) -> Option<bool> {
    match read_trimmed(path)?.as_str() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// The last component of what a symlink points at: `.../drivers/e1000` -> `e1000`.
fn link_basename(path: &Path) -> Option<String> {
    let target: PathBuf = std::fs::read_link(path).ok()?;
    Some(target.file_name()?.to_str()?.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three questions a drive answers, and the order they have to be asked in.
    #[test]
    fn a_drive_is_named_by_what_it_is() {
        assert_eq!(
            disk_kind("nvme0n1", Some(false), Some(false)),
            DiskKind::Nvme
        );
        // A namespace that says it spins is still NVMe: the flag is not the question.
        assert_eq!(
            disk_kind("nvme0n1", Some(false), Some(true)),
            DiskKind::Nvme
        );
        assert_eq!(disk_kind("sda", Some(false), Some(true)), DiskKind::Hdd);
        assert_eq!(disk_kind("sda", Some(false), Some(false)), DiskKind::Ssd);
        // A stick is removable first and whatever its bridge chip claims second.
        assert_eq!(
            disk_kind("sdb", Some(true), Some(true)),
            DiskKind::Removable
        );
        assert_eq!(disk_kind("sdc", None, None), DiskKind::Unknown);
    }

    #[test]
    fn a_vendor_loses_its_legal_suffix_and_nothing_else() {
        assert_eq!(short_vendor("Intel Corporation"), "Intel");
        assert_eq!(short_vendor("Realtek Semiconductor"), "Realtek");
        assert_eq!(short_vendor("Broadcom Inc."), "Broadcom");
        assert_eq!(short_vendor("Qualcomm Atheros"), "Qualcomm Atheros");
    }

    /// The whole point of the file: `8086:100e` is the card the virtual machine has.
    #[test]
    fn the_table_is_read_by_indentation() {
        // The shape of pci.ids: vendor at the margin, device one tab in, subsystem two.
        let table = "\
# comment
8086  Intel Corporation
\t100e  82540EM Gigabit Ethernet Controller
\t\t8086 001e  PRO/1000 MT Desktop Adapter
\t1533  I210 Gigabit Network Connection
10ec  Realtek Semiconductor
\t8168  RTL8111/8168/8411 PCI Express Gigabit Ethernet Controller
C 03  Display controller
";
        let wanted: Vec<Option<(String, String)>> =
            [("8086", "100e"), ("10ec", "8168"), ("8086", "ffff")]
                .iter()
                .map(|(vendor, device)| Some(((*vendor).to_owned(), (*device).to_owned())))
                .collect();
        let found = lookup(table, &wanted);
        assert_eq!(
            found[0].as_deref(),
            Some("Intel 82540EM Gigabit Ethernet Controller")
        );
        assert_eq!(
            found[1].as_deref(),
            Some("Realtek RTL8111/8168/8411 PCI Express Gigabit Ethernet Controller")
        );
        assert_eq!(found[2], None, "a device the table does not list");
    }
}
