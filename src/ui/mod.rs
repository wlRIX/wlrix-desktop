// SPDX-License-Identifier: GPL-3.0-or-later
//! The desktop, as a Wayland client.
//!
//! One **wlr-layer-shell background surface**, anchored to all four edges of the chosen
//! output. That is the right place for desktop icons -- below every window, above nothing --
//! and it is the reason `wlrix-compositor` had to learn to route input to layer surfaces at
//! all: `wlrix-greeter` only ever put an *inert* backdrop on a layer, whereas here the
//! background is the interactive part.
//!
//! Two differences from the greeter's backdrop are load-bearing:
//!
//! - `set_exclusive_zone(0)`, not `-1`. Zero means "shrink me so I do not cover anyone's
//!   exclusive zone", which is what a desktop wants when a panel appears. The greeter uses
//!   `-1` because its backdrop must cover everything.
//! - `KeyboardInteractivity::OnDemand`, so clicking the desktop takes focus off whatever
//!   window had it -- which is what makes a click read as "I am talking to the desktop now".
//!
//! `calloop` drives everything: the Wayland connection and the inotify watch on the desktop
//! directory are both sources on one loop, so a file appearing and a pointer moving arrive the
//! same way and nothing polls.

pub mod canvas;
pub mod icons;
pub mod motif;
pub mod paint;

use std::path::PathBuf;
use std::time::Duration;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
    reexports::calloop_wayland_source::WaylandSource,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{
        wl_output::WlOutput, wl_pointer::WlPointer, wl_seat::WlSeat, wl_shm, wl_surface::WlSurface,
    },
};

use crate::config::Config;
use crate::entries::Entry;
use crate::layout::{Grid, Metrics, Placed, Point, Rect};
use crate::select::Selection;
use crate::state::State;
use crate::theme::font::Fonts;
use crate::watch::Watch;

/// `wl_pointer`'s left button, from `linux/input-event-codes.h`.
const BTN_LEFT: u32 = 0x110;

/// The whole desktop's state.
pub struct Desktop {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    compositor: CompositorState,
    layer_shell: LayerShell,
    pool: SlotPool,

    /// The background surface, once an output has been picked and configured.
    layer: Option<LayerSurface>,
    /// Which output it is on. Kept so an unplug can be told from any other output going.
    output: Option<WlOutput>,
    width: u32,
    height: u32,

    pointer: Option<WlPointer>,

    fonts: Fonts,
    directory: PathBuf,
    watch: Watch,
    config: Config,
    metrics: Metrics,
    saved: State,
    snap_to_grid: bool,

    entries: Vec<Entry>,
    placed: Vec<Placed>,
    selection: Selection,

    dirty: bool,
    exit: bool,
}

