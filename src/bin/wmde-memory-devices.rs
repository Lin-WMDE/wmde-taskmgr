// SPDX-License-Identifier: GPL-3.0-only

//! Reads the memory modules out of SMBIOS and writes them where a desktop application can
//! see them.
//!
//! `/sys/firmware/dmi/entries/17-*/raw` is `0400 root`, and it is the only place that knows
//! the size, speed, type and part number of a module. This runs once at boot from a oneshot
//! unit, parses the records and leaves a world-readable report; the task manager only reads
//! that. Nothing here stays resident and nothing here opens a socket.
//!
//! Serial numbers and asset tags are read past but never written out. The report is
//! world-readable, and the load page has no use for the identity of a particular stick.
//!
//! Its own binary rather than a flag on the window: what a unit runs as root should not be a
//! file linked against a toolkit, a renderer and a Wayland client.

#[path = "../memory.rs"]
mod memory;

use memory::{Module, Report};

const ENTRIES: &str = "/sys/firmware/dmi/entries";

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let out = args.next();

    let modules = collect(std::path::Path::new(ENTRIES));
    if modules.is_empty() {
        // Every virtual machine lands here, and so does firmware that describes no memory
        // devices. Not an error: the page simply leaves the block out.
        eprintln!("no SMBIOS memory devices in {ENTRIES}, nothing to report");
        return std::process::ExitCode::SUCCESS;
    }

    let report = Report { modules };
    let text = match ron::ser::to_string_pretty(&report, ron::ser::PrettyConfig::default()) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("cannot serialise the report: {err}");
            return std::process::ExitCode::FAILURE;
        }
    };

    match out {
        None => {
            println!("{text}");
            std::process::ExitCode::SUCCESS
        }
        Some(path) => match write_readable(std::path::Path::new(&path), &text) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("cannot write {path}: {err}");
                std::process::ExitCode::FAILURE
            }
        },
    }
}

/// Writes through a temporary file and renames it into place, `0444`.
///
/// The rename is what makes a reader see either the whole report or none of it; a reader
/// that opened the file halfway through a write would parse a truncated one.
fn write_readable(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = path.with_extension("ron.new");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o444))?;
    std::fs::rename(&tmp, path)
}

/// Every `17-*` entry under `dir`, in the order the firmware numbered them.
fn collect(dir: &std::path::Path) -> Vec<Module> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut found: Vec<(u32, Vec<u8>)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // `17-0`, `17-1`, ... - type 17 is Memory Device. The number after the dash is the
        // instance, and it is what orders the slots.
        let Some(instance) = name.strip_prefix("17-") else {
            continue;
        };
        let Ok(instance) = instance.parse::<u32>() else {
            continue;
        };
        if let Ok(raw) = std::fs::read(entry.path().join("raw")) {
            found.push((instance, raw));
        }
    }
    found.sort_by_key(|(instance, _)| *instance);
    found.iter().filter_map(|(_, raw)| parse(raw)).collect()
}

/// One SMBIOS Type 17 record.
///
/// The formatted area is followed by the string table, and its length is in byte 1. Fields
/// past that length belong to a later version of the specification than the firmware wrote,
/// and reading them would be reading the strings: every access goes through the helpers
/// below, which answer `None` once past the end.
fn parse(raw: &[u8]) -> Option<Module> {
    const MEMORY_DEVICE: u8 = 17;

    let length = *raw.get(1)? as usize;
    if *raw.first()? != MEMORY_DEVICE || length < 0x15 || raw.len() < length {
        return None;
    }
    let strings = strings(&raw[length..]);
    let text = |offset: usize| -> String {
        byte(raw, length, offset)
            .and_then(|index| strings.get(index.checked_sub(1)? as usize))
            .cloned()
            .unwrap_or_default()
    };

    Some(Module {
        locator: text(0x10),
        bank: text(0x11),
        size: size(raw, length)?,
        kind: memory_kind(byte(raw, length, 0x12).unwrap_or(0)),
        form_factor: form_factor(byte(raw, length, 0x0E).unwrap_or(0)),
        speed_max: speed(word(raw, length, 0x15)),
        speed_now: speed(word(raw, length, 0x20)),
        vendor: text(0x17),
        part: text(0x1A),
        // Bits 3:0, and zero there means the firmware does not know.
        ranks: byte(raw, length, 0x1B).map(|a| a & 0x0F).filter(|r| *r != 0),
        volts: word(raw, length, 0x26).filter(|v| *v != 0).map(u32::from),
    })
}

