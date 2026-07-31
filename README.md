# wlrix-desktop

The wlRIX desktop. Shows the files in your XDG desktop directory as icons, IRIX-style.

- **Language:** Rust
- **License:** GPL-3.0-or-later
- **Reference:** IRIX 6.5's Indigo Magic Desktop

Icons fill from the **top right of the primary monitor, downward**, starting a new column to the left when one runs out
of room — the opposite corner and axis from the minimized-window tiles `wlrix-compositor` draws top-left, so the two
grids grow away from each other.

Files and folders are drawn as coverage masks and tinted by state: knocked-back gray at rest, white under the pointer,
and IRIX's lamp yellow when selected. Drawing the shape once and blending the tint through it means the three states
cannot drift apart.

## Labels

The label under an icon is the file name, except for a `.desktop` launcher, which is labeled with its `Name` — so
`mpv.desktop` reads "mpv Media Player". A launcher with no `Name`, or one too broken to parse, falls back to its file
name rather than going nameless. Icons are ordered by the label, so the order on screen is the order of the words the
user can actually read; the file name breaks ties, since two launchers may share a `Name`.

The label is only ever what is *shown*. Identity stays the file name — that is what positions are remembered under, and
what selection and hit-testing work in terms of — so changing `LANG` does not scatter every launcher across the desktop.

### Translations

`Name` is a `localestring`: a launcher carries one per language, and the spec says which of them a given locale takes.

```ini
Name=mpv Media Player
Name[fr]=Lecteur multimédia mpv
Name[ja]=mpv メディアプレイヤー
Name[zh_CN]=mpv 媒体播放器
Name[zh_TW]=mpv 媒體播放器
```

The locale comes from `LC_ALL`, then `LC_MESSAGES`, then `LANG`. It reads `lang_COUNTRY.ENCODING@MODIFIER`; the encoding
takes no part in matching, and the rest is tried most specific first — `sr_RS@latin` looks for `sr_RS@latin`,
`sr_RS`, `sr@latin`, `sr`, and then the plain `Name`. This is the spec's list and not a general "try a shorter language"
rule: `zh_CN` and `zh_TW` are different text and neither stands in for the other. `C`, `POSIX` and an unset environment
all mean the plain key.

`Icon` is a `localestring` too, and follows the same lookup. `Exec` is not, and does not — a translated command line
would be a different program.

Labels wrap to two lines, breaking between words where it can. Where a word break would cost the end of the name — as it
does for "mpv メディアプレイヤー", which has one space and then a run too long for a line — it breaks mid-run instead.
Showing the whole name is worth more than tidy words. What still does not fit is cut with an ellipsis.

## The magic carpet

An application launcher is drawn as two layers, as IRIX's Indigo Magic desktop did: a **magic carpet** underneath saying
whether the application is running — lying flat when it is not, stood upright when it is — with the application's own
**symbol** standing on it. The symbol's placement is the same in both states, so starting an application rotates the
carpet under a symbol that stays put.

Only `Type=Application` entries get one; a file, a folder or a `Type=Link` bookmark has no running state to show.

**The tint currently only goes on the carpet, not the symbol.** The carpet is wlRIX's own artwork, so gray/white/yellow
is exactly the IRIX rule there, while an application's icon keeps the colors it was drawn in. The tint *multiplies*
rather than replaces, so the carpet's black outline and shadow survive a yellow selection instead of flattening into a
blob.

`Icon=` is resolved through the XDG icon theme — index.theme, inheritance, size directories and the
`/usr/share/pixmaps` fallback — and rasterized at whatever the configured icon size is, so an SVG stays crisp.

### `X-WLRIX-Running-Icon`

A wlRIX extension, not in the spec, using the `X-` prefix the spec reserves for exactly this — so a file carrying it
remains a valid desktop entry everywhere else. It names an icon to show *instead of* `Icon=` while the application is
running, which is a thing IRIX allowed:

```ini
[Desktop Entry]
Type=Application
Name=Showcase
Exec=showcase
Icon=showcase
X-WLRIX-Running-Icon=showcase-running
```

### How "running" is decided

Two sources, because neither alone is right. **Windows**, via `ext-foreign-toplevel-list-v1`, are the honest answer —
they count what is actually on screen however it was started, and notice an application quitting. But they say nothing
during the second or two before the first window maps, so a **process we launched** counts too. A window is matched to a
launcher by `StartupWMClass`, then the desktop file's basename, then the `Exec` binary — case-insensitively.

## The desktop menu

Right-clicking posts the 4Dwm menu, styled as the compositor's window menu and opening with a centred "Desktop"
header. Log Out, Open, Remove and Select All work; Make Copy, Make Reference, Change Permissions and Add New Directory
are drawn disabled so the menu's shape is right. Open and Remove gray out with nothing selected.

### Launcher actions

A `.desktop` file can offer more than one way in. Steam's lists Store, Library, Friends and the rest as
`[Desktop Action …]` groups; a browser offers a private window. When **exactly one** launcher is selected and it has
any, they are added below the fixed items, after a third separator:

```ini
[Desktop Entry]
Type=Application
Name=Steam
Exec=/usr/bin/steam %U
Actions=Store;Library;Friends;

[Desktop Action Store]
Name=Store
Name[ja]=ストア
Name[uk]=Крамниця
Exec=/usr/bin/steam steam://store
```

Exactly one, because an action belongs to a particular file: "Store" means nothing applied to three selected icons at
once. Names are `localestring`s and translate like any other, so the rows read in the user's language.

`Actions=` decides which groups are offered and in what order, as the spec says — a `[Desktop Action …]` group it does
not name is ignored, so a group nobody listed is not an offer. **One deviation:** a file with action groups and *no*
`Actions=` key would be ignored entirely by the spec, and here the groups are taken in file order instead. Such a file
is malformed either way, and a hand-written one that forgot the key is better served by showing them. An action missing
`Name` or `Exec` is dropped in both cases; the spec requires both, and without them there is no row to draw or nothing
to run.

The panel is a fixed 186px wide until an action's label needs more, at which point it grows — a translated name out of a
file is not bounded by "Change Permissions". A menu with no actions is laid out exactly as it always was.

Choosing an action **re-reads the file**, rather than running what the menu was built from: the menu can sit open for as
long as the user likes, and a launcher edited underneath it should run what it says now or not at all. The same rules
apply as to opening the launcher — the execute bit is still the consent, since an action is a command line out of the
same file, and `TryExec`, `Terminal=true` and `Path=` are all honored.

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
open = ["xdg-open"]     # what opens a file or URL; the target is appended
terminal = []           # what wraps a Terminal=true launcher; empty means work it out

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