/// Run the desktop until the compositor goes away.
pub fn run() -> Result<(), String> {
    let config = Config::load();
    let metrics = config.metrics.resolve();
    let saved = State::load();
    let snap_to_grid = saved.snap_to_grid(config.snap_to_grid);

    let directory = crate::xdg::desktop_dir()
        .ok_or_else(|| "no home directory, so no desktop directory".to_string())?;
    let watch = Watch::new(&directory)
        .map_err(|err| format!("could not watch {}: {err}", directory.display()))?;

    let fonts = Fonts::load()?;
    eprintln!(
        "wlrix-desktop: watching {} ({}), labels in {} ({} faces), snap {}",
        directory.display(),
        if watch.is_watching_directory() {
            "present"
        } else {
            "not created yet"
        },
        fonts.family(),
        fonts.face_count(),
        if snap_to_grid { "on" } else { "off" },
    );

    let conn = Connection::connect_to_env()
        .map_err(|err| format!("no Wayland compositor to connect to: {err}"))?;
    let (globals, event_queue) =
        registry_queue_init(&conn).map_err(|err| format!("could not read the registry: {err}"))?;
    let qh = event_queue.handle();

    let mut event_loop: EventLoop<Desktop> =
        EventLoop::try_new().map_err(|err| format!("could not create the event loop: {err}"))?;
    let loop_handle = event_loop.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|err| format!("wl_compositor unavailable: {err}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|err| format!("wlr-layer-shell unavailable: {err}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|err| format!("wl_shm unavailable: {err}"))?;
    // Grown on demand when the real output size arrives; this is only a starting guess.
    let pool = SlotPool::new(1920 * 1080 * 4, &shm)
        .map_err(|err| format!("could not create a buffer pool: {err}"))?;

    // Duplicated before the watch moves into the struct: the loop source owns an fd of its
    // own, and the `Watch` keeps the one it reads from.
    let inotify_fd = watch
        .as_fd()
        .try_clone()
        .map_err(|err| format!("could not duplicate the inotify descriptor: {err}"))?;

    let mut desktop = Desktop {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        shm,
        compositor,
        layer_shell,
        pool,
        layer: None,
        output: None,
        width: 0,
        height: 0,
        pointer: None,
        fonts,
        directory,
        watch,
        config,
        metrics,
        saved,
        snap_to_grid,
        entries: Vec::new(),
        placed: Vec::new(),
        selection: Selection::default(),
        dirty: true,
        exit: false,
    };

    WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .map_err(|err| format!("could not drive Wayland from the loop: {err}"))?;

    // The desktop directory, on the same loop. Level-triggered, so `drain` must empty the fd
    // every time or this would spin.
    loop_handle
        .insert_source(
            Generic::new(inotify_fd, Interest::READ, Mode::Level),
            |_, _, desktop: &mut Desktop| {
                let directory = desktop.directory.clone();
                if desktop.watch.drain(&directory) {
                    desktop.rescan();
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|err| format!("could not watch the desktop directory: {err}"))?;

    desktop.rescan();

    // The surface needs an output, which arrives from the loop; one dispatch settles the
    // output list, and `new_output` covers anything that shows up later.
    let qh2 = qh.clone();
    event_loop
        .dispatch(Duration::from_millis(200), &mut desktop)
        .map_err(|err| format!("initial dispatch failed: {err}"))?;
    desktop.ensure_surface(&qh2);

    loop {
        event_loop
            .dispatch(Duration::from_secs(1), &mut desktop)
            .map_err(|err| format!("event loop failed: {err}"))?;
        // Painted after each batch rather than on a frame callback: the desktop redraws on
        // demand -- a hover, a click, a file appearing -- and a frame callback only arrives
        // after a commit that asked for one, so waiting on it stalls once nothing is moving.
        desktop.draw_if_dirty();
        // Only writes when something actually changed; see `State::is_dirty`.
        desktop.saved.save();
        if desktop.exit {
            return Ok(());
        }
    }
}

impl Desktop {
    /// The grid over the current surface.
    fn grid(&self) -> Grid {
        Grid::new(
            Rect::new(0, 0, self.width.max(1) as i32, self.height.max(1) as i32),
            self.metrics,
        )
    }

    /// Re-read the desktop directory and lay it out again.
    fn rescan(&mut self) {
        self.entries = crate::entries::read(&self.directory);
        let names: Vec<String> = self
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        // Anything that left the desktop should stop being highlighted, and stop taking up
        // room in the state file.
        self.selection.retain_only(&names);
        self.saved.retain_only(&names);
        self.relayout();
    }

    /// Recompute where every icon sits. Called whenever anything feeding it changes: the
    /// entry list, the surface size, or a drop.
    fn relayout(&mut self) {
        // Nothing to lay out against until the compositor has said how big the surface is.
        // `grid()` floors at 1x1, and laying out against that would pin every icon into a
        // single-cell column of nonsense -- and `State::arrange` would write it to disk.
        if self.width == 0 || self.height == 0 {
            return;
        }
        let grid = self.grid();
        self.placed = self.saved.arrange(&grid, &self.entries);
        self.dirty = true;
    }

    /// Which output the icons go on.
    ///
    /// Wayland has no notion of a primary output, so: the one named in the config, else the
    /// one at the origin of the compositor's coordinate space (which is what "leftmost" means
    /// in practice, and matches the compositor's own `space.outputs().next()`), else whatever
    /// came first.
    fn pick_output(&self) -> Option<WlOutput> {
        let outputs: Vec<WlOutput> = self.output_state.outputs().collect();
        if let Some(wanted) = self.config.output.as_deref() {
            let named = outputs.iter().find(|output| {
                self.output_state
                    .info(output)
                    .and_then(|info| info.name)
                    .is_some_and(|name| name == wanted)
            });
            if let Some(output) = named {
                return Some(output.clone());
            }
            // Named but absent: say so once rather than silently landing somewhere else.
            eprintln!("wlrix-desktop: no output named {wanted:?}; using the default");
        }
        outputs
            .iter()
            .find(|output| {
                self.output_state
                    .info(output)
                    .and_then(|info| info.logical_position)
                    .is_some_and(|(x, y)| x == 0 && y == 0)
            })
            .or_else(|| outputs.first())
            .cloned()
    }

    /// Create the background surface, once there is an output to put it on.
    fn ensure_surface(&mut self, qh: &QueueHandle<Self>) {
        if self.layer.is_some() {
            return;
        }
        let Some(output) = self.pick_output() else {
            return;
        };

        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Background,
            Some("wlrix-desktop"),
            Some(&output),
        );
        layer.set_anchor(Anchor::all());
        // 0, not -1: shrink to avoid a panel's exclusive zone rather than covering it.
        layer.set_exclusive_zone(0);
        // The desktop is clickable, so it must be focusable. `OnDemand` means the compositor
        // gives it the keyboard when it is clicked, and not before.
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        // (0, 0) with all four anchors: the compositor decides, and the configure says what.
        layer.set_size(0, 0);
        layer.commit();

        self.layer = Some(layer);
        self.output = Some(output);
    }

    /// Paint, if anything changed.
    fn draw_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.draw();
    }

    fn draw(&mut self) {
        let Some(layer) = self.layer.clone() else {
            return;
        };
        let (width, height) = (self.width.max(1), self.height.max(1));
        let stride = width as i32 * 4;

        let (buffer, pixels) = match self.pool.create_buffer(
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("wlrix-desktop: could not get a buffer to draw into: {err}");
                return;
            }
        };

        let mut canvas = canvas::Canvas::new(pixels, width as i32, height as i32);
        paint::desktop(
            &mut canvas,
            &mut self.fonts,
            &self.placed,
            &self.selection,
            self.metrics.icon,
        );

        let surface = layer.wl_surface();
        surface.damage_buffer(0, 0, width as i32, height as i32);
        if let Err(err) = buffer.attach_to(surface) {
            eprintln!("wlrix-desktop: could not attach the buffer: {err}");
            return;
        }
        surface.commit();
    }
}

