// SPDX-License-Identifier: GPL-3.0-only

//! The load page: a rail of devices on the left, the chosen one drawn large on the right.
//!
//! Laid out after `ref/mission-center/cpu-dark.png`: heading and processor model on one line,
//! the time window and the top of the scale labelled above the plot, the tiles filling what
//! is left, and the facts panel down the right. Nothing is drawn under the tiles - the
//! per-core switch lives in the graph's context menu, as it does in the reference.

use std::collections::HashMap;

use cosmic::iced::widget::canvas;
use cosmic::iced::widget::text::Wrapping;
use cosmic::iced::{Alignment, Color, Length};
use cosmic::prelude::*;
use cosmic::widget;
use wmde_sysmon::graph::{Colors, Kind, Metrics, Ring, Strip, Style};
use wmde_sysmon::probe::{Probe, Sample, Sources};

use crate::facts::{Facts, Live};
use crate::fl;

/// Seconds of history, which is what the reference shows and labels.
const WINDOW: usize = 60;

/// Cells of the grid under a curve, across and up.
///
/// A count, not a size. The reference divides a tile into the same grid however large the
/// tile is, so a cell always means one thing - ten seconds across at a window of sixty, a
/// tenth of the scale up - and no stub cell is left over against an edge. A fixed cell side
/// in pixels cannot do either.
const GRID_COLS: u16 = 6;
const GRID_ROWS: u16 = 10;

const PLOT_HEIGHT: f32 = 240.0;
/// A per-core tile never goes below this, however many cores there are.
const TILE_MIN_HEIGHT: f32 = 60.0;
/// Height over width of a per-core tile, measured off `ref/taskman/cpu.png`: 187 by 223.
///
/// `ref/mission-center/cpu-dark.png` draws 149 by 124, which is the same shape to within a
/// pixel. Two references that share no ancestry agreeing is why this is a constant and not a
/// guess. The tiles keep this shape instead of stretching to whatever height the page has
/// left, so the grid ends where it ends.
const TILE_ASPECT: f32 = 187.0 / 223.0;
const THUMB_WIDTH: f32 = 80.0;
const THUMB_HEIGHT: f32 = 54.0;
const RAIL_WIDTH: f32 = 278.0;
const FACTS_WIDTH: f32 = 250.0;
/// Width of the key column in the facts list, so every value starts on one line.
const FACT_KEY_WIDTH: f32 = 148.0;
/// Gap between rows of the rail. Below any theme step on purpose: at 74 px a row is tall
/// enough that four pixels of air read as a gap between blocks rather than between rows.
const RAIL_GAP: u16 = 2;

/// Columns for `n` per-core tiles: a square-ish grid, never wider than it is tall.
///
/// Four logical processors give two by two, nine three by three, sixteen four by four. For a
/// count between two squares the reference is the tie-breaker: it lays twenty out in **four
/// columns by five rows**, so the floor of the root is taken and not the ceiling. The
/// transpose looked equally reasonable until the reference was measured.
fn tile_columns(n: usize) -> usize {
    n.isqrt().max(1)
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

    /// The top of the scale, where the scale has a top worth naming.
    ///
    /// Processor and memory are drawn against the whole machine and stop at a hundred per
    /// cent. Disk and network are drawn against whatever the busiest moment of the window
    /// was, so there is no fixed ceiling to label and none is invented.
    fn scale_top(self) -> Option<String> {
        matches!(self, Self::Cpu | Self::Memory).then(|| percent(1.0))
    }
}

/// What the context menu over the graph offers.
///
/// Both entries carry the state they set rather than a toggle: a menu that says
/// "За логічними процесорами" and flips to the opposite of whatever is showing would read
/// as a command in one state and as a label in the other.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    PerCore,
    Overall,
}

impl widget::menu::Action for Action {
    type Message = Message;

