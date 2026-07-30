# wlrix-desktop

The wlRIX desktop. Shows the files in your XDG desktop directory as icons, IRIX-style.

- **Language:** Rust
- **License:** GPL-3.0-or-later
- **Reference:** IRIX 6.5's Indigo Magic Desktop

Icons fill from the **top right of the primary monitor, downward**, starting a new column to the left when one runs out
of room — the opposite corner and axis from the minimized-window tiles `wlrix-compositor` draws top-left, so the two
grids grow away from each other.

Each icon is drawn as a coverage mask and tinted by its state: knocked-back gray at rest, white under the pointer, and
IRIX's lamp yellow when selected. Drawing the shape once and blending the tint through it means the three states cannot
drift apart, and it is what makes real artwork a drop-in later — supply a mask instead of a drawing routine.

## How it reaches the screen

A **wlr-layer-shell surface on the `bottom` layer**, anchored to all four edges and painted with a **transparent**
background. That puts it below every window but above the `background` layer, which is where the wallpaper lives
(`swaybg` and friends) — so the wallpaper shows through wherever an icon is not drawn.

Not `background`: two clients sharing a layer have no defined order between them, so a wallpaper restarted after the
desktop would be composited over the icons.

This is why `wlrix-compositor` had to learn to route input to layer surfaces: `wlrix-greeter`
only ever put an *inert* backdrop on a layer, whereas here the background is the interactive part. The compositor
hit-tests overlay/top layers above windows and bottom/background below them, and gives a layer surface keyboard focus
when it is clicked and asked for `on-demand`
interactivity. Without that, the icons could not be clicked at all.

Three settings differ from the greeter's backdrop and are load-bearing:

- `Layer::Bottom`, not `Background` — above the wallpaper, below the windows.
- `exclusive_zone(0)`, not `-1` — shrink to avoid a panel's reserved space rather than covering it.
- `KeyboardInteractivity::OnDemand`, so clicking the desktop takes focus off whatever window had it.

The surface takes clicks across its whole area even where it is transparent, which is what makes clicking bare desktop
deselect. A wallpaper below it never sees the pointer, which is correct — wallpapers are not interactive.

## Build and run

```sh
cargo build
```

It needs a running compositor with `zwlr_layer_shell_v1`. `wlrix-session` starts it automatically, before the toolchest
and the Desks overview.

```sh
WAYLAND_DISPLAY=wayland-1 cargo run
```

## Configuration

Two files, following the same split `wlrix-compositor` uses for displays: a hand-edited one holding *defaults*, and a
machine-written one holding *what is true right now*. They merge per field with the state file winning, so a snap
setting changed at runtime outlives a restart while a hand-set cell size stays hand-set.

`~/.config/wlrix/desktop.toml` — hand-edited. Unknown keys are an error.

```toml
snap_to_grid = false    # icons drop where released; true aligns them to cells
output = "DP-1"         # which monitor gets the icons; default is the leftmost

[metrics]
icon = 64               # the icon artwork, square
cell_width = 96         # one cell: the icon plus its label
cell_height = 104
gap = 8                 # between cells
margin = 12             # between the outermost cells and the screen edge
```

`$XDG_STATE_HOME/wlrix/desktop-icons.toml` — machine-written. Icons are keyed by file name, not path, so moving the
desktop directory keeps every icon where it was left.

**Every** icon is recorded, not just the ones you dragged. An icon whose position is only implied by its place in the
alphabetical list is not really placed at all, so moving or deleting one would make everything below it slide up to
close the gap. Pinning what the first layout decided means a gap stays a gap, and a new file fills a hole rather than
pushing its neighbors down.

```toml
snap_to_grid = true

[[icon]]
name = "notes.txt"
slot = 3

[[icon]]
name = "photo.png"
x = 412
y = 260
```

The desktop directory itself comes from `XDG_DESKTOP_DIR`, then `~/.config/user-dirs.dirs`, then `~/Desktop`. It does
not have to exist — an empty desktop is a desktop, and an inotify watch on the parent picks the directory up if it
appears later.

## Colors

`src/theme/palette.rs` is **generated** by `wlrix-assets/tools/palettegen` from the shared palette JSON, the same source
the compositor, the greeter and the Avalonia theme are built from. Edit the JSON and re-run `just palette` from
`wlrix-epoch`; never edit the generated file.
