// SPDX-License-Identifier: GPL-3.0-only

//! The load page: a rail of devices on the left, the chosen one drawn large on the right.
//!
//! Laid out after `ref/mission-center/cpu-dark.png`: heading and processor model on one line,
//! the time window and the top of the scale labelled above the plot, the tiles filling what
//! is left, and the facts panel down the right. Nothing is drawn under the tiles - the
//! per-core switch lives in the graph's context menu, as it does in the reference.

use std::collections::HashMap;
use std::time::Instant;

use cosmic::iced::widget::canvas;
use cosmic::iced::widget::text::Wrapping;
use cosmic::iced::{Alignment, Color, Length};
use cosmic::prelude::*;
use cosmic::widget;
use wmde_sysmon::graph::{Colors, Kind, Metrics, Ring, Strip, Style};
use wmde_sysmon::probe::{DeviceRate, Probe, Sample, Sources};

use crate::memory::Report;

use crate::devices::{Catalog, DiskKind, GpuName, LinkKind};
use crate::facts::{Facts, Live};
use crate::fl;
use crate::gpu::{Card, Gpus};

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
/// Characters a rail row's name has room for.
///
/// Measured on the running window rather than guessed: the text column of a row is 177 px -
/// from where the text starts to the divider - and the interface face averages 5.8 px a
/// character, which leaves about twenty-eight once the row's padding and the scrollbar's
/// spacing are taken off. The face is proportional, so this is the average and not a
/// guarantee; a name of wide letters is still cut by the widget, which is what the ellipsis
/// warns about.
const TITLE_CHARS: usize = 28;

/// Columns for `n` per-core tiles: a square-ish grid, never wider than it is tall.
///
/// Four logical processors give two by two, nine three by three, sixteen four by four. For a
/// count between two squares the reference is the tie-breaker: it lays twenty out in **four
/// columns by five rows**, so the floor of the root is taken and not the ceiling. The
/// transpose looked equally reasonable until the reference was measured.
fn tile_columns(n: usize) -> usize {
    n.isqrt().max(1)
}

/// One row of the rail, in the reference's order.
///
/// The processor and the memory are the machine; a disk and a card are one of however many
/// the machine has, and carry their place in the probe's list. That list is sorted by name
/// and only changes when hardware does, so the number is stable between rescans - and
/// [`valid`] catches the rescan where it is not. A graphics card carries its place in
/// [`Gpus`], which is read once and never changes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Device {
    #[default]
    Cpu,
    Memory,
    Disk(usize),
    Network(usize),
    Gpu(usize),
}

impl Device {
    const fn kind(self) -> Kind {
        match self {
            Self::Cpu => Kind::Cpu,
            Self::Memory => Kind::Memory,
            Self::Disk(_) => Kind::Disk,
            Self::Network(_) => Kind::Network,
            Self::Gpu(_) => Kind::Gpu,
        }
    }

    /// The top of the scale, where the scale has a top worth naming.
    ///
    /// Processor, memory and graphics card are drawn as a share of the whole and stop at a
    /// hundred per cent. Disk and network are drawn against whatever the busiest moment of
    /// the window was, so there is no fixed ceiling to label and none is invented.
    fn scale_top(self) -> Option<String> {
        matches!(self, Self::Cpu | Self::Memory | Self::Gpu(_)).then(|| percent(1.0))
    }
}

/// The rail for a machine with this many disks, network cards and graphics cards.
///
/// Graphics cards come last, where the reference puts them.
fn rail(disks: usize, nets: usize, gpus: usize) -> Vec<Device> {
    let mut out = Vec::with_capacity(2 + disks + nets + gpus);
    out.push(Device::Cpu);
    out.push(Device::Memory);
    out.extend((0..disks).map(Device::Disk));
    out.extend((0..nets).map(Device::Network));
    out.extend((0..gpus).map(Device::Gpu));
    out
}

/// Cuts a name that will not fit and marks the cut, so that a clipped name reads as clipped
/// and not as the whole of it.
///
/// The ellipsis is one character - the same one the rest of the stack writes - and it takes
/// the place of the last character kept, not a place of its own.
fn elide(text: &str, chars: usize) -> String {
    if chars == 0 || text.chars().count() <= chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(chars - 1).collect();
    format!("{}…", head.trim_end())
}

