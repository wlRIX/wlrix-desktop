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
    Connection, Dispatch, Proxy, QueueHandle,
    backend::WaylandError,
    globals::registry_queue_init,
    protocol::{
        wl_output::WlOutput, wl_pointer::WlPointer, wl_seat::WlSeat, wl_shm, wl_surface::WlSurface,
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
use crate::watch::Watch;
use wlrix_ui::canvas::Canvas;
use wlrix_ui::palette::Palette;
use wlrix_ui::text::{Face, Fonts};

/// `wl_pointer`'s buttons, from `linux/input-event-codes.h`.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

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

    fonts: Fonts,
    directory: PathBuf,
    watch: Watch,
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
        configured: false,
        width: 0,
        height: 0,
        pointer: None,
        fonts,
        directory,
        watch,
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
            }
            Err(why) => eprintln!("wlrix-desktop: {why}"),
        }
    }

    /// Post the desktop menu at `at`, clamped so the whole panel is reachable.
    ///
    /// What each row can do is decided here, once, from the selection as it stands -- so a
    /// row that looks disabled cannot act, and one that looks live cannot fail.
    fn open_menu(&mut self, at: Point) {
        let selected = self.selection.selected().len();
        let launcher = self.lone_launcher();
        let area = Rect::new(0, 0, self.width.max(1) as i32, self.height.max(1) as i32);

        // How wide a label is belongs to the font, and the menu knows nothing about fonts; it
        // asks through here. `&mut` because `Fonts` caches its shaping as it measures.
        let fonts = &mut self.fonts;
        let mut measure = |label: &str| fonts.width(Face::Bold, crate::menu::LABEL_PX, label);
        let actions = launcher
            .as_ref()
            .map(|(name, items)| MenuActions { entry: name, items });

        let size = Menu::size_for(selected, actions.clone(), &mut measure);
        let origin = Menu::clamp(at, size, area);
        let menu = Menu::new(origin, selected, actions, &mut measure);

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
        let mut removed = 0;
        for name in &selected {
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
                }
                PointerEventKind::Leave { .. } => {
                    self.dirty |= self.selection.leave();
                }
                // A right click posts the menu; a second one moves it.
                PointerEventKind::Press { button, .. } if button == BTN_RIGHT => {
                    self.open_menu(point);
                }
                PointerEventKind::Press { button, time, .. } if button == BTN_LEFT => {
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
                    let grid = self.grid();
                    // A drop can move several icons at once, when a rubber band selected them
                    // and one of them was then dragged.
                    let dropped = self
                        .selection
                        .release(&self.placed, &grid, self.snap_to_grid);
                    if !dropped.is_empty() {
                        for drop in dropped {
                            self.saved.set_spot(&drop.name, drop.spot);
                        }
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

/// The scheme `config` names, or the default if it names nothing this build ships.
///
/// Never fails. A desktop that refused to start over a misspelled scheme name would be a
/// worse answer than a desktop in the wrong colors.
fn resolve_palette(config: &Config) -> &'static Palette {
    let (palette, unknown) = wlrix_ui::palette::resolve(config.appearance.palette.as_deref());
    if let Some(why) = unknown {
        eprintln!("wlrix-desktop: {why}; using {}", palette.id);
    }
    palette
}
