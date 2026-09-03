// SPDX-License-Identifier: GPL-3.0-only

//! What the memory modules are, as handed over by the collector.
//!
//! The kernel keeps SMBIOS at `0400 root`: `/sys/firmware/dmi/entries/17-*/raw` is the only
//! place that knows the size, speed and type of a module, and a desktop application cannot
//! read it. `wmde-memory-devices` reads it once at boot from a oneshot unit and writes this
//! file world-readable; the window only parses what is already there.
//!
//! The whole block is optional. No file - because the unit is not enabled, because the
//! firmware carries no memory records, or because this is a virtual machine - and the rows
//! are not drawn, the way `Facts::base_khz` is left out when `intel_pstate` is silent.
//!
//! Shared between the window and the collector through `#[path]`, so the shape is written
//! once and the two cannot drift apart.

use serde::{Deserialize, Serialize};

/// Where the collector leaves its report.
pub const REPORT_PATH: &str = "/run/wmde/memory-devices.ron";

/// One memory slot, populated or not.
///
/// Serial numbers and asset tags are deliberately absent: the report is world-readable, and
/// nothing on this page needs to say which particular module a machine carries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Module {
    /// `DIMM_A1`, `ChannelA-DIMM0`. Empty when the firmware gives no name.
    pub locator: String,
    /// `BANK 0`, `ChannelA`. The channel is read out of this or out of the locator.
    pub bank: String,
    /// Bytes. **Zero means the slot is empty**, which is what the firmware says with a size
    /// of zero, and empty slots stay in the list - they are what `used of slots` counts.
    pub size: u64,
    /// `DDR4`, `DDR5`, `LPDDR5`. Empty when the type is one of the two SMBIOS spellings of
    /// "no answer".
    pub kind: String,
    /// `DIMM`, `SODIMM`.
    pub form_factor: String,
    /// What the module is rated for, MT/s. `None` when the firmware does not say.
    pub speed_max: Option<u32>,
    /// What it is actually clocked at, MT/s. This is the "mode" half: a 3600 module in a
    /// board that runs it at 3200 says both numbers, and only this one is the truth.
    pub speed_now: Option<u32>,
    pub vendor: String,
    pub part: String,
    /// Ranks, when the firmware fills the attributes byte.
    pub ranks: Option<u8>,
    /// Configured voltage, millivolts.
    pub volts: Option<u32>,
}

impl Module {
    #[must_use]
    pub fn populated(&self) -> bool {
        self.size > 0
    }

    /// The channel this slot sits on, as a single character: `A`, `B`, `0`, `1`.
    ///
    /// SMBIOS has no channel field. Every firmware writes it into a name instead, and in one
    /// of two shapes: `ChannelA-DIMM0` or `DIMM_A1`. Anything else returns `None` and the
    /// channel row is left out rather than guessed.
    #[must_use]
    pub fn channel(&self) -> Option<char> {
        for text in [self.bank.as_str(), self.locator.as_str()] {
            if let Some(c) = channel_of(text) {
                return Some(c);
            }
        }
        None
    }
}

/// `...Channel<c>...` first, then `..._<c><digit>`; the letter is upper-cased.
fn channel_of(text: &str) -> Option<char> {
    let lower = text.to_ascii_lowercase();
    if let Some(at) = lower.find("channel") {
        let rest = &text[at + "channel".len()..];
        let c = rest
            .chars()
            .find(|c| !c.is_whitespace() && *c != '-' && *c != '_')?;
        if c.is_ascii_alphanumeric() {
            return Some(c.to_ascii_uppercase());
        }
    }
    // `DIMM_A1`, `DIMM_B2`: a separator, one letter, one digit.
    let bytes = text.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        if (bytes[i] == b'_' || bytes[i] == b'-' || bytes[i] == b' ')
            && bytes[i + 1].is_ascii_alphabetic()
            && bytes[i + 2].is_ascii_digit()
        {
            return Some(bytes[i + 1].to_ascii_uppercase() as char);
        }
    }
    None
}

/// Every slot the firmware describes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Report {
    pub modules: Vec<Module>,
}