/// Keeps a selection pointing at something that still exists.
///
/// A stick pulled out or a cable unplugged shortens the list under the selection, and the
/// numbers of everything after it shift by one. Rather than follow a device that may not be
/// back, the page falls to the processor - the one row that is always there.
const fn valid(device: Device, disks: usize, nets: usize, gpus: usize) -> Device {
    match device {
        Device::Disk(i) if i >= disks => Device::Cpu,
        Device::Network(i) if i >= nets => Device::Cpu,
        Device::Gpu(i) if i >= gpus => Device::Cpu,
        other => other,
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

/// Pushes this tick onto one history per device, growing the set to match the probe.
///
/// A change of length clears them all: a device is addressed by its place in the list, and a
/// list that gained or lost one has moved everything after it. Hardware appearing is rare
/// enough that a minute of history is the right thing to spend on getting it right.
fn push_devices(sets: &mut Vec<Rings>, kind: Kind, meters: &[DeviceRate]) {
    if sets.len() != meters.len() {
        *sets = (0..meters.len()).map(|_| rings()).collect();
    }
    for (set, meter) in sets.iter_mut().zip(meters) {
        set[kind.index()].push(&[meter.rate[0] as f32, meter.rate[1] as f32]);
    }
}

/// One colour per device, taken from the theme's roles.
///
/// The panel applet colours a band by **what the load is** - user against system against
/// iowait - because in twenty pixels of height that is the only question worth answering.
/// A window has room to draw each device apart, and then the colour is free to mean the
/// device instead, which is what both the reference and every task manager before it does.
///
/// One `Colors` covers all four because the first band of each kind is a different band:
/// `CpuUser`, `MemUsed`, `NetIn`, `DiskRead`. Under [`Style::Curve`] the curve, its fill and
/// its grid all take the colour of that first band whatever else is stacked under it, so
/// nothing else needs overriding.
///
/// The colour is of the kind and not of the device: every disk is drawn green and every card
/// red, however many there are. The reference does the same, and a palette of four roles has
/// no second green to give the second drive anyway. The graphics card is the fifth kind and
/// has no role of its own; it takes `accent_purple`, the one named colour of the palette the
/// four roles leave free.
fn device_colors() -> Colors {
    let theme = cosmic::theme::active();
    let palette = theme.cosmic();
    let mut colors = Colors::default();
    for (kind, color) in [
        (Kind::Cpu, palette.accent_color()),
        (Kind::Memory, palette.warning_color()),
        (Kind::Disk, palette.success_color()),
        (Kind::Network, palette.destructive_color()),
        (Kind::Gpu, palette.palette.accent_purple),
    ] {
        colors.bands[kind.bands()[0] as usize] = Some(Color::from(color));
    }
    colors
}

pub struct Performance {
    probe: Probe,
    last: Sample,
    history: Rings,
    /// One set per logical processor, filled only while the per-core view is on.
    cores: Vec<Rings>,
    /// One set per disk and one per network card, in the probe's order.
    disks: Vec<Rings>,
    nets: Vec<Rings>,
    /// The graphics cards, found once, and a history for each in the same order.
    gpus: Gpus,
    gpu_rings: Vec<Rings>,
    /// What each card is called, read from `pci.ids` on the first tick.
    gpu_names: Vec<Option<GpuName>>,
    /// The rail as it stands: the two machine-wide rows, then a row per device.
    devices: Vec<Device>,
    /// What those disks and cards are. Read once each, when they first appear.
    catalog: Catalog,
    /// A cache per plot: the large one, then the rail thumbnails, then the core tiles.
    caches: Vec<canvas::Cache>,
    /// Rebuilt every tick: the palette moves when the theme does, and a colour taken once at
    /// start-up would keep the old theme's accent for the life of the window.
    colors: Colors,
    pub device: Device,
    pub per_core: bool,
    /// What the machine is, read once.
    facts: Facts,
    /// What the memory modules are, read once. `None` on a machine whose firmware lists
    /// none, and on one where the collector unit never ran.
    modules: Option<Report>,
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
            disks: Vec::new(),
            nets: Vec::new(),
            gpus: Gpus::new(),
            gpu_rings: Vec::new(),
            gpu_names: Vec::new(),
            // The two rows that are there before anything has been sampled. The first tick
            // adds the devices.
            devices: rail(0, 0, 0),
            catalog: Catalog::default(),
            caches: (0..=2).map(|_| canvas::Cache::new()).collect(),
            colors: device_colors(),
            device: Device::default(),
            // The reference opens on the tiles, and a machine busy on one core out of sixteen
            // reads as idle on the aggregate.
            per_core: true,
            facts: Facts::load(),
            modules: Report::load(),
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
            // The memory page prints available, committed and the slab; the strip in the
            // panel never does, which is why this is a flag and not always on.
            memory_detail: true,
            network: true,
            swap: true,
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
        // The machine-wide disk and network rings are left alone: the rail draws one row per
        // device, and nothing is left to draw their sum. The panel applet still does, out of
        // its own probe.
        self.catalog.refresh(
            self.probe.disks().iter().map(|meter| &*meter.name),
            self.probe.nets().iter().map(|meter| &*meter.name),
        );
        push_devices(&mut self.disks, Kind::Disk, self.probe.disks());
        push_devices(&mut self.nets, Kind::Network, self.probe.nets());

        self.gpus.sample(Instant::now());
        let cards = self.gpus.cards();
        if self.gpu_rings.len() != cards.len() {
            self.gpu_rings = (0..cards.len()).map(|_| rings()).collect();
            let devices: Vec<_> = cards.iter().map(|card| card.device.as_path()).collect();
            self.gpu_names = crate::devices::gpu_names(&devices);
        }
        // A card with nothing to report gets no column at all rather than a zero: an empty
        // graph says "unknown", a flat line would say "idle".
        for (set, card) in self.gpu_rings.iter_mut().zip(cards) {
            if let Some(busy) = card.busy {
                set[Kind::Gpu.index()].push(&[busy]);
            }
        }

        let devices = rail(self.disks.len(), self.nets.len(), self.gpu_rings.len());
        if devices != self.devices {
            self.devices = devices;
            self.device = valid(
                self.device,
                self.disks.len(),
                self.nets.len(),
                self.gpu_rings.len(),
            );
        }

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

        let needed = 1 + self.devices.len() + self.cores.len();
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

    /// A rail thumbnail, at the size the rail gives every one of them.
    ///
    /// Not `responsive` like the large plot, and that is not an optimization: a `responsive`
    /// inside a `scrollable` is laid out against an unbounded height, hands back a size the
    /// widget cannot use, and the mismatch takes the whole window's mouse handling with it -
    /// the rail highlights under the pointer and nothing anywhere answers a click. The
    /// thumbnail is eighty by fifty-four whatever the window does, so there is nothing to
    /// measure.
    fn thumb<'a>(&'a self, kind: Kind, history: &'a Rings, cache: usize) -> Element<'a, Message> {
        Strip::new(
            &Kind::ALL[kind.index()..=kind.index()],
            history,
            &self.colors,
            &self.caches[cache.min(self.caches.len() - 1)],
            Metrics {
                horizontal: true,
                length: THUMB_WIDTH,
                thickness: THUMB_HEIGHT,
                gap: 0.0,
            },
        )
        // No grid in a thumbnail: the reference draws none, and six by ten inside eighty by
        // fifty-four pixels would be a hatch rather than a grid.
        .style(Style::Curve { cols: 1, rows: 1 })
        .into()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();

        let titles = self.titles();
        let mut rail = widget::column::with_capacity(self.devices.len()).spacing(RAIL_GAP);
        for (i, device) in self.devices.iter().copied().enumerate() {
            let thumb = self.thumb(device.kind(), self.rings_of(device), 1 + i);
            let entry = widget::row::with_capacity(2)
                .spacing(spacing.space_xs)
                .align_y(Alignment::Center)
                .push(widget::container(thumb).width(Length::Fixed(THUMB_WIDTH)))
                .push(
                    // Three lines, as the reference has: what the device is called, what it
                    // is, and what it is doing. None of them wraps - a wrapped line makes one
                    // row taller than its neighbours and the rail stops reading as a list.
                    widget::column::with_capacity(3)
                        .push(widget::text::body(titles[i].clone()).wrapping(Wrapping::None))
                        .push(widget::text::caption(self.detail(device)).wrapping(Wrapping::None))
                        .push(widget::text::caption(self.reading(device)).wrapping(Wrapping::None)),
                );
            rail = rail.push(
                widget::button::custom(entry)
                    .width(Length::Fill)
                    .padding(spacing.space_xxs)
                    .selected(self.device == device)
                    .class(if self.device == device {
                        selected_row()
                    } else {
                        cosmic::theme::Button::ListItem([0.0; 4])
                    })
                    .on_press(Message::Select(device)),
            );
        }

        // Heading line: the device on the left, what it is on the right, against the far edge
        // of the plot. The processor model used to sit in the facts panel, where it cost a
        // row of a column that has rows to spare; a disk and a card put there what the rail
        // row has no width for - the drive's node and size, the card's full name.
        // The heading has the width the rail has not, so it carries the name whole.
        let mut heading = widget::row::with_capacity(3)
            .align_y(Alignment::Center)
            .push(widget::text::title3(self.title(self.device)))
            .push(widget::Space::new().width(Length::Fill));
        let aside = self.aside(self.device);
        if !aside.is_empty() {
            heading = heading.push(widget::text::body(aside));
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
                    self.rings_of(self.device),
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

        let mut main = widget::column::with_capacity(4)
            .spacing(spacing.space_xxs)
            .push(heading)
            .push(labels)
            .push(graph);
        if self.device == Device::Memory {
            main = main.push(self.memory_panel());
        }

        // The rail scrolls: a machine with six drives has eight rows at seventy-four pixels,
        // which is more than the window has. The spacing keeps the text out from under the
        // bar, and only while the bar is showing - the usual four rows keep the full width.
        let rail = rail
            .apply(widget::scrollable)
            .spacing(spacing.space_xxxs)
            .height(Length::Fill);

        let mut row = widget::row::with_capacity(4)
            .push(widget::container(rail).width(Length::Fixed(RAIL_WIDTH)))
            .push(widget::divider::vertical::default())
            .push(
                widget::container(main)
                    .width(Length::Fill)
                    // Fill, so `core_tiles` is handed the height the page actually has. It
                    // needs a real number: the tiles keep their shape until the shape would
                    // push the grid off the bottom, and that test is against this height.
                    .height(Length::Fill)
                    .padding(spacing.space_s),
            );
        let facts = match self.device {
            Device::Cpu => Some(self.facts_panel()),
            Device::Gpu(i) => self.gpus.cards().get(i).map(gpu_panel),
            _ => None,
        };
        if let Some(facts) = facts {
            row = row.push(
                widget::container(facts)
                    .width(Length::Fixed(FACTS_WIDTH))
                    .padding(spacing.space_s),
            );
        }
        row.into()
    }

    /// Under the memory plot: what the memory is doing on the left, what the modules are on
    /// the right, and one line per slot beneath them.
    ///
    /// The reference puts the same two blocks in the same place - `ref/taskman/memory\.png`.
    /// The hardware half is drawn only when the collector left a report; a machine whose
    /// firmware lists no memory devices, and every virtual machine, gets the left half alone.
    fn memory_panel(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let s = &self.last;
        let detail = s.mem_detail;

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

        let mut figures = widget::column::with_capacity(3).spacing(spacing.space_xxs);
        figures = figures.push(group(vec![
            pair(fl!("fact-mem-in-use"), size(s.mem[0])),
            pair(fl!("fact-mem-available"), size(detail.available)),
        ]));
        if detail.commit_limit > 0 {
            figures = figures.push(group(vec![
                pair(
                    fl!("fact-mem-committed"),
                    fl!(
                        "fact-mem-committed-value",
                        used = size(detail.committed),
                        limit = size(detail.commit_limit)
                    ),
                ),
                pair(fl!("fact-mem-cached"), size(s.mem[2])),
            ]));
        }
        if s.swap_total > 0 {
            figures = figures.push(group(vec![pair(
                fl!("fact-mem-swap"),
                fl!(
                    "fact-mem-committed-value",
                    used = size(s.swap_used),
                    limit = size(s.swap_total)
                ),
            )]));
        }

        let mut row = widget::row::with_capacity(2)
            .spacing(spacing.space_s)
            .push(figures)
            .push(widget::Space::new().width(Length::Fill));
        let mut column = widget::column::with_capacity(3).spacing(spacing.space_s);

        let Some(modules) = &self.modules else {
            return column.push(row).into();
        };

        // The hardware column: every row left out when the firmware has nothing to put in it.
        let mut hardware = widget::column::with_capacity(5).spacing(spacing.space_xxxs);
        if let Some((now, max)) = modules.speed() {
            hardware = hardware.push(line(
                fl!("fact-mem-speed"),
                match max {
                    Some(max) => fl!("fact-mem-speed-rated", now = now, max = max),
                    None => fl!("fact-mem-speed-value", value = now),
                },
            ));
        }
        hardware = hardware.push(line(
            fl!("fact-mem-slots"),
            fl!(
                "fact-mem-slots-value",
                used = modules.used(),
                total = modules.slots()
            ),
        ));
        if let Some(kind) = modules.kind() {
            hardware = hardware.push(line(fl!("fact-mem-kind"), kind));
        }
        if let Some(form) = modules.form_factor() {
            hardware = hardware.push(line(fl!("fact-mem-form-factor"), form));
        }
        if let Some(channels) = modules.channels() {
            hardware = hardware.push(line(fl!("fact-mem-channels"), channels.to_string()));
        }
        row = row.push(widget::container(hardware).width(Length::Fixed(FACTS_WIDTH)));
        column = column.push(row);

        // One line per populated slot. Empty slots are counted above and not listed: a row
        // that says only "DIMM_B2" carries nothing the slot count did not already say.
        let mut list = widget::column::with_capacity(modules.slots() + 1)
            .spacing(spacing.space_xxxs)
            .push(widget::text::heading(fl!("fact-mem-modules")));
        for module in modules.modules.iter().filter(|m| m.populated()) {
            let mut what = size(module.size);
            if !module.kind.is_empty() {
                what.push(' ');
                what.push_str(&module.kind);
                if let Some(speed) = module.speed_now {
                    what.push('-');
                    what.push_str(&speed.to_string());
                }
            }
            for extra in [&module.vendor, &module.part] {
                if !extra.is_empty() {
                    what.push_str(", ");
                    what.push_str(extra);
                }
            }
            list = list.push(line(module.locator.clone(), what));
        }
        column.push(list).into()
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
                                1 + self.devices.len() + i,
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
        .into()
    }

    /// The history a row draws. Falls back to the machine-wide set rather than panicking on
    /// a number the last tick invalidated - the next tick puts the selection right.
    fn rings_of(&self, device: Device) -> &Rings {
        match device {
            Device::Disk(i) => self.disks.get(i),
            Device::Network(i) => self.nets.get(i),
            Device::Gpu(i) => self.gpu_rings.get(i),
            _ => None,
        }
        .unwrap_or(&self.history)
    }

    /// The kernel's name for a device, which is the key to everything else about it.
    fn node(&self, device: Device) -> &str {
        match device {
            Device::Disk(i) => self.probe.disks().get(i).map(|meter| &*meter.name),
            Device::Network(i) => self.probe.nets().get(i).map(|meter| &*meter.name),
            Device::Gpu(i) => self.gpus.cards().get(i).map(|card| &*card.node),
            _ => None,
        }
        .unwrap_or("")
    }

    /// What `pci.ids` calls a graphics card, if it knows the card.
    fn gpu_name(&self, device: Device) -> Option<&GpuName> {
        match device {
            Device::Gpu(i) => self.gpu_names.get(i)?.as_ref(),
            _ => None,
        }
    }

    /// The top line of a rail row, and the heading of the page.
    ///
    /// A drive is called by its model, the way the reference calls it: a name off the box the
    /// user bought. A network card has no such name short enough for a rail row - `pci.ids`
    /// spells it out in fifty characters - so it goes by what it is, and the full name sits
    /// beside the heading where there is room for it. A graphics card does have one: the
    /// product in brackets, `GeForce GTX 1660`, with the chip it is built on beside it.
    fn title(&self, device: Device) -> String {
        match device {
            Device::Cpu => fl!("device-cpu"),
            Device::Memory => fl!("device-memory"),
            Device::Disk(_) => {
                let node = self.node(device);
                self.catalog
                    .disk(node)
                    .map(|disk| disk.model.clone())
                    .filter(|model| !model.is_empty())
                    .unwrap_or_else(|| node.to_owned())
            }
            // The shortened name, which is the model without the words every card in the
            // table shares. A card the table does not name, or names in shared words only,
            // goes by its link type - which is what the reference calls it anyway.
            Device::Network(_) => {
                let card = self.catalog.interface(self.node(device));
                match card
                    .map(|card| card.short.as_str())
                    .filter(|s| !s.is_empty())
                {
                    Some(short) => short.to_owned(),
                    None if card.is_some_and(|card| card.kind == LinkKind::Wireless) => {
                        fl!("device-net-wireless")
                    }
                    None => fl!("device-net-ethernet"),
                }
            }
            // The product, `NVIDIA GeForce GTX 1660`, and not the chip it is built on.
            Device::Gpu(_) => self
                .gpu_name(device)
                .map(|name| name.model.clone())
                .filter(|model| !model.is_empty())
                .unwrap_or_else(|| fl!("device-gpu")),
        }
    }

    /// The rail's names, cut to the width a row has and told apart where two came out alike.
    ///
    /// Two cards of one model, or two drives of one part number, would otherwise carry the
    /// same name twice and be distinguishable only by the line beneath. Names are built for
    /// the whole rail at once because that comparison needs all of them.
    fn titles(&self) -> Vec<String> {
        let base: Vec<String> = self.devices.iter().map(|d| self.title(*d)).collect();
        self.devices
            .iter()
            .enumerate()
            .map(|(i, device)| {
                let twin = self.devices.iter().enumerate().any(|(j, other)| {
                    j != i && other.kind() == device.kind() && base[j] == base[i]
                });
                let node = self.node(*device);
                if twin && !node.is_empty() {
                    // The node is what makes them two, so it is what the name has to keep.
                    let room = TITLE_CHARS.saturating_sub(node.chars().count() + 3);
                    fl!("device-node", what = elide(&base[i], room), node = node)
                } else {
                    elide(&base[i], TITLE_CHARS)
                }
            })
            .collect()
    }

    /// What sits against the right edge of the heading: the identity that did not fit in the
    /// rail. Empty for a device that has nothing more to say.
    fn aside(&self, device: Device) -> String {
        match device {
            Device::Cpu => self.facts.model.clone(),
            Device::Memory => String::new(),
            Device::Disk(_) => {
                let node = self.node(device);
                match self.catalog.disk(node).filter(|disk| disk.size > 0) {
                    Some(disk) => fl!("device-disk-node", node = node, size = size(disk.size)),
                    None => node.to_owned(),
                }
            }
            Device::Network(_) => self
                .catalog
                .interface(self.node(device))
                .map(|card| card.model.clone())
                .unwrap_or_default(),
            Device::Gpu(_) => self.detail(device),
        }
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
            // Everything the machine did not publish is left out rather than written as a
            // blank: a drive with no model reads as its node and its size, and a card with
            // no negotiated speed as its node alone.
            Device::Disk(_) => {
                let node = self.node(device);
                let mut what = Vec::with_capacity(2);
                if let Some(disk) = self.catalog.disk(node) {
                    if let Some(kind) = disk_kind(disk.kind) {
                        what.push(kind);
                    }
                    if disk.size > 0 {
                        what.push(size(disk.size));
                    }
                }
                match what.is_empty() {
                    true => node.to_owned(),
                    false => fl!("device-node", what = what.join(", "), node = node),
                }
            }
            Device::Network(_) => {
                let node = self.node(device);
                match self
                    .catalog
                    .interface(node)
                    .and_then(|card| card.speed_mbps)
                    .map(link_speed)
                {
                    Some(speed) => fl!("device-node", what = speed, node = node),
                    None => node.to_owned(),
                }
            }
            // The chip and the node, `TU116 (card0)`; the node alone for a card the table
            // names without a chip.
            Device::Gpu(_) => {
                let node = self.node(device);
                match self
                    .gpu_name(device)
                    .map(|name| name.chip.as_str())
                    .filter(|chip| !chip.is_empty())
                {
                    Some(chip) => fl!("device-node", what = chip, node = node),
                    None => node.to_owned(),
                }
            }
        }
    }

    /// The bottom line of a rail row: what it is doing right now.
    ///
    /// Both rates on one line, as the reference puts them - it writes `S: 0 R: 0 Kbps` in the
    /// width one figure would take spelled out.
    fn reading(&self, device: Device) -> String {
        let s = &self.last;
        match device {
            Device::Cpu => percent(s.cpu.iter().sum()),
            Device::Memory => percent(s.mem[0] as f32 / s.mem_total.max(1) as f32),
            Device::Disk(i) => {
                let meter = self.probe.disks().get(i);
                let lanes = meter.map_or([0.0; 2], |meter| meter.rate);
                fl!(
                    "summary-disk-rate",
                    read = rate(lanes[0]),
                    write = rate(lanes[1])
                )
            }
            Device::Network(i) => {
                let meter = self.probe.nets().get(i);
                let lanes = meter.map_or([0.0; 2], |meter| meter.rate);
                fl!("summary-net-rate", rx = rate(lanes[0]), tx = rate(lanes[1]))
            }
            Device::Gpu(i) => gpu_busy(self.gpus.cards().get(i)),
        }
    }
}

/// How busy a card is, or the dash for a card whose driver does not say.
fn gpu_busy(card: Option<&Card>) -> String {
    card.and_then(|card| card.busy)
        .map_or_else(|| fl!("no-value"), percent)
}

/// The facts panel of a graphics card: what it is doing, then what it is.
///
/// Utilization is always drawn - a dash for a card that publishes none, so the page says
/// "unknown" in the place the figure would be. Video memory is drawn only for a card that has
/// its own and reports it; an integrated card shares the machine's, which the memory page
/// already shows.
fn gpu_panel(card: &Card) -> Element<'_, Message> {
    let spacing = cosmic::theme::spacing();
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

    let mut top = vec![pair(fl!("fact-utilization"), gpu_busy(Some(card)))];
    if let Some((used, total)) = card.memory {
        top.push(pair(
            fl!("fact-gpu-memory"),
            fl!(
                "fact-mem-committed-value",
                used = size(used),
                limit = size(total)
            ),
        ));
    }

    let mut list = widget::column::with_capacity(2).spacing(spacing.space_xxxs);
    if !card.driver.is_empty() {
        list = list.push(line(fl!("fact-gpu-driver"), card.driver.to_string()));
    }
    list = list.push(line(fl!("fact-gpu-location"), card.address.to_string()));

    widget::column::with_capacity(2)
        .spacing(spacing.space_xxs)
        .push(group(top))
        .push(list)
        .into()
}

/// What to call a drive of this kind, or nothing when the machine would not say.
fn disk_kind(kind: DiskKind) -> Option<String> {
    match kind {
        DiskKind::Nvme => Some(fl!("device-disk-nvme")),
        DiskKind::Ssd => Some(fl!("device-disk-ssd")),
        DiskKind::Hdd => Some(fl!("device-disk-hdd")),
        DiskKind::Removable => Some(fl!("device-disk-removable")),
        DiskKind::Unknown => None,
    }
}

/// A negotiated link speed, in the unit it is usually spoken in: gigabits once it reaches a
/// thousand megabits, which is where every wired card of the last twenty years sits.
fn link_speed(mbps: u32) -> String {
    let gbit: u32 = mbps / 1000;
    if mbps >= 1000 && mbps.is_multiple_of(1000) {
        fl!("link-gbit", value = gbit)
    } else {
        fl!("link-mbit", value = mbps)
    }
}

/// A selected rail row: the grey of a selected list item, and neutral text with it.
///
/// `Button::ListItem` tints text and icon with the accent when selected. That is forbidden by
/// rule 3 of [.doc/lists](../../../.doc/lists/README.md), and neither reference does it - the
/// selected row of the reference's rail is the same white as every other row. Every state maps
/// to the one appearance, so hovering the current device does not flicker it back to the
/// unselected look. Same shape as `sidebar::selected_style` in wmde-files, at radius 0 rather
/// than 2: the file manager's rounding is one of the two exceptions the lists chapter records,
/// and this is not a third.
fn selected_row() -> cosmic::theme::Button {
    fn appearance(theme: &cosmic::Theme) -> widget::button::Style {
        let cosmic = theme.cosmic();
        let mut style = widget::button::Style::new();
        style.background =
            Some(Color::from(cosmic.primary(theme.transparent).component.hover).into());
        style.text_color = Some(Color::from(cosmic.on_bg_color()));
        style.icon_color = Some(Color::from(cosmic.on_bg_color()));
        style.border_radius = [0.0_f32; 4].into();
        style
    }
    cosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| appearance(theme)),
        disabled: Box::new(appearance),
        hovered: Box::new(|_focused, theme| appearance(theme)),
        pressed: Box::new(|_focused, theme| appearance(theme)),
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
        fl!(
            "size-mib",
            value = format!("{:.0}", value / (1024.0 * 1024.0))
        )
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
        for (n, cols) in [
            (1, 1),
            (2, 1),
            (4, 2),
            (6, 2),
            (9, 3),
            (12, 3),
            (16, 4),
            (20, 4),
        ] {
            assert_eq!(tile_columns(n), cols, "{n} processors");
            let rows = n.div_ceil(cols);
            assert!(
                rows >= cols,
                "{n} processors give {rows} rows of {cols} columns"
            );
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

    /// Only the devices drawn against a fixed ceiling name one.
    #[test]
    fn a_scale_is_labelled_only_where_it_has_a_top() {
        assert!(Device::Cpu.scale_top().is_some());
        assert!(Device::Memory.scale_top().is_some());
        assert!(Device::Gpu(0).scale_top().is_some());
        assert!(Device::Disk(0).scale_top().is_none());
        assert!(Device::Network(3).scale_top().is_none());
    }

    /// The two machine-wide rows first, then a row per device in the probe's order, and the
    /// graphics cards last.
    #[test]
    fn the_rail_is_the_machine_then_its_devices() {
        assert_eq!(rail(0, 0, 0), [Device::Cpu, Device::Memory]);
        assert_eq!(
            rail(2, 1, 1),
            [
                Device::Cpu,
                Device::Memory,
                Device::Disk(0),
                Device::Disk(1),
                Device::Network(0),
                Device::Gpu(0),
            ]
        );
    }

    /// A device that went away does not leave the page pointing past the end of the list.
    #[test]
    fn a_selection_never_outlives_its_device() {
        assert_eq!(valid(Device::Disk(1), 2, 1, 0), Device::Disk(1));
        assert_eq!(valid(Device::Disk(1), 1, 1, 0), Device::Cpu);
        assert_eq!(valid(Device::Network(0), 1, 0, 0), Device::Cpu);
        assert_eq!(valid(Device::Gpu(0), 0, 0, 1), Device::Gpu(0));
        assert_eq!(valid(Device::Gpu(1), 0, 0, 1), Device::Cpu);
        // The two that are always there are never touched, whatever is plugged in.
        assert_eq!(valid(Device::Memory, 0, 0, 0), Device::Memory);
    }

    /// Every device kind is drawn in a colour of its own, the graphics card included.
    #[test]
    fn every_device_kind_has_its_own_colour() {
        let colors = device_colors();
        let kinds = [
            Kind::Cpu,
            Kind::Memory,
            Kind::Disk,
            Kind::Network,
            Kind::Gpu,
        ];
        let picked: Vec<Color> = kinds
            .iter()
            .map(|kind| colors.bands[kind.bands()[0] as usize].expect("set per device"))
            .collect();
        for (i, a) in picked.iter().enumerate() {
            for b in &picked[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    /// A name that fits is left alone; one that does not ends in an ellipsis and never
    /// exceeds the room measured for it.
    #[test]
    fn a_name_too_long_for_the_rail_says_so() {
        assert_eq!(elide("Intel I226-V", TITLE_CHARS), "Intel I226-V");
        // Exactly the budget is not too long.
        let exact: String = "x".repeat(TITLE_CHARS);
        assert_eq!(elide(&exact, TITLE_CHARS), exact);

        let cut = elide("Intel 82540EM Gigabit Ethernet Controller", TITLE_CHARS);
        assert_eq!(cut.chars().count(), TITLE_CHARS);
        assert!(cut.ends_with('…'));
        // The cut lands inside the text, not against a space left dangling before the mark.
        assert!(!cut.contains(" …"));
    }

    /// A gigabit link is spoken of in gigabits, and anything else in what it negotiated.
    ///
    /// The unit itself is not asserted on: it is localized, and the catalogue the test runs
    /// against is whatever the machine's language is. The number is the same in every one.
    #[test]
    fn a_link_speed_is_read_in_the_unit_it_is_spoken_in() {
        assert!(
            !link_speed(1000).contains("1000"),
            "a whole gigabit is spoken in gigabits"
        );
        // 2.5 gigabit is not a whole number of them, so it stays in megabits.
        assert!(link_speed(2500).contains("2500"));
        assert!(link_speed(100).contains("100"));
    }
}
