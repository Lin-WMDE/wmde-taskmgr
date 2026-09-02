// SPDX-License-Identifier: GPL-3.0-only

//! The load page: a rail of devices on the left, the chosen one drawn large on the right.

use cosmic::iced::{Alignment, Length};
use cosmic::iced::widget::canvas;
use cosmic::prelude::*;
use cosmic::widget;
use wmde_sysmon::graph::{Colors, Kind, Metrics, Ring, Strip, Style};
use wmde_sysmon::probe::{Probe, Sample, Sources};

use crate::facts::Facts;
use crate::fl;

/// Seconds of history, which is what the reference shows and labels.
const WINDOW: usize = 60;

/// Side of a grid square, in pixels. Measured off `ref/taskman/`, where the grid holds its
/// cell size whatever the plot is sized to. The theme has no step for it: the grid belongs to
/// the graph, not to the layout around it.
const GRID_CELL: f32 = 40.0;

const PLOT_HEIGHT: f32 = 240.0;
/// A per-core tile never goes below this, however many cores there are.
const TILE_MIN_HEIGHT: f32 = 60.0;
/// Height over width of a per-core tile, measured off `ref/taskman/cpu.png`: 187 by 223.
///
/// The tiles keep this shape instead of stretching to whatever height the page has left, so
/// the grid ends where it ends and the numbers under it sit right below the last row.
const TILE_ASPECT: f32 = 187.0 / 223.0;
const THUMB_WIDTH: f32 = 60.0;
const THUMB_HEIGHT: f32 = 34.0;
const RAIL_WIDTH: f32 = 220.0;
const FACTS_WIDTH: f32 = 190.0;
/// Columns for `n` per-core tiles: a square-ish grid, rows and columns both.
///
/// Four logical processors give two by two, which is exactly what the reference draws; nine
/// give three by three, sixteen four by four. A single row is what this replaced, and it left
/// the tiles too thin to read at any core count worth looking at.
fn tile_columns(n: usize) -> usize {
    (n as f64).sqrt().ceil() as usize
}

/// The devices the rail lists, in the reference's order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Device {
    #[default]
    Cpu,
    Memory,
    Disk,
    Network,
}

impl Device {
    pub const ALL: [Self; 4] = [Self::Cpu, Self::Memory, Self::Disk, Self::Network];

    const fn kind(self) -> Kind {
        match self {
            Self::Cpu => Kind::Cpu,
            Self::Memory => Kind::Memory,
            Self::Disk => Kind::Disk,
            Self::Network => Kind::Network,
        }
    }