fn byte(raw: &[u8], length: usize, offset: usize) -> Option<u8> {
    (offset < length).then(|| raw.get(offset).copied())?
}

fn word(raw: &[u8], length: usize, offset: usize) -> Option<u16> {
    if offset + 1 >= length {
        return None;
    }
    Some(u16::from_le_bytes([
        *raw.get(offset)?,
        *raw.get(offset + 1)?,
    ]))
}

/// Size in bytes, or `None` when the record is unreadable. **Zero is a real answer**: it is
/// how the firmware says the slot is empty.
fn size(raw: &[u8], length: usize) -> Option<u64> {
    const EXTENDED: u16 = 0x7FFF;
    const UNKNOWN: u16 = 0xFFFF;

    let size = word(raw, length, 0x0C)?;
    if size == UNKNOWN {
        return Some(0);
    }
    if size == EXTENDED {
        // Extended Size arrived with SMBIOS 2.7 and is megabytes in the low 31 bits.
        let low = word(raw, length, 0x1C)?;
        let high = word(raw, length, 0x1E)?;
        let megabytes = u64::from(u32::from_le_bytes([
            low.to_le_bytes()[0],
            low.to_le_bytes()[1],
            high.to_le_bytes()[0],
            high.to_le_bytes()[1],
        ]) & 0x7FFF_FFFF);
        return Some(megabytes << 20);
    }
    // Bit 15 clear - megabytes; set - kilobytes.
    Some(if size & 0x8000 == 0 {
        u64::from(size) << 20
    } else {
        u64::from(size & 0x7FFF) << 10
    })
}

/// `0` and `0xFFFF` both mean "not filled in"; anything else is a real clock in MT/s.
fn speed(raw: Option<u16>) -> Option<u32> {
    raw.filter(|s| *s != 0 && *s != 0xFFFF).map(u32::from)
}

/// SMBIOS 3.8, table 76. Unknown and Other come back empty, so the row is left out.
fn memory_kind(code: u8) -> String {
    match code {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x14 => "DDR2 FB-DIMM",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x20 => "HBM",
        0x21 => "HBM2",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        0x24 => "HBM3",
        _ => "",
    }
    .to_string()
}

/// SMBIOS 3.8, table 75.
fn form_factor(code: u8) -> String {
    match code {
        0x03 => "SIMM",
        0x05 => "Chip",
        0x09 => "DIMM",
        0x0B => "Row of chips",
        0x0C => "RIMM",
        0x0D => "SODIMM",
        0x0F => "FB-DIMM",
        0x10 => "Die",
        _ => "",
    }
    .to_string()
}

