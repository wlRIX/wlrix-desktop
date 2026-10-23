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

pub mod icons;
pub mod paint;

use std::path::PathBuf;
use std::time::Duration;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{
        DataDeviceManagerState, WritePipe,
        data_device::{DataDevice, DataDeviceData, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{DataSourceHandler, DragSource},
    },
    delegate_compositor, delegate_data_device, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
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
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    backend::WaylandError,
    globals::registry_queue_init,
    protocol::{
        wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
        wl_data_source::WlDataSource, wl_output::WlOutput, wl_pointer::WlPointer, wl_seat::WlSeat,
        wl_shm, wl_surface::WlSurface,
    },
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};

use crate::config::Config;
use crate::entries::Entry;
use crate::icon_theme::Icons;
use crate::layout::{Grid, Metrics, Placed, Point, Rect};
use crate::menu::{Action, Actions as MenuActions, Menu};
use crate::running::Running;
use crate::select::Selection;
use crate::state::State;
use crate::transfer::Payload;
use crate::watch::Watch;
use wlrix_ui::canvas::Canvas;
use wlrix_ui::palette::Palette;
use wlrix_ui::text::{Face, Fonts};

/// `wl_pointer`'s buttons, from `linux/input-event-codes.h`.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// How to move a surface relative to where it was, by the `wl_surface` version it has.
///
/// Before version 5 the move rides on `attach`'s x and y. From 5 on that is a protocol error
/// -- the compositor disconnects the client for it, which took the whole desktop down the first
/// time a drag image was offset -- and the move is its own request, `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IconPlacement {
    /// What to pass `wl_surface.offset`, when the surface has it.
    offset: Option<(i32, i32)>,
    /// What to pass `wl_surface.attach` as x and y.
    attach: (i32, i32),
}

impl IconPlacement {
    fn for_version(version: u32, x: i32, y: i32) -> Self {
        if version >= 5 {
            Self {
                offset: Some((x, y)),
                attach: (0, 0),
            }
        } else {
            Self {
                offset: None,
                attach: (x, y),
            }
        }
    }
}