    fn title(self) -> String {
        match self {
            Self::Cpu => fl!("device-cpu"),
            Self::Memory => fl!("device-memory"),
            Self::Disk => fl!("device-disk"),
            Self::Network => fl!("device-network"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Select(Device),
    /// Between one graph per logical processor and one for the machine.
    TogglePerCore,
}

/// One ring per graph kind, which is the shape [`Strip`] indexes.
type Rings = [Ring; Kind::COUNT];

fn rings() -> Rings {
    std::array::from_fn(|i| Ring::new(Kind::ALL[i].bands().len(), WINDOW))
}

pub struct Performance {
    probe: Probe,
    last: Sample,
    history: Rings,
    /// One set per logical processor, filled only while the per-core view is on.
    cores: Vec<Rings>,
    /// A cache per plot: the large one, then the rail thumbnails, then the core tiles.
    caches: Vec<canvas::Cache>,
    colors: Colors,
    pub device: Device,
    pub per_core: bool,
    /// What the machine is, read once.
    facts: Facts,
    /// Processes and threads as of the last walk, handed in by the application: this half
    /// samples the machine, not `/proc/*/`.
    pub counts: (usize, u32),
}

impl Default for Performance {
    fn default() -> Self {
        Self::new()
    }
}

impl Performance {
    #[must_use]
    pub fn new() -> Self {
        Self {
            probe: Probe::new(),
            last: Sample::default(),
            history: rings(),
            cores: Vec::new(),
            caches: (0..=Device::ALL.len()).map(|_| canvas::Cache::new()).collect(),
            colors: Colors::default(),
            device: Device::default(),
            // The reference opens on the tiles, and a machine busy on one core out of sixteen
            // reads as idle on the aggregate.
            per_core: true,
            facts: Facts::load(),
            counts: (0, 0),
        }
    }

    /// Read every source once and push a column onto each history.
    pub fn tick(&mut self) {
        let want = Sources {
            cpu: true,
            cpu_cores: self.per_core,
            memory: true,
            network: true,
            swap: false,
            load: false,
            disk: true,
        };
        let s = self.probe.sample(want);
        self.last = s;

        let total = s.mem_total.max(1) as f32;
        self.history[Kind::Cpu.index()].push(&s.cpu);
        self.history[Kind::Memory.index()].push(&[
            s.mem[0] as f32 / total,
            s.mem[1] as f32 / total,
            s.mem[2] as f32 / total,
        ]);
        self.history[Kind::Network.index()].push(&[s.net[0] as f32, s.net[1] as f32]);
        self.history[Kind::Disk.index()].push(&[s.disk[0] as f32, s.disk[1] as f32]);

        if self.per_core {
            let cores = self.probe.cpu_cores().len();
            if self.cores.len() != cores {
                self.cores = (0..cores).map(|_| rings()).collect();
            }
            for (i, set) in self.cores.iter_mut().enumerate() {
                set[Kind::Cpu.index()].push(&self.probe.cpu_cores()[i]);
            }
        } else {
            self.cores.clear();
        }

        let needed = 1 + Device::ALL.len() + self.cores.len();
        while self.caches.len() < needed {
            self.caches.push(canvas::Cache::new());
        }
        // A cache holds the last drawing; a new sample makes every one of them stale.
        for cache in &self.caches {
            cache.clear();
        }
    }

    /// Throw away every cached drawing.
    ///
    /// A `canvas::Cache` is keyed by the bounds it was drawn at, and every large plot has the
    /// same bounds - so a cache still holding the previous device's curve is handed back
    /// verbatim for the new one.
    pub fn invalidate(&self) {
        for cache in &self.caches {
            cache.clear();
        }
    }

    /// One plot, sized to whatever the layout gives it.
    ///
    /// Wrapped in `responsive` because [`Strip`] is a fixed-size widget: it has to be told a
    /// width, and the width is only known once the row around it has been laid out.
    fn plot<'a>(
        &'a self,
        kind: Kind,
        history: &'a Rings,
        cache: usize,
        height: f32,
    ) -> Element<'a, Message> {
        let cache = &self.caches[cache.min(self.caches.len() - 1)];
        widget::responsive(move |size| {
            Strip::new(
                &Kind::ALL[kind.index()..=kind.index()],
                history,
                &self.colors,
                cache,
                Metrics {
                    horizontal: true,
                    length: size.width.max(1.0),
                    thickness: height,
                    gap: 0.0,
                },
            )
            .style(Style::Curve { cell: GRID_CELL })
            .into()
        })
        .height(Length::Fixed(height))
        .into()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();

        let mut rail = widget::column::with_capacity(Device::ALL.len()).spacing(spacing.space_xxxs);
        for (i, device) in Device::ALL.into_iter().enumerate() {
            let entry = widget::row::with_capacity(2)
                .spacing(spacing.space_xs)
                .align_y(Alignment::Center)
                .push(
                    widget::container(self.plot(device.kind(), &self.history, 1 + i, THUMB_HEIGHT))
                        .width(Length::Fixed(THUMB_WIDTH)),
                )
                .push(
                    widget::column::with_capacity(2)
                        .push(widget::text::body(device.title()))
                        .push(widget::text::caption(self.summary(device))),
                );
            rail = rail.push(
                widget::button::custom(entry)
                    .width(Length::Fill)
                    .padding(spacing.space_xxs)
                    .selected(self.device == device)
                    .class(cosmic::theme::Button::ListItem([0.0; 4]))
                    .on_press(Message::Select(device)),
            );
        }

        let mut main = widget::column::with_capacity(5)
            .spacing(spacing.space_xxs)
            .push(widget::text::heading(self.device.title()));

        if self.device == Device::Cpu && self.per_core && !self.cores.is_empty() {
            main = main.push(self.core_tiles(spacing.space_xxxs));
        } else {
            main = main.push(self.plot(self.device.kind(), &self.history, 0, PLOT_HEIGHT));
        }

        main = main
            .push(widget::text::caption(last_seconds()))
            .push(widget::text::body(self.summary(self.device)));

        if self.device == Device::Cpu {
            main = main.push(
                widget::button::text(if self.per_core {
                    fl!("show-overall")
                } else {
                    fl!("show-per-core")
                })
                .on_press(Message::TogglePerCore),
            );
        }

        let mut row = widget::row::with_capacity(3)
            .spacing(spacing.space_s)
            .padding(spacing.space_s)
            .push(widget::container(rail).width(Length::Fixed(RAIL_WIDTH)))
            .push(widget::container(main).width(Length::Fill));
        if self.device == Device::Cpu {
            row = row.push(widget::container(self.facts_panel()).width(Length::Fixed(FACTS_WIDTH)));
        }
        row.into()
    }

    /// The last sample, for whoever needs the machine-wide figures.
    #[must_use]
    pub fn last(&self) -> &Sample {
        &self.last
    }

    /// What the machine is doing, and beside it what the machine is.
    fn facts_panel(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let busy: f32 = self.last.cpu.iter().sum();
        let (processes, threads) = self.counts;

        let pair = |label: String, value: String| {
            widget::column::with_capacity(2)
                .push(widget::text::caption(label))
                .push(widget::text::body(value))
        };
        let line = |label: String, value: String| {
            widget::row::with_capacity(2)
                .spacing(spacing.space_xxs)
                .push(widget::text::caption(label).width(Length::Fill))
                .push(widget::text::caption(value))
        };

        let mut panel = widget::column::with_capacity(12)
            .spacing(spacing.space_xxs)
            .push(pair(
                fl!("fact-utilization"),
                format!("{:.0}%", busy * 100.0),
            ))
            .push(pair(fl!("fact-processes"), processes.to_string()))
            .push(pair(fl!("fact-threads"), threads.to_string()))
            .push(pair(fl!("fact-uptime"), uptime()));

        if !self.facts.model.is_empty() {
            panel = panel.push(widget::text::caption(self.facts.model.clone()));
        }
        panel = panel
            .push(line(fl!("fact-cpus"), self.facts.cpus.to_string()))
            .push(line(fl!("fact-sockets"), self.facts.sockets.to_string()))
            .push(line(
                fl!("fact-virtual"),
                if self.facts.virtualized {
                    fl!("fact-yes")
                } else {
                    fl!("fact-no")
                },
            ));
        for (level, size) in &self.facts.caches {
            panel = panel.push(line(level.clone(), size.clone()));
        }
        panel.into()
    }

    /// One plot per logical processor, laid out in a grid.
    ///
    /// Wrapped in `responsive` for the same reason a single plot is: the tiles are drawn by a
    /// fixed-size widget, and how wide each one may be is only known once the page around it
    /// has been laid out.
    fn core_tiles(&self, gap: u16) -> Element<'_, Message> {
        let count = self.cores.len();
        let columns = tile_columns(count).max(1);
        let lines = count.div_ceil(columns);

        widget::responsive(move |size| {
            let gaps = gap as f32 * (columns.saturating_sub(1)) as f32;
            let width = ((size.width - gaps) / columns as f32).max(1.0);
            let mut tile = (width * TILE_ASPECT).max(TILE_MIN_HEIGHT);

            // The shape gives way before the page does: a machine with many cores would
            // otherwise push the caption and the numbers off the bottom.
            let rows_gaps = gap as f32 * (lines.saturating_sub(1)) as f32;
            let room = (size.height - rows_gaps) / lines as f32;
            if room > 1.0 && room < tile {
                tile = room.max(1.0);
            }

            let mut grid = widget::column::with_capacity(lines).spacing(gap);
            for line in 0..lines {
                let mut row = widget::row::with_capacity(columns).spacing(gap);
                for slot in 0..columns {
                    let i = line * columns + slot;
                    row = match self.cores.get(i) {
                        Some(set) => row.push(
                            widget::container(self.plot(
                                Kind::Cpu,
                                set,
                                1 + Device::ALL.len() + i,
                                tile,
                            ))
                            .width(Length::FillPortion(1)),
                        ),
                        // The last row is padded rather than stretched, so a machine with six
                        // cores does not draw two double-width tiles.
                        None => row.push(widget::Space::new().width(Length::FillPortion(1))),
                    };
                }
                grid = grid.push(row);
            }
            grid.into()
        })
        .into()
    }