/// The string table: NUL-terminated strings, the set ended by an empty one.
fn strings(tail: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = tail;
    while let Some(end) = rest.iter().position(|b| *b == 0) {
        if end == 0 {
            break;
        }
        out.push(String::from_utf8_lossy(&rest[..end]).trim().to_string());
        rest = &rest[end + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Type 17 record long enough to carry Configured Memory Speed, with four strings.
    fn record() -> Vec<u8> {
        let mut raw = vec![0u8; 0x28];
        raw[0x00] = 17;
        raw[0x01] = 0x28;
        raw[0x0C..0x0E].copy_from_slice(&16384u16.to_le_bytes()); // 16 GiB in MB
        raw[0x0E] = 0x09; // DIMM
        raw[0x10] = 1; // locator -> first string
        raw[0x11] = 2; // bank
        raw[0x12] = 0x1A; // DDR4
        raw[0x15..0x17].copy_from_slice(&3600u16.to_le_bytes());
        raw[0x17] = 3; // manufacturer
        raw[0x18] = 4; // serial, read past and never written out
        raw[0x1A] = 5; // part number
        raw[0x1B] = 0x02; // two ranks
        raw[0x20..0x22].copy_from_slice(&3200u16.to_le_bytes());
        raw[0x26..0x28].copy_from_slice(&1200u16.to_le_bytes());
        raw.extend_from_slice(b"DIMM_A1\0ChannelA\0Kingston\0SERIAL123\0KHX3200\0\0");
        raw
    }

    #[test]
    fn a_full_record_gives_every_field() {
        let module = parse(&record()).expect("parses");
        assert_eq!(module.locator, "DIMM_A1");
        assert_eq!(module.bank, "ChannelA");
        assert_eq!(module.size, 16 << 30);
        assert_eq!(module.kind, "DDR4");
        assert_eq!(module.form_factor, "DIMM");
        assert_eq!(module.speed_max, Some(3600));
        assert_eq!(module.speed_now, Some(3200), "the mode, not the rating");
        assert_eq!(module.vendor, "Kingston");
        assert_eq!(module.part, "KHX3200");
        assert_eq!(module.ranks, Some(2));
        assert_eq!(module.volts, Some(1200));
    }

    #[test]
    fn the_serial_number_never_leaves_the_record() {
        let module = parse(&record()).expect("parses");
        let text = format!("{module:?}");
        assert!(
            !text.contains("SERIAL123"),
            "the report is world-readable: {text}"
        );
    }

    #[test]
    fn a_short_record_stops_at_its_own_length() {
        // SMBIOS 2.3: the formatted area ends at Attributes, before Configured Memory Speed
        // and the voltages, and the string table starts right there. Reading the later
        // fields anyway would be reading the strings as numbers.
        let mut raw = vec![0u8; 0x1B];
        raw[0x00] = 17;
        raw[0x01] = 0x1B;
        raw[0x0C..0x0E].copy_from_slice(&16384u16.to_le_bytes());
        raw[0x0E] = 0x09;
        raw[0x10] = 1;
        raw[0x11] = 2;
        raw[0x12] = 0x1A;
        raw[0x15..0x17].copy_from_slice(&3600u16.to_le_bytes());
        raw[0x17] = 3;
        raw[0x1A] = 4;
        raw.extend_from_slice(b"DIMM_A1\0ChannelA\0Kingston\0KHX3200\0\0");

        let module = parse(&raw).expect("parses");
        assert_eq!(module.locator, "DIMM_A1", "the strings start at the length");
        assert_eq!(module.part, "KHX3200");
        assert_eq!(module.speed_max, Some(3600), "still inside the record");
        assert_eq!(module.speed_now, None, "past the end, so unanswered");
        assert_eq!(module.volts, None);
        assert_eq!(module.ranks, None, "attributes begin at the length itself");
    }

    #[test]
    fn an_empty_slot_is_a_record_with_no_size() {
        let mut raw = record();
        raw[0x0C..0x0E].copy_from_slice(&0u16.to_le_bytes());
        let module = parse(&raw).expect("parses");
        assert_eq!(module.size, 0);
        assert!(!module.populated());
        assert_eq!(module.locator, "DIMM_A1", "an empty slot still has a name");
    }

    #[test]
    fn a_large_module_comes_from_the_extended_field() {
        let mut raw = record();
        raw[0x0C..0x0E].copy_from_slice(&0x7FFFu16.to_le_bytes());
        raw[0x1C..0x20].copy_from_slice(&65536u32.to_le_bytes()); // 64 GiB in MB
        assert_eq!(parse(&raw).expect("parses").size, 64 << 30);
    }

    #[test]
    fn kilobyte_sizes_are_understood() {
        let mut raw = record();
        // Bit 15 set: the value is kilobytes, not megabytes.
        raw[0x0C..0x0E].copy_from_slice(&(0x8000u16 | 512).to_le_bytes());
        assert_eq!(parse(&raw).expect("parses").size, 512 << 10);
    }

    #[test]
    fn anything_that_is_not_a_memory_device_is_ignored() {
        let mut raw = record();
        raw[0x00] = 4; // Processor Information
        assert_eq!(parse(&raw), None);
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[17, 0x28]), None, "a length longer than the record");
    }

    #[test]
    fn an_unfilled_speed_is_not_a_speed() {
        let mut raw = record();
        raw[0x15..0x17].copy_from_slice(&0xFFFFu16.to_le_bytes());
        raw[0x20..0x22].copy_from_slice(&0u16.to_le_bytes());
        let module = parse(&raw).expect("parses");
        assert_eq!(module.speed_max, None);
        assert_eq!(module.speed_now, None);
    }

    #[test]
    fn a_machine_with_no_entries_reports_nothing() {
        assert!(collect(std::path::Path::new("/nonexistent/dmi/entries")).is_empty());
    }
}