impl Report {
    /// Reads the report, or `None` when there is none to read.
    ///
    /// Any failure is the same answer: the block is not drawn. A missing unit, a machine
    /// whose firmware lists no memory devices and a truncated file are all "the machine did
    /// not say", and none of them is worth an error in front of the user.
    #[must_use]
    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(REPORT_PATH).ok()?;
        let report: Self = ron::from_str(&text)
            .inspect_err(|err| tracing::warn!("{REPORT_PATH}: {err}"))
            .ok()?;
        (!report.modules.is_empty()).then_some(report)
    }

    #[must_use]
    pub fn slots(&self) -> usize {
        self.modules.len()
    }

    #[must_use]
    pub fn used(&self) -> usize {
        self.modules.iter().filter(|m| m.populated()).count()
    }

    #[must_use]
    pub fn total(&self) -> u64 {
        self.modules.iter().map(|m| m.size).sum()
    }

    /// The one value every populated slot agrees on, or `None` when they disagree.
    ///
    /// A machine with two different modules has no single type or form factor to print, and
    /// printing the first one would be a guess dressed as a fact.
    fn agreed<T: PartialEq, F: Fn(&Module) -> T>(&self, of: F) -> Option<T> {
        let mut populated = self.modules.iter().filter(|m| m.populated());
        let first = of(populated.next()?);
        populated.all(|m| of(m) == first).then_some(first)
    }

    #[must_use]
    pub fn kind(&self) -> Option<String> {
        self.agreed(|m| m.kind.clone()).filter(|k| !k.is_empty())
    }

    #[must_use]
    pub fn form_factor(&self) -> Option<String> {
        self.agreed(|m| m.form_factor.clone())
            .filter(|f| !f.is_empty())
    }

    /// Running speed and rated speed, in MT/s.
    #[must_use]
    pub fn speed(&self) -> Option<(u32, Option<u32>)> {
        let now = self.agreed(|m| m.speed_now)??;
        let max = self.agreed(|m| m.speed_max).flatten().filter(|m| *m != now);
        Some((now, max))
    }

    /// How many distinct channels carry a populated slot.
    #[must_use]
    pub fn channels(&self) -> Option<usize> {
        let mut seen = Vec::new();
        for module in self.modules.iter().filter(|m| m.populated()) {
            let channel = module.channel()?;
            if !seen.contains(&channel) {
                seen.push(channel);
            }
        }
        (!seen.is_empty()).then_some(seen.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(locator: &str, bank: &str, size: u64) -> Module {
        Module {
            locator: locator.to_string(),
            bank: bank.to_string(),
            size,
            ..Module::default()
        }
    }

    #[test]
    fn the_channel_is_read_out_of_either_name() {
        assert_eq!(channel_of("ChannelA-DIMM0"), Some('A'));
        assert_eq!(channel_of("Channel-B"), Some('B'));
        assert_eq!(channel_of("DIMM_A1"), Some('A'));
        assert_eq!(channel_of("Controller0-ChannelA-DIMM1"), Some('A'));
        // Nothing that names a channel: guessing here would invent a layout.
        assert_eq!(channel_of("BANK 0"), None);
        assert_eq!(channel_of(""), None);
    }

    #[test]
    fn empty_slots_are_counted_but_not_described() {
        let report = Report {
            modules: vec![
                module("DIMM_A1", "BANK 0", 16 << 30),
                module("DIMM_A2", "BANK 1", 0),
                module("DIMM_B1", "BANK 2", 16 << 30),
                module("DIMM_B2", "BANK 3", 0),
            ],
        };
        assert_eq!(report.slots(), 4);
        assert_eq!(report.used(), 2);
        assert_eq!(report.total(), 32 << 30);
        assert_eq!(
            report.channels(),
            Some(2),
            "two populated slots, two channels"
        );
    }

    #[test]
    fn a_disagreement_is_not_printed() {
        let mut ddr4 = module("DIMM_A1", "", 8 << 30);
        ddr4.kind = "DDR4".to_string();
        ddr4.speed_now = Some(3200);
        let mut ddr5 = module("DIMM_B1", "", 8 << 30);
        ddr5.kind = "DDR5".to_string();
        ddr5.speed_now = Some(4800);

        let same = Report {
            modules: vec![ddr4.clone(), ddr4.clone()],
        };
        assert_eq!(same.kind().as_deref(), Some("DDR4"));
        assert_eq!(same.speed(), Some((3200, None)), "no rated speed to add");

        let mixed = Report {
            modules: vec![ddr4, ddr5],
        };
        assert_eq!(mixed.kind(), None);
        assert_eq!(mixed.speed(), None);
    }

    #[test]
    fn the_rated_speed_shows_only_when_it_differs() {
        let mut module = module("DIMM_A1", "", 8 << 30);
        module.speed_now = Some(3200);
        module.speed_max = Some(3600);
        let report = Report {
            modules: vec![module.clone()],
        };
        assert_eq!(report.speed(), Some((3200, Some(3600))));

        let mut matched = module;
        matched.speed_max = Some(3200);
        let report = Report {
            modules: vec![matched],
        };
        assert_eq!(report.speed(), Some((3200, None)));
    }

    #[test]
    fn the_report_survives_a_round_trip() {
        let report = Report {
            modules: vec![Module {
                locator: "DIMM_A1".to_string(),
                bank: "BANK 0".to_string(),
                size: 16 << 30,
                kind: "DDR4".to_string(),
                form_factor: "DIMM".to_string(),
                speed_max: Some(3600),
                speed_now: Some(3200),
                vendor: "Kingston".to_string(),
                part: "KHX3200C16D4/16GX".to_string(),
                ranks: Some(2),
                volts: Some(1200),
            }],
        };
        let text = ron::ser::to_string_pretty(&report, ron::ser::PrettyConfig::default())
            .expect("serialises");
        assert_eq!(ron::from_str::<Report>(&text).expect("parses"), report);
    }
}