    /// The line under a device's name: what it is doing right now.
    fn summary(&self, device: Device) -> String {
        let s = &self.last;
        match device {
            Device::Cpu => {
                let busy: f32 = s.cpu.iter().sum();
                fl!("summary-cpu", value = format!("{:.0}%", busy * 100.0))
            }
            Device::Memory => fl!(
                "summary-memory",
                used = size(s.mem[0]),
                total = size(s.mem_total)
            ),
            Device::Disk => fl!(
                "summary-disk",
                read = rate(s.disk[0]),
                write = rate(s.disk[1])
            ),
            Device::Network => fl!("summary-network", rx = rate(s.net[0]), tx = rate(s.net[1])),
        }
    }
}

fn last_seconds() -> String {
    let count = WINDOW as i64;
    fl!("last-seconds", count = count)
}

/// How long the machine has been up, as the reference writes it: `d:hh:mm:ss`.
fn uptime() -> String {
    let secs = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    let (d, h, m, s) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    format!("{d}:{h:02}:{m:02}:{s:02}")
}

/// Bytes as the largest unit that leaves a number worth reading.
fn size(bytes: u64) -> String {
    let value = bytes as f64;
    if value >= 1024.0 * 1024.0 * 1024.0 {
        fl!(
            "size-gib",
            value = format!("{:.1}", value / (1024.0 * 1024.0 * 1024.0))
        )
    } else if value >= 1024.0 * 1024.0 {
        fl!("size-mib", value = format!("{:.0}", value / (1024.0 * 1024.0)))
    } else {
        fl!("size-kib", value = format!("{:.0}", value / 1024.0))
    }
}