impl CompositorHandler for Desktop {
    fn scale_factor_changed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new: i32,
    ) {
        // Everything is drawn at scale 1 for now; a HiDPI pass is separate work.
    }

    fn transform_changed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new: wayland_client::protocol::wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _surface: &WlSurface, _t: u32) {
        // Drawing is driven by the loop, not by frame callbacks; see `run`.
    }

    fn surface_enter(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }
}

impl OutputHandler for Desktop {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _c: &Connection, qh: &QueueHandle<Self>, _o: WlOutput) {
        // A desktop started before the compositor finished enumerating monitors still gets
        // a surface, as soon as the first one shows up.
        self.ensure_surface(qh);
    }

    fn update_output(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _o: WlOutput) {}

    fn output_destroyed(&mut self, _c: &Connection, qh: &QueueHandle<Self>, output: WlOutput) {
        if self.output.as_ref() != Some(&output) {
            return;
        }
        // The monitor the icons were on has gone. Drop the surface and take the next one, so
        // unplugging a screen moves the desktop rather than losing it.
        self.layer = None;
        self.output = None;
        self.width = 0;
        self.height = 0;
        self.ensure_surface(qh);
    }
}

impl LayerShellHandler for Desktop {
    fn closed(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _layer: &LayerSurface) {
        // The compositor took the surface away; there is nothing left to draw on.
        self.exit = true;
    }

    fn configure(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let (width, height) = configure.new_size;
        if (width, height) == (self.width, self.height) {
            // Still needs a first paint: the surface has no content until one is attached.
            self.dirty = true;
            return;
        }
        self.width = width;
        self.height = height;
        // The grid depends on the surface size, so a resized screen re-flows the icons --
        // remembered slots are honored where they still fit, which `arrange` handles.
        self.relayout();
    }
}

impl SeatHandler for Desktop {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _seat: WlSeat) {}

    fn new_capability(
        &mut self,
        _c: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointer = Some(pointer),
                Err(err) => eprintln!("wlrix-desktop: no pointer: {err}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
        }
    }

    fn remove_seat(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _seat: WlSeat) {}
}

impl PointerHandler for Desktop {
    fn pointer_frame(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        let Some(surface) = self.layer.as_ref().map(|layer| layer.wl_surface().clone()) else {
            return;
        };

        for event in events {
            if event.surface != surface {
                continue;
            }
            let point = Point::new(event.position.0 as i32, event.position.1 as i32);
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    // While a button is held this is a drag; otherwise a hover. Both are
                    // asked, and each says whether it changed anything worth redrawing.
                    let dragged = self.selection.motion(point);
                    let hovered = self.selection.hover(&self.placed, point);
                    self.dirty |= dragged || hovered;
                }
                PointerEventKind::Leave { .. } => {
                    self.dirty |= self.selection.leave();
                }
                PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                    self.dirty |= self.selection.press(&self.placed, point);
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    let grid = self.grid();
                    if let Some(dropped) = self.selection.release(&grid, self.snap_to_grid) {
                        self.saved.set_spot(&dropped.name, dropped.spot);
                        self.relayout();
                    }
                    self.dirty = true;
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for Desktop {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Desktop {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Desktop);
delegate_output!(Desktop);
delegate_shm!(Desktop);
delegate_seat!(Desktop);
delegate_pointer!(Desktop);
delegate_layer!(Desktop);
delegate_registry!(Desktop);