    fn message(&self) -> Self::Message {
        Message::PerCore(matches!(self, Self::PerCore))
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Select(Device),
    /// One graph per logical processor, or one for the machine.
    PerCore(bool),
}

/// One ring per graph kind, which is the shape [`Strip`] indexes.
type Rings = [Ring; Kind::COUNT];

fn rings() -> Rings {
    std::array::from_fn(|i| Ring::new(Kind::ALL[i].bands().len(), WINDOW))
}

/// One colour per device, taken from the theme's roles.
///
/// The panel applet colours a band by **what the load is** - user against system against
/// iowait - because in twenty pixels of height that is the only question worth answering.
/// A window has room to draw each device apart, and then the colour is free to mean the
/// device instead, which is what both the reference and every task manager before it does.
///
/// One `Colors` covers all four because the first band of each kind is a different band:
/// `CpuUser`, `MemUsed`, `NetIn`, `DiskRead`. Under [`Style::Curve`] the first band is the
/// only one drawn, so nothing else needs overriding.
fn device_colors() -> Colors {
    let theme = cosmic::theme::active();
    let palette = theme.cosmic();
    let mut colors = Colors::default();
    for device in Device::ALL {
        let color = Color::from(match device {
            Device::Cpu => palette.accent_color(),
            Device::Memory => palette.warning_color(),
            Device::Disk => palette.success_color(),
            Device::Network => palette.destructive_color(),
        });
        colors.bands[device.kind().bands()[0] as usize] = Some(color);
    }
    colors
}

pub struct Performance {
    probe: Probe,
    last: Sample,
    history: Rings,
    /// One set per logical processor, filled only while the per-core view is on.
    cores: Vec<Rings>,
    /// A cache per plot: the large one, then the rail thumbnails, then the core tiles.
    caches: Vec<canvas::Cache>,
    /// Rebuilt every tick: the palette moves when the theme does, and a colour taken once at
    /// start-up would keep the old theme's accent for the life of the window.
    colors: Colors,
    pub device: Device,
    pub per_core: bool,
    /// What the machine is, read once.
    facts: Facts,
    /// The two figures of the facts panel that do not sit still.
    live: Live,
    speed_khz: Option<u64>,
    handles: Option<u64>,
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
            colors: device_colors(),
            device: Device::default(),
            // The reference opens on the tiles, and a machine busy on one core out of sixteen
            // reads as idle on the aggregate.
            per_core: true,
            facts: Facts::load(),
            live: Live::new(),
            speed_khz: None,
            handles: None,
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