fn rate(per_second: f64) -> String {
    if per_second >= 1024.0 * 1024.0 {
        fl!(
            "rate-mib",
            value = format!("{:.1}", per_second / (1024.0 * 1024.0))
        )
    } else if per_second >= 1024.0 {
        fl!("rate-kib", value = format!("{:.0}", per_second / 1024.0))
    } else {
        fl!("rate-b", value = format!("{per_second:.0}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference draws four logical processors two by two, and that is the whole point of
    /// the rule: rows AND columns, not one long row.
    #[test]
    fn four_processors_make_a_two_by_two_grid() {
        assert_eq!(tile_columns(4), 2);
        assert_eq!(4_usize.div_ceil(tile_columns(4)), 2);
    }

    #[test]
    fn the_grid_stays_square_ish() {
        for (n, cols) in [(1, 1), (2, 2), (4, 2), (6, 3), (9, 3), (12, 4), (16, 4), (20, 5)] {
            assert_eq!(tile_columns(n), cols, "{n} processors");
            let rows = n.div_ceil(cols);
            assert!(rows <= cols, "{n} processors give {rows} rows of {cols} columns");
        }
    }

    /// A tile keeps the reference's shape rather than stretching: 187 by 223.
    #[test]
    fn a_tile_is_shaped_like_the_reference() {
        let width = 223.0_f32;
        assert!((width * TILE_ASPECT - 187.0).abs() < 0.5);
    }
}