/// A drag of desktop icons that the compositor is carrying, as a Wayland drag-and-drop.
///
/// Every icon drag becomes one as soon as it passes the threshold. The desktop is a surface
/// under every window, so it cannot tell by itself whether the pointer is over bare desktop or
/// over a window -- but the compositor can, and with a real drag it says so: the drag comes
/// back to the desktop's own surface when it is over bare desktop, and goes to the window when
/// it is over one. Dropped back on the desktop, the drag is the move it always was; dropped on
/// a window, it hands that application the files.
struct Outgoing {
    /// The offer. Dropping it cancels the drag, so it lives exactly as long as the drag does.
    source: DragSource,
    payload: Payload,
    /// The drag image's surface, which the compositor draws under the pointer.
    icon: WlSurface,
    /// Its buffer, kept so the image is not released while it is on screen.
    _image: Option<Buffer>,
    /// Whether the drag is over the desktop's own surface right now.
    over_desktop: bool,
    /// Whether it was dropped there -- as a move of the icons, already carried out.
    landed: bool,
}

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
    /// Whether the compositor has configured `layer` yet. Attaching a buffer before the first
    /// configure is acked is a protocol error, which kills the connection -- so a freshly
    /// created surface is not drawable until this turns true; see `draw_if_dirty`.
    configured: bool,
    width: u32,
    height: u32,

    pointer: Option<WlPointer>,
    /// The serial of the last button press, which is what a drag has to be started with.
    press_serial: u32,

    /// Drag-and-drop with other clients. Optional: without it, icons still move about the
    /// desktop the old way, they just cannot leave it.
    data_devices: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    /// The drag under way, while the compositor carries one of ours.
    outgoing: Option<Outgoing>,

    fonts: Fonts,
    directory: PathBuf,
    watch: Watch,
    /// The trash's `files` directory, for the Dumpster. `None` with no home directory, when
    /// there is no trash and no Dumpster either.
    trash_files: Option<PathBuf>,
    /// A watch on `trash_files`, so the Dumpster shows full the moment something is thrown
    /// away from anywhere -- Files, a shell, another desktop -- and empty once it is emptied.
    trash_watch: Option<Watch>,
    config: Config,
    /// The color scheme everything is drawn in, resolved from `config` at load and on every
    /// reload. `&'static`, because every scheme is baked into `wlrix-ui`.
    palette: &'static Palette,
    metrics: Metrics,
    saved: State,
    snap_to_grid: bool,

    /// Decoded icon artwork, cached across frames.
    images: Icons,
    /// Which applications have a window open, for the magic carpet's state.
    running: Running,

    entries: Vec<Entry>,
    placed: Vec<Placed>,
    selection: Selection,
    /// The right-click menu, while one is posted.
    menu: Option<Menu>,
    /// Programs started by a double-click, kept only so they can be reaped; see `reap`.
    children: Vec<std::process::Child>,

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

    // The Dumpster's directory and its watch. Neither is worth refusing to start over: without
    // them the desktop simply has no Dumpster.
    let trash_files = crate::trash::files_dir();
    let trash_watch = trash_files
        .as_deref()
        .and_then(|files| match Watch::new(files) {
            Ok(watch) => Some(watch),
            Err(err) => {
                eprintln!("wlrix-desktop: could not watch {}: {err}", files.display());
                None
            }
        });

    let fonts = Fonts::load()?;
    let mut icons = Icons::default();
    icons.set_theme(config.appearance.icon_theme());
    eprintln!(
        "wlrix-desktop: watching {} ({}), labels in {} ({} faces), icons from {}, snap {}",
        directory.display(),
        if watch.is_watching_directory() {
            "present"
        } else {
            "not created yet"
        },
        fonts.family(),
        fonts.face_count(),
        // Named in the log because "no icon found for ..." right underneath it is otherwise a
        // line with no next step: which theme was searched is the first thing to check.
        match config.appearance.icon_theme() {
            "" => "hicolor only",
            theme => theme,
        },
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
    // What lets an icon be dragged into another application. A compositor without it still
    // gets a desktop whose icons move about on it.
    let data_devices = match DataDeviceManagerState::bind(&globals, &qh) {
        Ok(state) => Some(state),
        Err(err) => {
            eprintln!(
                "wlrix-desktop: no wl_data_device_manager ({err}); icons cannot be dragged \
                 into other applications"
            );
            None
        }
    };

    // Duplicated before the watch moves into the struct: the loop source owns an fd of its
    // own, and the `Watch` keeps the one it reads from.
    let inotify_fd = watch
        .as_fd()
        .try_clone()
        .map_err(|err| format!("could not duplicate the inotify descriptor: {err}"))?;

    let trash_fd = trash_watch
        .as_ref()
        .map(|watch| watch.as_fd().try_clone())
        .transpose()
        .map_err(|err| format!("could not duplicate the trash watch descriptor: {err}"))?;

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
        configured: false,
        width: 0,
        height: 0,
        pointer: None,
        press_serial: 0,
        data_devices,
        data_device: None,
        outgoing: None,
        fonts,
        directory,
        watch,
        trash_files,
        trash_watch,
        palette: resolve_palette(&config),
        config,
        metrics,
        saved,
        snap_to_grid,
        images: icons,
        running: Running::default(),
        entries: Vec::new(),
        placed: Vec::new(),
        selection: Selection::default(),
        menu: None,
        children: Vec::new(),
        dirty: true,
        exit: false,
    };

    // The window list, for the magic carpet's running state. Optional: a compositor without it
    // still gets a working desktop, with every carpet closed.
    match desktop
        .registry_state
        .bind_one::<ExtForeignToplevelListV1, _, _>(&qh, 1..=1, ())
    {
        Ok(_list) => {
            // Nothing to hold: the compositor pushes toplevels at us, and the handles arrive
            // as children of the list object.
        }
        Err(err) => eprintln!(
            "wlrix-desktop: no ext-foreign-toplevel-list ({err}); \
             application icons will always show as not running"
        ),
    }

    // Kept for the loop below, to tell a compositor that has gone away from a real failure.
    let health = conn.clone();
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

    // The trash, the same way. Anything at all happening there may have changed whether the
    // Dumpster is full, and a rescan is how the icon finds out.
    if let Some(trash_fd) = trash_fd {
        loop_handle
            .insert_source(
                Generic::new(trash_fd, Interest::READ, Mode::Level),
                |_, _, desktop: &mut Desktop| {
                    let changed = match (desktop.trash_watch.as_mut(), desktop.trash_files.clone())
                    {
                        (Some(watch), Some(files)) => watch.drain(&files),
                        _ => false,
                    };
                    if changed {
                        desktop.rescan();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|err| format!("could not watch the trash: {err}"))?;
    }

    // Re-read `desktop.toml` on `SIGHUP`, so a settings change applies without restarting the
    // desktop. `wlrix-settings-daemon` finds this process through the pidfile below; `kill -HUP`
    // does the same thing by hand.
    let (reload_ping, reload_source) = calloop::ping::make_ping()
        .map_err(|err| format!("could not create the reload ping: {err}"))?;
    loop_handle
        .insert_source(reload_source, |_, _, desktop: &mut Desktop| {
            desktop.reload_config();
        })
        .map_err(|err| format!("could not watch for reloads: {err}"))?;
    crate::signals::forward_reload_to_loop(reload_ping);

    // Held until `run` returns, then its guard removes the file -- so a live pidfile means a
    // live desktop.
    let _pidfile = crate::pidfile::write();

    desktop.rescan();

    // The surface needs an output, which arrives from the loop; one dispatch settles the
    // output list, and `new_output` covers anything that shows up later.
    let qh2 = qh.clone();
    event_loop
        .dispatch(Duration::from_millis(200), &mut desktop)
        .map_err(|err| format!("initial dispatch failed: {err}"))?;
    desktop.ensure_surface(&qh2, None);

    loop {
        if let Err(err) = event_loop.dispatch(Duration::from_secs(1), &mut desktop) {
            // Asked first: a compositor that disconnects us for a protocol error also leaves the
            // socket closed, and without this the desktop's own bug would read as a logout and
            // vanish with exit status 0 and nothing in the log.
            if let Some(err) = health.protocol_error() {
                return Err(format!("protocol error: {err}"));
            }
            return match health.flush() {
                // The compositor going away is the ordinary end of a Wayland client's life,
                // not a failure: logging out from the desktop's own menu looks exactly like
                // this, and reporting it would put an error in the session log every single
                // time.
                Err(WaylandError::Io(_)) => Ok(()),
                // A connection that is still usable, or one we broke ourselves, means
                // something else went wrong -- which is worth saying.
                _ => Err(format!("event loop failed: {err}")),
            };
        }
        // A protocol error is this program's own bug, and it has to be caught here by hand:
        // `calloop-wayland-source` matches only `WaylandError::Io` on all three of its error
        // paths, and `EventQueue::dispatch_pending` drops the error outright, so the dispatch
        // above returns `Ok`. Meanwhile the socket stays readable forever -- the backend
        // reports its stored error without draining it -- so the loop would spin at 100% of a
        // core for the rest of the session, silently, with nothing on screen.
        if let Some(err) = health.protocol_error() {
            return Err(format!("protocol error: {err}"));
        }
        // Painted after each batch rather than on a frame callback: the desktop redraws on
        // demand -- a hover, a click, a file appearing -- and a frame callback only arrives
        // after a commit that asked for one, so waiting on it stalls once nothing is moving.
        desktop.draw_if_dirty();
        // Only writes when something actually changed; see `State::is_dirty`.
        desktop.saved.save();
        desktop.reap();
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
        // The desktop's own icons first, as IRIX laid them out: the home folder, then the
        // Dumpster, then the files. A desktop that has never been arranged places them in that
        // order; one that has keeps wherever they were put.
        let mut entries = Vec::new();
        if let Some(home) = crate::xdg::home() {
            entries.push(crate::entries::home(&home, &login_name(&home)));
        }
        if let Some(files) = &self.trash_files {
            entries.push(crate::entries::dumpster(
                files,
                crate::trash::is_full(files),
            ));
        }
        entries.extend(crate::entries::read(&self.directory));
        self.entries = entries;
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

    /// Re-read `desktop.toml` and apply what changed, on `SIGHUP`.
    ///
    /// What `wlrix-settings-daemon` sends after writing a setting, and what `kill -HUP` does by
    /// hand. A file that no longer parses keeps the running config rather than falling back to
    /// defaults: the daemon does not signal for a broken file at all, so getting here with one
    /// means somebody edited it themselves, and taking their whole desktop layout away while
    /// they are halfway through a sentence would be the wrong answer.
    ///
    /// `output` is deliberately not re-applied. Moving the icons to another monitor means
    /// tearing down the layer surface and building a new one against a different output, which
    /// is a restart's worth of work for a setting nobody changes twice.
    fn reload_config(&mut self) {
        let config = Config::load();
        self.metrics = config.metrics.resolve();
        // Only when it actually changed: a `SIGHUP` for an unrelated setting -- which is most
        // of them -- should not repaint, and comparing the resolved scheme rather than the
        // configured string means correcting a typo to the name of the scheme already showing
        // is correctly a no-op.
        let palette = resolve_palette(&config);
        if palette != self.palette {
            eprintln!("wlrix-desktop: palette is now {}", palette.id);
            self.palette = palette;
        }
        // The live snap setting is the user's, from the state file; the config only ever
        // supplied its starting value, so a reload must not overrule what they chose.
        if config.output.as_deref() != self.config.output.as_deref() {
            eprintln!("wlrix-desktop: [output] changed; that only takes effect on a restart");
        }
        self.config = config;
        // Before the clear below, so a theme that has not changed does not clear twice.
        self.images.set_theme(self.config.appearance.icon_theme());
        // The image cache holds *un-tinted* decodes and the tint is applied per draw, so a
        // palette change does not invalidate it. This clear is for the icon size; `set_theme`
        // has already covered a theme change.
        self.images.clear();
        self.relayout();
        eprintln!("wlrix-desktop: reloaded desktop.toml");
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
    ///
    /// `going` is an output that is on its way out and must not be chosen. `OutputState` drops
    /// a destroyed output only *after* `output_destroyed` returns, so without this the monitor
    /// that just went away is still in the list -- and picking it puts the surface on an output
    /// the compositor has already unmapped, where no configure will ever arrive.
    fn pick_output(&self, going: Option<&WlOutput>) -> Option<WlOutput> {
        let outputs: Vec<WlOutput> = self
            .output_state
            .outputs()
            .filter(|output| Some(output) != going)
            .collect();
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
    ///
    /// `going` is passed straight to [`Self::pick_output`]; see there.
    fn ensure_surface(&mut self, qh: &QueueHandle<Self>, going: Option<&WlOutput>) {
        if self.layer.is_some() {
            return;
        }
        let Some(output) = self.pick_output(going) else {
            return;
        };

        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            // `Bottom`, not `Background`: the background layer belongs to the wallpaper
            // (`wlrix-bg`), and two clients sharing a layer have no defined order between them
            // -- restarting the wallpaper would draw it over the icons. Bottom is still below
            // every window, which is all the desktop needs.
            Layer::Bottom,
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
        // Nothing may be attached until the compositor answers that commit.
        self.configured = false;
    }

    /// Open the icon called `name`: a launcher runs, anything else goes to the opener.
    ///
    /// A refusal is reported rather than swallowed. There is no way to put a dialog on screen
    /// yet, so the session log is the only place it can go -- and a double-click that silently
    /// does nothing is the worst of both worlds.
    fn activate(&mut self, name: &str) {
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .cloned()
        else {
            return;
        };
        match crate::open::open(&entry, &self.config) {
            Ok(child) => {
                eprintln!("wlrix-desktop: opened {} (pid {})", entry.name, child.id());
                self.children.push(child);
                // IRIX's two double-click sounds: a folder opening, and everything else, which
                // starts a program -- the launcher's, or the one a document opens in.
                crate::sound::play(match entry.kind {
                    crate::entries::Kind::Directory | crate::entries::Kind::Dumpster => {
                        "wlrix-open-folder"
                    }
                    _ => "wlrix-launch",
                });
            }
            Err(why) => {
                eprintln!("wlrix-desktop: {why}");
                crate::sound::play("bell");
            }
        }
    }

    /// Post the desktop menu at `at`, clamped so the whole panel is reachable.
    ///
    /// What each row can do is decided here, once, from the selection as it stands -- so a
    /// row that looks disabled cannot act, and one that looks live cannot fail.
    fn open_menu(&mut self, at: Point) {
        let selected = self.selection.selected().len();
        let launcher = self.lone_launcher();
        // Empty Dumpster, when the Dumpster is all that is selected; enabled while it is full.
        let dumpster = match self.selection.selected() {
            [name] if name == crate::entries::DUMPSTER => {
                self.trash_files.as_deref().map(crate::trash::is_full)
            }
            _ => None,
        };
        let area = Rect::new(0, 0, self.width.max(1) as i32, self.height.max(1) as i32);

        // How wide a label is belongs to the font, and the menu knows nothing about fonts; it
        // asks through here. `&mut` because `Fonts` caches its shaping as it measures.
        let fonts = &mut self.fonts;
        let mut measure = |label: &str| fonts.width(Face::Bold, crate::menu::LABEL_PX, label);
        let actions = launcher
            .as_ref()
            .map(|(name, items)| MenuActions { entry: name, items });

        let size = Menu::size_with_dumpster(selected, actions.clone(), dumpster, &mut measure);
        let origin = Menu::clamp(at, size, area);
        let menu = Menu::with_dumpster(origin, selected, actions, dumpster, &mut measure);

        self.menu = Some(menu);
        self.dirty = true;
    }

    /// The selected launcher's actions, when exactly one launcher is selected and it has any.
    ///
    /// `None` for anything else: an action belongs to one file, so "Store" applied to three
    /// selected icons would mean nothing.
    fn lone_launcher(&self) -> Option<(String, Vec<crate::entries::LauncherAction>)> {
        let [name] = self.selection.selected() else {
            return None;
        };
        let entry = self.entries.iter().find(|entry| &entry.name == name)?;
        let launcher = entry.launcher.as_ref()?;
        (!launcher.actions.is_empty()).then(|| (entry.name.clone(), launcher.actions.clone()))
    }

    /// Run one of a launcher's `[Desktop Action …]` groups.
    ///
    /// The file is read again rather than run from what the menu was built with. The menu can
    /// sit open for as long as the user likes, and a launcher edited or replaced underneath it
    /// should run what it says now or not at all.
    fn run_action(&mut self, name: &str, id: &str) {
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .cloned()
        else {
            return;
        };
        match crate::open::action(&entry, id, &self.config) {
            Ok(child) => {
                eprintln!(
                    "wlrix-desktop: ran {} action {id:?} (pid {})",
                    entry.name,
                    child.id()
                );
                self.children.push(child);
            }
            Err(why) => eprintln!("wlrix-desktop: {why}"),
        }
    }

    /// Carry out a menu choice. `menu` is the one it was chosen from, for [`Action::Run`].
    fn perform(&mut self, action: Action, menu: &Menu) {
        match action {
            Action::LogOut => self.log_out(),
            // Open every selected item, exactly as double-clicking each would.
            Action::Open => {
                for name in self.selection.selected().to_vec() {
                    self.activate(&name);
                }
            }
            Action::Remove => self.remove_selected(),
            Action::EmptyDumpster => self.empty_dumpster(),
            Action::SelectAll => {
                let names: Vec<String> = self
                    .entries
                    .iter()
                    .map(|entry| entry.name.clone())
                    .collect();
                self.selection.select_all(names);
                self.dirty = true;
            }
            Action::Run(index) => {
                // Copied out before touching `self`: the menu is borrowed for the length of
                // this call and `run_action` needs the whole of it mutably.
                if let Some((name, id)) = menu.action_target(index) {
                    let (name, id) = (name.to_owned(), id.to_owned());
                    self.run_action(&name, &id);
                }
            }
            // Drawn but disabled, so `action_at` never hands these back. Matched explicitly
            // rather than with a wildcard, so adding an action cannot silently do nothing.
            Action::MakeCopy
            | Action::MakeReference
            | Action::ChangePermissions
            | Action::AddNewDirectory => {}
        }
    }

    /// End the session and return to the greeter.
    ///
    /// The compositor *is* the session -- `wlrix-session` starts it and exits when it does --
    /// so ending the session means asking the compositor to stop. It drops its pid in a
    /// well-known file for exactly this kind of thing; the settings apps use the same file to
    /// send it a `SIGHUP`.
    ///
    /// There is no "really log out?" here because there is nowhere to ask: the desktop cannot
    /// put a dialog on screen yet. Choosing the item is taken as meaning it.
    fn log_out(&mut self) {
        match crate::session::log_out() {
            Ok(pid) => eprintln!("wlrix-desktop: asked the compositor (pid {pid}) to stop"),
            Err(why) => eprintln!("wlrix-desktop: could not log out: {why}"),
        }
    }

    /// Move every selected item to the trash.
    ///
    /// Trashed, not deleted: there is no confirmation and no undo, so a misclick on a
    /// band-selected group must be recoverable. See [`crate::trash`].
    fn remove_selected(&mut self) {
        let selected = self.selection.selected().to_vec();
        self.throw_away(&selected, "file-trash");
    }

    /// Empty Dumpster: ask, then delete everything in the trash for good.
    ///
    /// Handed to `wlrix-files --empty-trash`, which asks the question -- the desktop cannot put
    /// a dialog on screen -- and does the emptying. A running file manager asks over its own
    /// window; otherwise that command shows the question alone and exits. The watch on the
    /// trash turns the icon back to empty when it is done.
    fn empty_dumpster(&mut self) {
        match std::process::Command::new("wlrix-files")
            .arg("--empty-trash")
            .spawn()
        {
            Ok(child) => self.children.push(child),
            Err(err) => {
                eprintln!("wlrix-desktop: could not run wlrix-files to empty the Dumpster: {err}");
                crate::sound::play("bell");
            }
        }
    }

    /// Move the icons called `names` to the trash, playing `sound` if any went.
    ///
    /// The desktop's own icons are skipped. The Dumpster is not a file, and throwing the trash
    /// into the trash is not something anyone means; the home folder is the user's whole home
    /// directory, and no click on the desktop should be able to throw that away.
    fn throw_away(&mut self, names: &[String], sound: &str) {
        let mut removed = 0;
        for name in names {
            if crate::entries::is_builtin(name) {
                continue;
            }
            let Some(entry) = self.entries.iter().find(|entry| &entry.name == name) else {
                continue;
            };
            match crate::trash::trash(&entry.path) {
                Ok(()) => removed += 1,
                Err(why) => eprintln!("wlrix-desktop: {why}"),
            }
        }
        if removed > 0 {
            eprintln!("wlrix-desktop: moved {removed} item(s) to the trash");
            crate::sound::play(sound);
            // The watch will notice too, but rescanning now means the icons go immediately
            // rather than after the inotify round trip.
            self.rescan();
        }
    }

    /// Clear away children that have exited.
    ///
    /// Nothing waits on a launched program -- it outlives the double-click by design -- so
    /// without this each one left a zombie behind for the life of the session. `try_wait` does
    /// not block, so this is safe to call every turn of the loop.
    fn reap(&mut self) {
        self.children.retain_mut(|child| match child.try_wait() {
            // Still running: keep it.
            Ok(None) => true,
            Ok(Some(status)) => {
                // A launcher that dies immediately is worth a line; one that ran and finished
                // normally is not.
                if !status.success() {
                    eprintln!("wlrix-desktop: a launched program exited with {status}");
                }
                false
            }
            // Already reaped, or not ours any more. Either way, stop tracking it.
            Err(_) => false,
        });
    }

    /// Drop the dragged icons at `point` on the desktop: onto the Dumpster, which throws them
    /// away, or anywhere else, which moves them there.
    fn drop_icons(&mut self, point: Point) {
        // Asked before the release, which ends the drag: is the pointer over the Dumpster, with
        // icons that are not the Dumpster being dragged onto it.
        let into_dumpster = self.selection.is_dragging_any()
            && !self.selection.is_dragging(crate::entries::DUMPSTER)
            && self.placed.iter().any(|item| {
                item.entry.kind == crate::entries::Kind::Dumpster && item.rect.contains(point)
            });
        let grid = self.grid();
        // A drop can move several icons at once, when a rubber band selected them and one of
        // them was then dragged.
        let dropped = self
            .selection
            .release(&self.placed, &grid, self.snap_to_grid);
        if into_dumpster && !dropped.is_empty() {
            // Thrown away rather than moved: they keep no position, since they are no longer
            // on the desktop.
            let names: Vec<String> = dropped.into_iter().map(|drop| drop.name).collect();
            self.throw_away(&names, "wlrix-drag-to-trash");
        } else if !dropped.is_empty() {
            for drop in dropped {
                self.saved.set_spot(&drop.name, drop.spot);
            }
            self.relayout();
            crate::sound::play("wlrix-icon-shift");
        }
        self.dirty = true;
    }

    /// Hand the drag under way to the compositor, as a Wayland drag-and-drop. See [`Outgoing`].
    ///
    /// Without a data device -- a compositor that has none -- this does nothing, and the drag
    /// carries on as a move on the desktop alone.
    fn start_drag(&mut self, qh: &QueueHandle<Self>) {
        let (Some(manager), Some(device), Some(layer)) = (
            self.data_devices.as_ref(),
            self.data_device.as_ref(),
            self.layer.as_ref(),
        ) else {
            return;
        };

        // The files to hand over. The desktop's own icons travel as nothing; see
        // `crate::transfer`.
        let paths = self
            .selection
            .dragged()
            .iter()
            .filter(|name| !crate::entries::is_builtin(name))
            .filter_map(|name| self.entries.iter().find(|entry| &entry.name == name))
            .map(|entry| entry.path.clone())
            .collect();
        let payload = Payload::new(paths);

        // Copy or move, the target's choice -- the file manager moves unless told otherwise,
        // the icon catalog only ever copies, since it keeps a reference and nothing else.
        let source = manager.create_drag_and_drop_source(
            qh,
            payload.mime_types(),
            DndAction::Copy | DndAction::Move,
        );
        let icon = self.compositor.create_surface(qh);
        source.start_drag(device, layer.wl_surface(), Some(&icon), self.press_serial);
        // Drawn after the request, which is when the surface takes its role.
        let image = self.draw_drag_image(&icon);

        self.selection.carry();
        self.outgoing = Some(Outgoing {
            source,
            payload,
            icon,
            _image: image,
            over_desktop: true,
            landed: false,
        });
        self.dirty = true;
    }

    /// Draw the dragged icons onto `icon`, placed so the point that was grabbed sits under the
    /// pointer.
    fn draw_drag_image(&mut self, icon: &WlSurface) -> Option<Buffer> {
        let bounds = paint::drag_bounds(&self.placed, &self.selection)?;
        let grab = self.selection.drag_origin()?;
        let (buffer, pixels) = self
            .pool
            .create_buffer(bounds.w, bounds.h, bounds.w * 4, wl_shm::Format::Argb8888)
            .map_err(|err| eprintln!("wlrix-desktop: could not get a buffer for the drag: {err}"))
            .ok()?;

        let mut canvas = Canvas::new(pixels, bounds.w, bounds.h);
        paint::drag_image(
            &mut canvas,
            &mut paint::Frame {
                palette: self.palette,
                fonts: &mut self.fonts,
                images: &mut self.images,
                placed: &self.placed,
                selection: &self.selection,
                running: &self.running,
                menu: None,
                icon_size: self.metrics.icon,
            },
            bounds,
        );

        // The image's corner goes up and left of the pointer by however far into the icons the
        // press landed.
        let placement =
            IconPlacement::for_version(icon.version(), bounds.x - grab.x, bounds.y - grab.y);
        if let Some((x, y)) = placement.offset {
            icon.offset(x, y);
        }
        let (x, y) = placement.attach;
        icon.attach(Some(buffer.wl_buffer()), x, y);
        icon.damage_buffer(0, 0, bounds.w, bounds.h);
        icon.commit();
        Some(buffer)
    }

    /// The drag is over, one way or another. Unless it came back and was dropped on the
    /// desktop, nothing here moves: either another application took the files -- and moving
    /// them is the file system's business, which the watch notices -- or nobody did.
    fn end_drag(&mut self, source: &WlDataSource) {
        if self
            .outgoing
            .as_ref()
            .is_none_or(|outgoing| outgoing.source.inner() != source)
        {
            return;
        }
        let Some(outgoing) = self.outgoing.take() else {
            return;
        };
        outgoing.icon.destroy();
        if !outgoing.landed {
            self.selection.cancel_drag();
        }
        self.dirty = true;
    }

    /// Whether `surface` is the desktop's own.
    fn is_desktop(&self, surface: &WlSurface) -> bool {
        self.layer
            .as_ref()
            .is_some_and(|layer| layer.wl_surface() == surface)
    }

    /// Paint, if anything changed.
    fn draw_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        // There is nothing to attach to until the compositor has configured the surface, and
        // attaching anyway is a protocol error that takes the whole connection down with it.
        // `dirty` deliberately stays set, so the paint happens from `configure` instead of
        // being dropped on the floor.
        if !self.configured {
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

        let mut canvas = Canvas::new(pixels, width as i32, height as i32);
        paint::desktop(
            &mut canvas,
            &mut paint::Frame {
                palette: self.palette,
                fonts: &mut self.fonts,
                images: &mut self.images,
                placed: &self.placed,
                selection: &self.selection,
                running: &self.running,
                menu: self.menu.as_ref(),
                icon_size: self.metrics.icon,
            },
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

// --- ext-foreign-toplevel-list-v1 -------------------------------------------------------
//
// Not something smithay-client-toolkit wraps, so the two `Dispatch` impls are written out.
// The desktop only reads this list: it wants app ids, to decide which launchers have a window
// open and so which magic carpets stand open.

impl Dispatch<ExtForeignToplevelListV1, ()> for Desktop {
    fn event(
        desktop: &mut Self,
        _list: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Finished = event {
            // The compositor has stopped listing windows. Nothing is known to be running any
            // more, so every carpet closes rather than freezing as it was.
            desktop.running = Running::default();
            desktop.dirty = true;
        }
    }

    // The `toplevel` event carries a new object the compositor created for us.
    wayland_client::event_created_child!(Desktop, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for Desktop {
    fn event(
        desktop: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // The protocol object's own id, which is unique per window for as long as it exists.
        let id = handle.id().protocol_id();
        match event {
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                desktop.running.window(id, &app_id);
                desktop.dirty = true;
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                desktop.running.closed(id);
                desktop.dirty = true;
                handle.destroy();
            }
            _ => {}
        }
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
        // a surface, as soon as the first one shows up. This is also the way back from every
        // monitor going at once: a DisplayPort screen entering power save drops the link, so
        // the compositor really does destroy the outputs and advertise them again on wake.
        self.ensure_surface(qh, None);
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
        self.configured = false;
        self.width = 0;
        self.height = 0;
        // Leaving `layer` set to a surface on the departed output would be the end of the
        // desktop: `ensure_surface` returns early when there is one, so the monitors coming
        // back would never build a new surface, and the icons would never reappear.
        self.ensure_surface(qh, Some(&output));
    }
}

impl LayerShellHandler for Desktop {
    fn closed(&mut self, _c: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface) {
        // Not the end of the desktop. The compositor closes every layer surface on an output it
        // is removing, and an output being removed is what a DisplayPort monitor entering power
        // save looks like -- so exiting here would mean the icons never came back from an idle
        // blank. Take another output if one is left; if none is, `new_output` picks the desktop
        // back up when the monitors return.
        if self.layer.as_ref().map(WaylandSurface::wl_surface) != Some(layer.wl_surface()) {
            // A surface already replaced, closed on its way out. Nothing to do: acting on it
            // would tear down the *live* one.
            return;
        }
        // Excluded from the next choice for the same reason as in `output_destroyed`: the
        // output this surface was on is on its way out, and its global may not have been
        // withdrawn yet.
        let going = self.output.take();
        self.layer = None;
        self.configured = false;
        self.width = 0;
        self.height = 0;
        self.ensure_surface(qh, going.as_ref());
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
        // `sctk` acked this on the way in, so the surface is drawable from here on.
        self.configured = true;
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            // The grid depends on the surface size, so a resized screen re-flows the icons --
            // remembered slots are honored where they still fit, which `arrange` handles.
            self.relayout();
        }
        // Either way it still needs a paint: the surface has no content until one is attached,
        // and this is where a paint held back for want of a configure finally gets to run.
        self.dirty = true;
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
        // The seat's drag-and-drop half, which a drag off the desktop is started on.
        if capability == Capability::Pointer
            && self.data_device.is_none()
            && let Some(manager) = self.data_devices.as_ref()
        {
            self.data_device = Some(manager.get_data_device(qh, &seat));
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
        qh: &QueueHandle<Self>,
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
                    // An open menu owns the pointer: it is drawn over the desktop, so
                    // hovering an icon underneath would highlight something the user cannot
                    // reach without putting the menu away first.
                    if let Some(menu) = self.menu.as_mut() {
                        self.dirty |= menu.hover(point);
                        continue;
                    }
                    // While a button is held this is a drag or a rubber band; otherwise a
                    // hover. Both are asked, and each says whether anything worth redrawing
                    // changed.
                    let dragged = self.selection.motion(&self.placed, point);
                    let hovered = self.selection.hover(&self.placed, point);
                    self.dirty |= dragged || hovered;
                    // Past the threshold, the drag is handed to the compositor, so it can
                    // leave the desktop for another application.
                    if self.selection.is_dragging_any() && !self.selection.is_carried() {
                        self.start_drag(qh);
                    }
                }
                PointerEventKind::Leave { .. } => {
                    self.dirty |= self.selection.leave();
                }
                // A right click posts the menu; a second one moves it.
                PointerEventKind::Press { button, .. } if button == BTN_RIGHT => {
                    self.open_menu(point);
                }
                PointerEventKind::Press {
                    button,
                    time,
                    serial,
                } if button == BTN_LEFT => {
                    // A drag can only be started with the serial of the press that began it.
                    self.press_serial = serial;
                    // With a menu open, a press either chooses a row or puts the menu away.
                    // Either way it does not reach the desktop: clicking "Remove" should not
                    // also start a rubber band underneath it.
                    if let Some(menu) = self.menu.take() {
                        self.dirty = true;
                        if let Some(action) = menu.action_at(point) {
                            self.perform(action, &menu);
                        }
                        continue;
                    }
                    let pressed = self.selection.press(&self.placed, point, time);
                    self.dirty |= pressed.changed;
                    // A double-click opens what it landed on.
                    if let Some(name) = pressed.activate {
                        self.activate(&name);
                    }
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    // A Wayland drag takes the pointer, so a release still arriving here means
                    // the compositor never started the one asked for. Let it go, and finish the
                    // drag the old way, on the desktop.
                    if let Some(outgoing) = self.outgoing.take() {
                        outgoing.icon.destroy();
                    }
                    self.drop_icons(point);
                }
                _ => {}
            }
        }
    }
}

// --- drag-and-drop ----------------------------------------------------------------------
//
// The desktop is a drop target only for its own drags, coming back to it: that is a move of the
// icons. Another application's drag is turned down -- dropping files onto the desktop to copy
// them there is still to come.

/// The drag currently over one of our surfaces, if any.
fn drag_offer(device: &WlDataDevice) -> Option<DragOffer> {
    device.data::<DataDeviceData>()?.drag_offer()
}

impl DataDeviceHandler for Desktop {
    fn enter(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        let Some(offer) = drag_offer(device) else {
            return;
        };
        let ours = self.is_desktop(surface)
            && self.outgoing.is_some()
            && offer.with_mime_types(|types| types.iter().any(|t| t == crate::transfer::INTERNAL));
        if !ours {
            offer.accept_mime_type(offer.serial, None);
            offer.set_actions(DndAction::empty(), DndAction::empty());
            return;
        }

        // Back over bare desktop: a move, wherever it lands.
        offer.accept_mime_type(offer.serial, Some(crate::transfer::INTERNAL.to_owned()));
        offer.set_actions(DndAction::Move, DndAction::Move);
        if let Some(outgoing) = self.outgoing.as_mut() {
            outgoing.over_desktop = true;
        }
        self.dirty |= self
            .selection
            .motion(&self.placed, Point::new(x as i32, y as i32));
    }

    fn leave(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _device: &WlDataDevice) {
        if let Some(outgoing) = self.outgoing.as_mut()
            && outgoing.over_desktop
        {
            outgoing.over_desktop = false;
            // Over a window now: nothing on the desktop is a drop target.
            self.dirty |= self.selection.drag_left();
        }
    }

    fn motion(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _device: &WlDataDevice,
        x: f64,
        y: f64,
    ) {
        if self
            .outgoing
            .as_ref()
            .is_some_and(|outgoing| outgoing.over_desktop)
        {
            self.dirty |= self
                .selection
                .motion(&self.placed, Point::new(x as i32, y as i32));
        }
    }

    fn selection(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _device: &WlDataDevice) {}

    fn drop_performed(&mut self, _c: &Connection, _q: &QueueHandle<Self>, device: &WlDataDevice) {
        let Some(offer) = drag_offer(device) else {
            return;
        };
        let Some(outgoing) = self
            .outgoing
            .as_mut()
            .filter(|outgoing| outgoing.over_desktop)
        else {
            return;
        };
        outgoing.landed = true;
        // Nothing to read: the icons and where they go are all here already.
        offer.finish();
        offer.destroy();
        self.drop_icons(Point::new(offer.x as i32, offer.y as i32));
    }
}

impl DataOfferHandler for Desktop {
    fn source_actions(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
    }
}

impl DataSourceHandler for Desktop {
    fn accept_mime(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _source: &WlDataSource,
        _mime: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        mut pipe: WritePipe,
    ) {
        use std::io::Write;
        let Some(bytes) = self
            .outgoing
            .as_ref()
            .filter(|outgoing| outgoing.source.inner() == source)
            .and_then(|outgoing| outgoing.payload.bytes_for(&mime))
        else {
            return;
        };
        // A few lines of paths, well inside a pipe's buffer, so this does not block the loop.
        // A target that hung up first is its own concern.
        if let Err(err) = pipe.write_all(&bytes) {
            eprintln!("wlrix-desktop: could not hand over the dragged files: {err}");
        }
    }

    fn cancelled(&mut self, _c: &Connection, _q: &QueueHandle<Self>, source: &WlDataSource) {
        self.end_drag(source);
    }

    fn dnd_dropped(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _source: &WlDataSource) {}

    fn dnd_finished(&mut self, _c: &Connection, _q: &QueueHandle<Self>, source: &WlDataSource) {
        self.end_drag(source);
    }

    fn action(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _source: &WlDataSource,
        _action: DndAction,
    ) {
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
delegate_data_device!(Desktop);

/// The scheme `config` names, or the default if it names nothing this build ships.
///
/// Never fails. A desktop that refused to start over a misspelled scheme name would be a
/// worse answer than a desktop in the wrong colors.
/// The name under the home folder: the login name, as IRIX showed it.
///
/// `$USER` as a login shell sets it, then `$LOGNAME`, and failing both the home directory's own
/// name -- which on almost every system is the same word.
fn login_name(home: &std::path::Path) -> String {
    ["USER", "LOGNAME"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok().filter(|name| !name.is_empty()))
        .or_else(|| {
            home.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "home".to_owned())
}

fn resolve_palette(config: &Config) -> &'static Palette {
    let (palette, unknown) = wlrix_ui::palette::resolve(config.appearance.palette.as_deref());
    if let Some(why) = unknown {
        eprintln!("wlrix-desktop: {why}; using {}", palette.id);
    }
    palette
}

#[cfg(test)]
mod tests {
    use super::IconPlacement;

    #[test]
    fn a_new_surface_is_moved_with_offset_and_never_with_attach() {
        // A non-zero attach on a version 5 surface is the protocol error that killed the desktop.
        for version in [5, 6] {
            let placement = IconPlacement::for_version(version, -40, -12);
            assert_eq!(placement.attach, (0, 0));
            assert_eq!(placement.offset, Some((-40, -12)));
        }
    }

    #[test]
    fn an_old_surface_is_moved_with_attach() {
        let placement = IconPlacement::for_version(4, -40, -12);
        assert_eq!(placement.attach, (-40, -12));
        assert_eq!(placement.offset, None);
    }
}