        self.colors = device_colors();
        self.speed_khz = self.live.speed_khz();
        self.handles = Live::handles();

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
        style: Style,
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
            .style(style)
            .into()
        })
        .height(Length::Fixed(height))
        .into()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();

        let mut rail = widget::column::with_capacity(Device::ALL.len()).spacing(RAIL_GAP);
        for (i, device) in Device::ALL.into_iter().enumerate() {
            // No grid in a thumbnail: the reference draws none, and six by ten inside eighty
            // by fifty-four pixels would be a hatch rather than a grid.
            let thumb = self.plot(
                device.kind(),
                &self.history,
                1 + i,
                THUMB_HEIGHT,
                Style::Curve { cols: 1, rows: 1 },
            );
            let entry = widget::row::with_capacity(2)
                .spacing(spacing.space_xs)
                .align_y(Alignment::Center)
                .push(widget::container(thumb).width(Length::Fixed(THUMB_WIDTH)))
                .push(
                    // Three lines, as the reference has: what the device is called, what it
                    // is, and what it is doing. None of them wraps - a wrapped line makes one
                    // row taller than its neighbours and the rail stops reading as a list.
                    widget::column::with_capacity(3)
                        .push(widget::text::body(device.title()).wrapping(Wrapping::None))
                        .push(widget::text::caption(self.detail(device)).wrapping(Wrapping::None))
                        .push(widget::text::caption(self.reading(device)).wrapping(Wrapping::None)),
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

        // Heading line: the device on the left, what the processor is on the right, against
        // the far edge of the plot. The model used to sit in the facts panel, where it cost a
        // row of a column that has rows to spare.
        let mut heading = widget::row::with_capacity(3)
            .align_y(Alignment::Center)
            .push(widget::text::title3(self.device.title()))
            .push(widget::Space::new().width(Length::Fill));
        if self.device == Device::Cpu && !self.facts.model.is_empty() {
            heading = heading.push(widget::text::body(self.facts.model.clone()));
        }

        // Axis labels, above the plot and not below it: the window on the left, the top of
        // the scale on the right.
        let mut labels = widget::row::with_capacity(3)
            .push(widget::text::caption(last_seconds()))
            .push(widget::Space::new().width(Length::Fill));
        if let Some(top) = self.device.scale_top() {
            labels = labels.push(widget::text::caption(top));
        }

        let graph: Element<'_, Message> =
            if self.device == Device::Cpu && self.per_core && !self.cores.is_empty() {
                self.core_tiles(spacing.space_xxs)
            } else {
                self.plot(
                    self.device.kind(),
                    &self.history,
                    0,
                    PLOT_HEIGHT,
                    Style::Curve {
                        cols: GRID_COLS,
                        rows: GRID_ROWS,
                    },
                )
            };
        let graph = if self.device == Device::Cpu {
            widget::context_menu(graph, Some(self.graph_menu())).into()
        } else {
            graph
        };

        let main = widget::column::with_capacity(3)
            .spacing(spacing.space_xxs)
            .push(heading)
            .push(labels)
            .push(graph);

        let mut row = widget::row::with_capacity(4)
            .push(widget::container(rail).width(Length::Fixed(RAIL_WIDTH)))
            .push(widget::divider::vertical::default())
            .push(
                widget::container(main)
                    .width(Length::Fill)
                    .padding(spacing.space_s),
            );
        if self.device == Device::Cpu {
            row = row.push(
                widget::container(self.facts_panel())
                    .width(Length::Fixed(FACTS_WIDTH))
                    .padding(spacing.space_s),
            );
        }
        row.into()
    }

    /// The last sample, for whoever needs the machine-wide figures.
    #[must_use]
    pub fn last(&self) -> &Sample {
        &self.last
    }

    /// What the graph offers on a right click: the two views, the current one ticked.
    fn graph_menu(&self) -> Vec<widget::menu::Tree<Message>> {
        widget::menu::items(
            &HashMap::new(),
            vec![
                widget::menu::Item::CheckBox(
                    fl!("show-per-core"),
                    None,
                    self.per_core,
                    Action::PerCore,
                ),
                widget::menu::Item::CheckBox(
                    fl!("show-overall"),
                    None,
                    !self.per_core,
                    Action::Overall,
                ),
            ],
        )
    }

    /// What the machine is doing, and under it what the machine is.
    ///
    /// The top group is a grid rather than a stack: the reference fits six figures into the
    /// height a single column spends on four, and the pairing carries meaning of its own -
    /// how busy against how fast, and the three counts of things the kernel is holding open.
    fn facts_panel(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let busy: f32 = self.last.cpu.iter().sum();
        let (processes, threads) = self.counts;

        // A step between the label and its figure. The reference makes the figure larger;
        // the stack has one interface size and four sizes reserved for headings, so the step
        // here is weight. Recorded in .doc/taskmgr/04-appearance.md as a divergence.
        let pair = |label: String, value: String| {
            widget::column::with_capacity(2)
                .push(widget::text::caption(label).wrapping(Wrapping::None))
                .push(widget::text::heading(value).wrapping(Wrapping::None))
                .apply(Element::from)
        };
        let line = |label: String, value: String| {
            widget::row::with_capacity(2)
                .push(
                    widget::text::caption(label)
                        .wrapping(Wrapping::None)
                        .width(Length::Fixed(FACT_KEY_WIDTH)),
                )
                .push(widget::text::caption(value).wrapping(Wrapping::None))
        };

        let mut top = vec![pair(fl!("fact-utilization"), percent(busy))];
        if let Some(khz) = self.speed_khz {
            top.push(pair(fl!("fact-speed"), gigahertz(khz)));
        }
        let mut counts = vec![
            pair(fl!("fact-processes"), processes.to_string()),
            pair(fl!("fact-threads"), threads.to_string()),
        ];
        if let Some(handles) = self.handles {
            counts.push(pair(fl!("fact-handles"), handles.to_string()));
        }

        let panel = widget::column::with_capacity(16)
            .spacing(spacing.space_xxs)
            .push(group(top))
            .push(group(counts))
            .push(group(vec![pair(fl!("fact-uptime"), uptime())]));

        // The list is tighter than the group above it: these are rows of a table, not
        // figures, and the reference pitches them at twenty pixels against the group's fifty.
        let mut list = widget::column::with_capacity(11).spacing(spacing.space_xxxs);
        if let Some(khz) = self.facts.base_khz {
            list = list.push(line(fl!("fact-base-speed"), gigahertz(khz)));
        }
        list = list
            .push(line(fl!("fact-sockets"), self.facts.sockets.to_string()))
            .push(line(fl!("fact-cpus"), self.facts.cpus.to_string()));
        if !self.facts.virtualization.is_empty() {
            list = list.push(line(
                fl!("fact-virtualization"),
                self.facts.virtualization.clone(),
            ));
        }
        list = list.push(line(
            fl!("fact-virtual"),
            if self.facts.virtualized {
                fl!("fact-yes")
            } else {
                fl!("fact-no")
            },
        ));
        for (level, size_bytes) in &self.facts.caches {
            list = list.push(line(
                fl!("fact-cache", level = level.clone()),
                size(*size_bytes),
            ));
        }
        if !self.facts.cpufreq_driver.is_empty() {
            list = list.push(line(
                fl!("fact-cpufreq-driver"),
                self.facts.cpufreq_driver.clone(),
            ));
        }
        if !self.facts.cpufreq_governor.is_empty() {
            list = list.push(line(
                fl!("fact-cpufreq-governor"),
                self.facts.cpufreq_governor.clone(),
            ));
        }

        panel.push(list).into()
    }

    /// One plot per logical processor, laid out in a grid.
    ///
    /// Wrapped in `responsive` for the same reason a single plot is: the tiles are drawn by a
    /// fixed-size widget, and how wide each one may be is only known once the page around it
    /// has been laid out.
    fn core_tiles(&self, gap: u16) -> Element<'_, Message> {
        let count = self.cores.len();
        let columns = tile_columns(count);
        let lines = count.div_ceil(columns);

        widget::responsive(move |size| {
            let gaps = gap as f32 * (columns.saturating_sub(1)) as f32;
            let width = ((size.width - gaps) / columns as f32).max(1.0);
            let mut tile = (width * TILE_ASPECT).max(TILE_MIN_HEIGHT);

            // The shape gives way before the page does: a machine with many cores would
            // otherwise push the grid off the bottom of the window.
            let rows_gaps = gap as f32 * (lines.saturating_sub(1)) as f32;
            let room = (size.height - rows_gaps) / lines as f32;
            if room > 1.0 && room < tile {
                tile = room.max(1.0);
            }

            // Fill on both, because the children are FillPortion: a Shrink row resolves them
            // against their intrinsic width, and the first tile takes the row while the rest
            // lay out at zero.
            let mut grid = widget::column::with_capacity(lines)
                .spacing(gap)
                .width(Length::Fill);
            for line in 0..lines {
                let mut row = widget::row::with_capacity(columns)
                    .spacing(gap)
                    .width(Length::Fill);
                for slot in 0..columns {
                    let i = line * columns + slot;
                    row = match self.cores.get(i) {
                        Some(set) => row.push(
                            widget::container(self.plot(
                                Kind::Cpu,
                                set,
                                1 + Device::ALL.len() + i,
                                tile,
                                Style::Curve {
                                    cols: GRID_COLS,
                                    rows: GRID_ROWS,
                                },
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
        // Shrink, not the default Fill: the grid inside settles at the height the tile shape
        // asks for, and a Fill box around it leaves the difference as dead space under the
        // last row instead of closing up behind it.
        .height(Length::Shrink)
        .into()
    }

    /// The middle line of a rail row: what the device is.
    fn detail(&self, device: Device) -> String {
        let s = &self.last;
        match device {
            Device::Cpu => self.facts.model.clone(),
            Device::Memory => fl!(
                "summary-memory",
                used = size(s.mem[0]),
                total = size(s.mem_total)
            ),
            Device::Disk => fl!("summary-disk-read", value = rate(s.disk[0])),
            Device::Network => fl!("summary-net-rx", value = rate(s.net[0])),
        }
    }

    /// The bottom line of a rail row: what it is doing right now.
    fn reading(&self, device: Device) -> String {
        let s = &self.last;
        match device {
            Device::Cpu => percent(s.cpu.iter().sum()),
            Device::Memory => percent(s.mem[0] as f32 / s.mem_total.max(1) as f32),
            Device::Disk => fl!("summary-disk-write", value = rate(s.disk[1])),
            Device::Network => fl!("summary-net-tx", value = rate(s.net[1])),
        }
    }
}

/// One row of the facts panel's top group.
///
/// A free function and not a closure inside `facts_panel`: a closure taking an `Element` and
/// returning a `Row` ties the two lifetimes together through the closure's own signature, and
/// the borrow checker then refuses to let the row outlive the call.
fn group<'a>(cells: Vec<Element<'a, Message>>) -> widget::Row<'a, Message, cosmic::Theme> {
    let spacing = cosmic::theme::spacing();
    let mut row = widget::row::with_capacity(cells.len()).spacing(spacing.space_s);
    for cell in cells {
        row = row.push(cell);
    }
    row
}

fn last_seconds() -> String {
    let count = WINDOW as i64;
    fl!("last-seconds", count = count)
}

/// A share of the whole, as the reference writes it: whole per cent, no decimal.
fn percent(share: f32) -> String {
    format!("{:.0}%", share * 100.0)
}

fn gigahertz(khz: u64) -> String {
    fl!(
        "speed-ghz",
        value = format!("{:.2}", khz as f64 / 1_000_000.0)
    )
}

/// How long the machine has been up, as the reference writes it: `dd:hh:mm:ss`.
///
/// The day field is padded like the others. Unpadded it changed width on the tenth day and
/// took the whole line with it.
fn uptime() -> String {
    let secs = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    let (d, h, m, s) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    format!("{d:02}:{h:02}:{m:02}:{s:02}")
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

    /// Twenty logical processors go four across and five down. The transpose is what
    /// `ceil(sqrt(n))` gave, and measuring `ref/mission-center/cpu-dark.png` ruled it out.
    #[test]
    fn the_grid_is_never_wider_than_it_is_tall() {
        for (n, cols) in [(1, 1), (2, 1), (4, 2), (6, 2), (9, 3), (12, 3), (16, 4), (20, 4)] {
            assert_eq!(tile_columns(n), cols, "{n} processors");
            let rows = n.div_ceil(cols);
            assert!(rows >= cols, "{n} processors give {rows} rows of {cols} columns");
        }
    }

    /// Every tile is drawn, and no row is left with a hole in the middle of it.
    #[test]
    fn the_grid_holds_every_processor() {
        for n in 1..=64_usize {
            let cols = tile_columns(n);
            assert!(cols >= 1);
            assert!(cols * n.div_ceil(cols) >= n, "{n} processors do not fit");
        }
    }

    /// A tile keeps the reference's shape rather than stretching: 187 by 223.
    #[test]
    fn a_tile_is_shaped_like_the_reference() {
        let width = 223.0_f32;
        assert!((width * TILE_ASPECT - 187.0).abs() < 0.5);
        // And the same shape the other reference draws: 149 by 124.
        assert!((149.0 * TILE_ASPECT - 124.0).abs() < 2.0);
    }

    /// Only the two devices drawn against a fixed ceiling name one.
    #[test]
    fn a_scale_is_labelled_only_where_it_has_a_top() {
        assert!(Device::Cpu.scale_top().is_some());
        assert!(Device::Memory.scale_top().is_some());
        assert!(Device::Disk.scale_top().is_none());
        assert!(Device::Network.scale_top().is_none());
    }
}
