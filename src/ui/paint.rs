// SPDX-License-Identifier: GPL-3.0-or-later
//! Composing the desktop: background, icons, labels.
//!
//! One pass over the laid-out icons, drawing each into its cell. Kept apart from
//! [`crate::ui`]'s Wayland plumbing so the drawing can be reasoned about -- and rendered into
//! a plain buffer by a test -- without a compositor.
//!
//! ## The magic carpet
//!
//! An application launcher is drawn as two layers, the way IRIX's Indigo Magic desktop did: a
//! **magic carpet** underneath saying whether the application is running -- flat when it is
//! not, stood upright when it is -- and the application's own **symbol** standing on it. The
//! symbol's placement does not change between the two, so starting an application rotates the
//! carpet under a symbol that stays put, which is what the IRIX figure shows.
//!
//! Only `Type=Application` entries get one. A file, a folder, or a `Type=Link` bookmark has no
//! running state to show, so they keep their drawn-in-code icons on the bare desktop.

use std::path::Path;

use crate::entries::Entry;
use crate::icon_theme::Icons;
use crate::layout::Placed;
use crate::menu::Menu;
use crate::running::Running;
use crate::select::Selection;
use wlrix_ui::text::{Face, Fonts, Run};

use crate::ui::icons::{self, label_color};
use wlrix_ui::IconState;
use wlrix_ui::canvas::{Canvas, Rect};
use wlrix_ui::motif;
use wlrix_ui::palette::Palette;

/// Label text size, in pixels.
const LABEL_PX: f32 = 12.0;
/// Gap between the icon square and the first line of its label.
const LABEL_GAP: i32 = 4;
/// How many lines a label may wrap to before it is cut short.
const LABEL_LINES: usize = 2;
/// Padding around the label's highlight, so a selected name is not painted edge to edge.
const LABEL_PAD: i32 = 2;

/// How big an application's symbol is, as a fraction of the icon square -- one drawn as an
/// ordinary theme icon. An IRIX drawing is in the carpet's frame and covers the whole square.
///
/// The rest of the square is the carpet it stands on. Eyeballed against the IRIX figure; easy
/// to retune, and the only thing that decides how the two layers sit together.
const SYMBOL_SCALE: f32 = 0.56;
/// Where the symbol's *bottom* sits, as a fraction down the icon square.
///
/// One value for both carpet states on purpose: in the IRIX figure the symbol stays where it
/// is and the carpet rotates beneath it. A symbol that jumped when an application started
/// would read as two different icons.
const SYMBOL_BASELINE: f32 = 0.66;

/// The carpet artwork, alongside the binary.
///
/// Loaded from the source tree at compile time rather than looked up at runtime: these are the
/// desktop's own drawings, not theme icons, and an installed desktop should not be able to lose
/// them to a missing data directory.
const CARPET_CLOSED: &[u8] = include_bytes!("../../assets/generic.exec.closed.svg");
const CARPET_OPEN: &[u8] = include_bytes!("../../assets/generic.exec.open.svg");

/// Everything one frame needs.
///
/// A struct rather than eight arguments, and it keeps the borrow of each piece explicit -- the
/// image cache in particular has to be mutable, since drawing is what fills it.
pub struct Frame<'a> {
    /// The scheme everything is drawn in. `&'static`, so this struct keeps the one lifetime
    /// it already had.
    pub palette: &'static Palette,
    pub fonts: &'a mut Fonts,
    pub images: &'a mut Icons,
    pub placed: &'a [Placed],
    pub selection: &'a Selection,
    /// Which applications have a window open. Empty is a perfectly good answer -- it means
    /// every carpet is closed.
    pub running: &'a Running,
    /// The right-click menu, while one is posted.
    pub menu: Option<&'a Menu>,
    pub icon_size: i32,
}

/// Draw the whole desktop.
///
/// `placed` is in back-to-front order, matching [`crate::select`]'s hit-testing, so an icon
/// dropped over another covers it and is also the one picked up.
pub fn desktop(canvas: &mut Canvas, frame: &mut Frame) {
    // Transparent, not the desktop gray: a wallpaper client sits on the layer below, and
    // painting a background here would hide it. With no wallpaper the compositor's own clear
    // color -- the same gray -- shows through, so the bare desktop looks unchanged.
    canvas.clear_transparent();

    // A drag, drawn as IRIX drew one (the IRIX Interactive Desktop UI Guidelines' figure of
    // icon states): every dragged icon leaves a dark gray ghost in its cell and follows the
    // pointer in selected yellow, and the icon under the pointer turns royal blue if it would
    // take the drop.
    let target = drop_target(frame.placed, frame.selection);

    for item in frame.placed {
        let home = Rect::new(item.rect.x, item.rect.y, item.rect.w, item.rect.h);
        if frame.selection.is_dragging(&item.entry.name) {
            icon_labeled(
                canvas,
                frame,
                item,
                home,
                IconState::Ghost,
                IconState::Selected,
            );
        } else if target.is_some_and(|target| std::ptr::eq(target, item)) {
            let accepting = accepting(item);
            icon(canvas, frame, &accepting, home, IconState::DropTarget);
        } else {
            let tint = tint_for(frame.selection, &item.entry.name);
            icon(canvas, frame, item, home, tint);
        }
    }

    // The dragged icons themselves, after everything else so nothing they pass over covers
    // them. Drawn under the pointer rather than in their cells, so they follow the hand instead
    // of jumping there on release; a whole selection moves by the same delta, which keeps the
    // group rigid.
    if let Some(delta) = frame.selection.drag_delta() {
        for item in frame.placed {
            if frame.selection.is_dragging(&item.entry.name) {
                let cell = Rect::new(
                    item.rect.x + delta.x,
                    item.rect.y + delta.y,
                    item.rect.w,
                    item.rect.h,
                );
                icon_labeled(
                    canvas,
                    frame,
                    item,
                    cell,
                    IconState::Selected,
                    IconState::Normal,
                );
            }
        }
    }

    // The rubber band goes last, over everything: it is a transient marker, not part of the
    // desktop, and an icon drawn on top of it would look like a hole in the band.
    if let Some(band) = frame.selection.band() {
        band_outline(canvas, frame.palette, band);
    }

    // The menu is above even that: it is chrome laid over the desktop, not on it.
    if let Some(menu) = frame.menu {
        self::menu(canvas, frame.palette, frame.fonts, menu);
    }
}

/// Where the dragged icons sit, together: the smallest rectangle around all their cells.
///
/// The size of the drag image the compositor carries; `None` when nothing is being dragged.
pub fn drag_bounds(placed: &[Placed], selection: &Selection) -> Option<Rect> {
    let mut cells = placed
        .iter()
        .filter(|item| selection.is_dragging(&item.entry.name))
        .map(|item| item.rect);
    let first = cells.next()?;
    let (mut left, mut top) = (first.x, first.y);
    let (mut right, mut bottom) = (first.x + first.w, first.y + first.h);
    for cell in cells {
        left = left.min(cell.x);
        top = top.min(cell.y);
        right = right.max(cell.x + cell.w);
        bottom = bottom.max(cell.y + cell.h);
    }
    Some(Rect::new(left, top, right - left, bottom - top))
}

/// The drag image: the dragged icons as they follow the pointer, for the compositor to carry.
///
/// Drawn into a canvas the size of [`drag_bounds`], each icon where it sits within them, so a
/// group keeps its shape. The same look as the icons drawn under the pointer before a drag
/// left the desktop -- selected yellow with a plain name -- so nothing changes as the drag
/// becomes one the rest of the desktop can see.
pub fn drag_image(canvas: &mut Canvas, frame: &mut Frame, bounds: Rect) {
    canvas.clear_transparent();
    for item in frame.placed {
        if frame.selection.is_dragging(&item.entry.name) {
            let cell = Rect::new(
                item.rect.x - bounds.x,
                item.rect.y - bounds.y,
                item.rect.w,
                item.rect.h,
            );
            icon_labeled(
                canvas,
                frame,
                item,
                cell,
                IconState::Selected,
                IconState::Normal,
            );
        }
    }
}

/// Draw the desktop menu: a raised panel, a centered title, and the rows.
///
/// The geometry and the colors are the compositor's window menu
/// (`wlrix-compositor/src/{menu,decoration}.rs`), because the two are the same object as far
/// as the user is concerned.
fn menu(canvas: &mut Canvas, palette: &Palette, fonts: &mut Fonts, menu: &Menu) {
    let panel = menu.panel();
    motif::panel(
        canvas,
        Rect::new(panel.x, panel.y, panel.w, panel.h),
        palette.face,
        motif::Bevel::raised(
            palette.face_top_shadow,
            palette.face_bottom_shadow,
            crate::menu::BEVEL,
        ),
    );

    let ascent = fonts.ascent(Face::Bold, crate::menu::LABEL_PX);
    let line = fonts.line_height(Face::Bold, crate::menu::LABEL_PX);

    for (index, entry) in menu.entries.iter().enumerate() {
        let row = menu.row(index);
        let row = Rect::new(row.x, row.y, row.w, row.h);

        if entry.is_separator() {
            motif::groove(
                canvas,
                row,
                palette.face_top_shadow,
                palette.face_bottom_shadow,
            );
            continue;
        }

        // The pointed-at row stands proud of the panel, as a button does.
        if menu.hovered == Some(index) {
            motif::panel(
                canvas,
                row,
                palette.title_active,
                motif::Bevel::raised(
                    palette.face_top_shadow,
                    palette.face_bottom_shadow,
                    crate::menu::BEVEL,
                ),
            );
        }

        let baseline = row.y + (row.h - line) / 2 + ascent;
        let width = fonts.width(Face::Bold, crate::menu::LABEL_PX, &entry.label);
        let (x, color) = if entry.is_header() {
            // The title is centered; everything else is left-aligned at the label inset.
            ((row.x + (row.w - width) / 2), palette.foreground)
        } else if entry.enabled {
            (row.x + crate::menu::LABEL_INSET, palette.foreground)
        } else {
            // Disabled rows take the bottom shadow, which is how Motif grays a label out.
            (row.x + crate::menu::LABEL_INSET, palette.face_bottom_shadow)
        };
        fonts.draw(
            canvas,
            Run {
                face: Face::Bold,
                px: crate::menu::LABEL_PX,
                x,
                baseline,
                color,
            },
            &entry.label,
        );

        // The header is closed off with a groove, as the IRIX menu had.
        if entry.is_header() {
            motif::groove(
                canvas,
                Rect::new(row.x, row.y + row.h - 2, row.w, 2),
                palette.face_top_shadow,
                palette.face_bottom_shadow,
            );
        }
    }
}

/// Draw the rubber band: an unfilled black outline, nothing inside it.
///
/// Unfilled on purpose -- a translucent wash would tint every icon it passes over and fight
/// the hover and selection colors, which are the thing the band is there to change.
fn band_outline(canvas: &mut Canvas, palette: &Palette, band: crate::layout::Rect) {
    // A band with no width or height is the instant before the pointer moves; stroking it
    // would leave a stray dot on the desktop.
    if band.w <= 0 || band.h <= 0 {
        return;
    }
    canvas.stroke_rect(
        Rect::new(band.x, band.y, band.w, band.h),
        palette.outer_line,
    );
}

/// Whether an application launcher is running, and so which carpet it stands on.
///
/// Only `Type=Application` entries can be: a `Link` bookmark or an unreadable desktop file has
/// no process behind it.
fn is_running(entry: &Entry, running: &Running) -> bool {
    entry
        .launcher
        .as_ref()
        .is_some_and(|launcher| launcher.is_application && running.any(&launcher.identities))
}

/// Draw an application's carpet and symbol into `square`.
///
/// Returns whether anything was drawn: a launcher whose `Icon=` resolves to nothing still gets
/// its carpet, but if even the carpet fails to rasterize the caller falls back to the
/// drawn-in-code icon rather than leaving an empty cell.
fn application(
    canvas: &mut Canvas,
    frame: &mut Frame,
    entry: &Entry,
    square: Rect,
    running: bool,
    tint: IconState,
) -> bool {
    let size = square.w.min(square.h);
    if size <= 0 {
        return false;
    }

    // The carpet fills the square; the symbol stands on it.
    //
    // The **carpet** always carries the hover and selection tint: it is wlRIX's own artwork, so
    // gray/white/yellow is exactly the IRIX rule. The symbol carries it only when it is an IRIX
    // drawing too (below); an application's own icon wants the colors it was drawn in. Without
    // the carpet's, such a launcher would be the one thing on the desktop that does not react
    // to the pointer at all.
    let carpet = if running { CARPET_OPEN } else { CARPET_CLOSED };
    let Some(carpet) = frame.images.load_bytes(carpet_key(running), carpet, size) else {
        return false;
    };
    carpet
        .in_state(tint, frame.palette)
        .draw(canvas, square.x, square.y);

    // `X-WLRIX-Running-Icon` replaces `Icon` while the application is up -- IRIX let an
    // application show a different symbol once it was running, and this is that.
    let launcher = entry.launcher.as_ref();
    let wanted = launcher.and_then(|launcher| {
        running
            .then_some(launcher.running_icon.as_deref())
            .flatten()
            .or(launcher.icon.as_deref())
    });
    let Some(wanted) = wanted else {
        // No `Icon=` at all: the carpet alone still says running or not.
        return true;
    };

    let symbol_size = ((size as f32) * SYMBOL_SCALE).round() as i32;
    let Some(path) = frame.images.resolve(wanted, symbol_size) else {
        return true;
    };
    // An IRIX drawing is in the carpet's own frame and goes over it 1:1, in register, and takes
    // the tint with it -- the IRIX icon catalog turned a selected keyboard's white body yellow
    // along with its carpet. Anything else is an application's ordinary icon: shrunk, stood on
    // the carpet, and left in the colors it was drawn in. See `icon_theme::is_carpet_frame`.
    let palette = frame.palette;
    if frame.images.is_registered(&path) {
        if let Some(symbol) = frame.images.load(&path, size) {
            symbol
                .in_state(tint, palette)
                .draw(canvas, square.x, square.y);
        }
    } else if let Some(symbol) = frame.images.load(&path, symbol_size) {
        let bottom = square.y + ((size as f32) * SYMBOL_BASELINE).round() as i32;
        symbol.draw(
            canvas,
            square.x + (size - symbol_size) / 2,
            bottom - symbol_size,
        );
    }
    true
}

/// Draw a file's or directory's icon-theme artwork, filling the square. `false` if none of the
/// entry's names resolves or loads, so the caller can fall back.
///
/// In the state the way a carpet is, by IRIX's rule ([`wlrix_ui::icon_state`]): only the light
/// body changes -- stippled gray at rest, white under the pointer, yellow when selected -- and
/// the outline, the drop shadow and colored detail stay as drawn. The wlrix theme's IRIX art
/// is drawn with exactly such a body. Colored art from an inherited theme has little or none,
/// so it changes little; its label still inverts when selected.
fn themed(
    canvas: &mut Canvas,
    frame: &mut Frame,
    entry: &Entry,
    square: Rect,
    tint: IconState,
) -> bool {
    let size = square.w.min(square.h);
    if size <= 0 || entry.icon_names.is_empty() {
        return false;
    }
    let Some(image) = frame.images.get_first(&entry.icon_names, size) else {
        return false;
    };
    // Centered in the square: a raster icon may come back smaller than asked for.
    let (x, y) = (
        square.x + (size - image.width) / 2,
        square.y + (size - image.height) / 2,
    );
    image.in_state(tint, frame.palette).draw(canvas, x, y);
    true
}

/// The cache key for a carpet. Not a real path -- the artwork is compiled in -- but the cache
/// is keyed by path, and these two names cannot collide with a file on disk.
fn carpet_key(running: bool) -> &'static Path {
    Path::new(if running {
        "<wlrix:carpet-open>"
    } else {
        "<wlrix:carpet-closed>"
    })
}

/// The icon a drag would drop on: the topmost one under the pointer that is not itself being
/// dragged and accepts a drop. `None` when nothing is being dragged.
fn drop_target<'a>(placed: &'a [Placed], selection: &Selection) -> Option<&'a Placed> {
    let point = selection.drag_point()?;
    placed
        .iter()
        .rev()
        .filter(|item| !selection.is_dragging(&item.entry.name))
        .find(|item| item.rect.contains(point))
        .filter(|item| item.entry.accepts_drop())
}

/// `item` as it looks while it would accept a drop. The Dumpster flings its lid open; anything
/// else looks as it always does, and the tint alone says it will take the drop.
fn accepting(item: &Placed) -> Placed {
    let mut item = item.clone();
    if item.entry.kind == crate::entries::Kind::Dumpster {
        item.entry
            .icon_names
            .insert(0, crate::entries::DUMPSTER_OPEN.to_owned());
    }
    item
}

/// Which tint an icon takes. Selection wins over hover: a selected icon under the pointer
/// should stay yellow rather than flicking to white as the pointer crosses it.
fn tint_for(selection: &Selection, name: &str) -> IconState {
    if selection.is_selected(name) {
        IconState::Selected
    } else if selection.hovered() == Some(name) {
        IconState::Hover
    } else {
        IconState::Normal
    }
}

/// Draw one icon and its label inside `cell`.
fn icon(canvas: &mut Canvas, frame: &mut Frame, item: &Placed, cell: Rect, tint: IconState) {
    icon_labeled(canvas, frame, item, cell, tint, tint);
}

/// [`icon`], with the label in a state of its own.
///
/// For a drag, where the two part company: the name stays highlighted under the ghost, which is
/// where the selection still is, and the copy following the pointer carries a plain one -- as
/// in IRIX's figure of a drag, whose dragged image has an unhighlighted name.
fn icon_labeled(
    canvas: &mut Canvas,
    frame: &mut Frame,
    item: &Placed,
    cell: Rect,
    tint: IconState,
    label_tint: IconState,
) {
    // The icon square, centered across the cell and hard against its top.
    let size = frame.icon_size.min(cell.w).min(cell.h);
    let square = Rect::new(cell.x + (cell.w - size) / 2, cell.y, size, size);

    // An application gets its own artwork on a magic carpet; everything else gets its type's
    // icon from the theme. The drawn-in-code icon is the fallback for both, when no artwork
    // will load.
    let drawn = item
        .entry
        .launcher
        .as_ref()
        .is_some_and(|launcher| launcher.is_application)
        && application(
            canvas,
            frame,
            &item.entry,
            square,
            is_running(&item.entry, frame.running),
            tint,
        );
    let drawn = drawn || themed(canvas, frame, &item.entry, square, tint);
    if !drawn {
        icons::draw(canvas, frame.palette, square, item.entry.kind, tint);
    }

    label(
        canvas,
        frame.palette,
        frame.fonts,
        &item.entry.label,
        Rect::new(
            cell.x,
            square.y + size + LABEL_GAP,
            cell.w,
            (cell.h - size - LABEL_GAP).max(0),
        ),
        label_tint,
    );
}

/// Wrap a filename to the label's face and size.
///
/// The algorithm is [`wlrix_ui::text::wrap`]; this only supplies the two constants that make
/// it the *desktop's* wrapping rather than anyone else's.
fn wrap(fonts: &mut Fonts, name: &str, width: i32, max_lines: usize) -> Vec<String> {
    wlrix_ui::text::wrap(fonts, Face::Regular, LABEL_PX, name, width, max_lines)
}

/// Draw a filename under its icon, wrapped and centered.
fn label(
    canvas: &mut Canvas,
    palette: &Palette,
    fonts: &mut Fonts,
    name: &str,
    area: Rect,
    tint: IconState,
) {
    if area.h <= 0 || area.w <= 0 {
        return;
    }
    let line_height = fonts.line_height(Face::Regular, LABEL_PX);
    let ascent = fonts.ascent(Face::Regular, LABEL_PX);
    // How many lines actually fit, which is not the same as how many we would like. `wrap`
    // has to be told, or it produces a line that is silently dropped below and the label
    // reads as cut with nothing saying so.
    let fits = if line_height > 0 {
        (area.h / line_height).max(1) as usize
    } else {
        1
    };
    let lines = wrap(fonts, name, area.w - 2 * LABEL_PAD, fits.min(LABEL_LINES));

    // A selected label inverts onto its tint, the way IRIX highlighted a picked name.
    if tint == IconState::Selected {
        let widest = lines
            .iter()
            .map(|line| fonts.width(Face::Regular, LABEL_PX, line))
            .max()
            .unwrap_or(0);
        let height = (line_height * lines.len() as i32).min(area.h);
        canvas.fill_rect(
            Rect::new(
                area.x + (area.w - widest) / 2 - LABEL_PAD,
                area.y,
                widest + 2 * LABEL_PAD,
                height,
            ),
            tint.flat(palette),
        );
    }

    for (index, line) in lines.iter().enumerate() {
        let top = area.y + line_height * index as i32;
        // Never spill past the cell into the icon below it.
        if top + line_height > area.y + area.h {
            break;
        }
        let width = fonts.width(Face::Regular, LABEL_PX, line);
        fonts.draw(
            canvas,
            Run {
                face: Face::Regular,
                px: LABEL_PX,
                x: area.x + (area.w - width) / 2,
                baseline: top + ascent,
                color: label_color(tint, palette),
            },
            line,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries::{Entry, Kind};
    use crate::layout::{Grid, Metrics, Point, Rect as LayoutRect, Spot};
    use crate::menu::Menu;
    use wlrix_ui::Rgb;

    /// Font loading needs a system font database, which a build machine may not have. Every
    /// test here skips rather than fails in that case -- the drawing is verified on a real
    /// desktop, and a CI box without fonts should not turn red over it.
    fn fonts() -> Option<Fonts> {
        Fonts::load().ok()
    }

    /// A frame with nothing running and a fresh image cache, which is the ordinary case.
    fn frame<'a>(
        fonts: &'a mut Fonts,
        placed: &'a [Placed],
        selection: &'a Selection,
    ) -> Frame<'a> {
        // Leaked so the borrow lives as long as the frame does; these are test-lifetime
        // allocations in a process that is about to exit.
        let images: &'a mut Icons = Box::leak(Box::new(Icons::default()));
        let running: &'a Running = Box::leak(Box::new(Running::default()));
        Frame {
            palette: wlrix_ui::palette::DEFAULT,
            fonts,
            images,
            placed,
            selection,
            running,
            menu: None,
            icon_size: 64,
        }
    }

    fn grid() -> Grid {
        Grid::new(LayoutRect::new(0, 0, 1280, 800), Metrics::default())
    }

    fn placed(names: &[&str]) -> Vec<Placed> {
        let grid = grid();
        names
            .iter()
            .enumerate()
            .map(|(index, name)| Placed {
                entry: Entry {
                    path: format!("/desktop/{name}").into(),
                    name: (*name).to_owned(),
                    label: (*name).to_owned(),
                    kind: Kind::Plain,
                    icon_names: Vec::new(),
                    launcher: None,
                },
                spot: Spot::Slot(index as i32),
                rect: grid.slot_rect(index as i32),
            })
            .collect()
    }

    /// One application launcher, laid out in the first cell.
    fn launcher(name: &str, icon: Option<&str>, running_icon: Option<&str>) -> Vec<Placed> {
        let grid = grid();
        vec![Placed {
            entry: Entry {
                path: format!("/desktop/{name}").into(),
                name: name.to_owned(),
                label: name.to_owned(),
                kind: Kind::Launcher,
                icon_names: Vec::new(),
                launcher: Some(crate::entries::Launcher {
                    is_application: true,
                    actions: Vec::new(),
                    icon: icon.map(str::to_owned),
                    running_icon: running_icon.map(str::to_owned),
                    identities: vec!["thing".to_owned()],
                }),
            },
            spot: Spot::Slot(0),
            rect: grid.slot_rect(0),
        }]
    }

    /// Paint one launcher, with `running` deciding the carpet, and hand back the buffer.
    fn paint_launcher(placed: &[Placed], running: &Running) -> Vec<u8> {
        let mut fonts = Fonts::load().expect("fonts");
        let mut images = Icons::default();
        let mut pixels = vec![0u8; 1280 * 800 * 4];
        {
            let mut canvas = Canvas::new(&mut pixels, 1280, 800);
            desktop(
                &mut canvas,
                &mut Frame {
                    palette: wlrix_ui::palette::DEFAULT,
                    fonts: &mut fonts,
                    images: &mut images,
                    placed,
                    selection: &Selection::default(),
                    running,
                    menu: None,
                    icon_size: 64,
                },
            );
        }
        pixels
    }

    fn running_with(app_id: &str) -> Running {
        let mut running = Running::default();
        running.window(1, app_id);
        running
    }

    #[test]
    fn an_application_launcher_is_drawn_on_a_carpet() {
        if Fonts::load().is_err() {
            return;
        }
        let placed = launcher("thing.desktop", None, None);
        let mut pixels = paint_launcher(&placed, &Running::default());
        // Even with no `Icon=` at all, the carpet alone is drawn.
        let canvas = Canvas::new(&mut pixels, 1280, 800);
        assert!(
            painted(&canvas, placed[0].rect) > 0,
            "the carpet drew nothing"
        );
    }

    #[test]
    fn the_carpet_changes_when_the_application_starts() {
        // The whole point of the feature: running and not-running must look different.
        if Fonts::load().is_err() {
            return;
        }
        let placed = launcher("thing.desktop", None, None);
        let closed = paint_launcher(&placed, &Running::default());
        let open = paint_launcher(&placed, &running_with("thing"));
        assert_ne!(closed, open, "the carpet did not change state");
    }

    #[test]
    fn an_unrelated_window_leaves_the_carpet_closed() {
        if Fonts::load().is_err() {
            return;
        }
        let placed = launcher("thing.desktop", None, None);
        let closed = paint_launcher(&placed, &Running::default());
        let other = paint_launcher(&placed, &running_with("something-else"));
        assert_eq!(closed, other, "an unrelated window should change nothing");
    }

    #[test]
    fn the_running_icon_replaces_the_normal_one() {
        // `X-WLRIX-Running-Icon`: with the *same* carpet state forced by the same `running`,
        // two different symbols must still paint differently.
        if Fonts::load().is_err() {
            return;
        }
        let carpet = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let closed = carpet.join("generic.exec.closed.svg");
        let open = carpet.join("generic.exec.open.svg");

        let running = running_with("thing");
        let same = launcher(
            "thing.desktop",
            Some(&closed.to_string_lossy()),
            Some(&closed.to_string_lossy()),
        );
        let different = launcher(
            "thing.desktop",
            Some(&closed.to_string_lossy()),
            Some(&open.to_string_lossy()),
        );
        assert_ne!(
            paint_launcher(&same, &running),
            paint_launcher(&different, &running),
            "X-WLRIX-Running-Icon should have changed the symbol"
        );
    }

    #[test]
    fn without_the_running_icon_key_the_symbol_stays_the_same() {
        if Fonts::load().is_err() {
            return;
        }
        let icon = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("generic.exec.closed.svg");
        let placed = launcher("thing.desktop", Some(&icon.to_string_lossy()), None);

        // The carpet still changes, but that is the carpet -- the symbol must not vanish.
        let running = paint_launcher(&placed, &running_with("thing"));
        assert!(
            running.iter().any(|byte| *byte != 0),
            "the running launcher drew nothing at all"
        );
    }

    #[test]
    fn a_launcher_reacts_to_hover_and_selection() {
        // Without this a launcher with real artwork would be the one thing on the desktop
        // that does not respond to the pointer: the tint goes on the carpet, not the symbol.
        if Fonts::load().is_err() {
            return;
        }
        let placed = launcher("thing.desktop", None, None);
        let at = Point::new(placed[0].rect.x + 4, placed[0].rect.y + 4);

        let plain = paint_with(&placed, &Selection::default());

        let mut hovered = Selection::default();
        hovered.hover(&placed, at);
        let hovered = paint_with(&placed, &hovered);

        let mut selected = Selection::default();
        selected.press(&placed, at, 0);
        let selected = paint_with(&placed, &selected);

        assert_ne!(plain, hovered, "hovering a launcher changed nothing");
        assert_ne!(plain, selected, "selecting a launcher changed nothing");
        assert_ne!(hovered, selected, "hover and selection look the same");
    }

    /// Paint one launcher with a given selection and nothing running.
    fn paint_with(placed: &[Placed], selection: &Selection) -> Vec<u8> {
        let mut fonts = Fonts::load().expect("fonts");
        let mut images = Icons::default();
        let running = Running::default();
        let mut pixels = vec![0u8; 1280 * 800 * 4];
        {
            let mut canvas = Canvas::new(&mut pixels, 1280, 800);
            desktop(
                &mut canvas,
                &mut Frame {
                    palette: wlrix_ui::palette::DEFAULT,
                    fonts: &mut fonts,
                    images: &mut images,
                    placed,
                    selection,
                    running: &running,
                    menu: None,
                    icon_size: 64,
                },
            );
        }
        pixels
    }

    #[test]
    fn the_rubber_band_is_an_unfilled_black_outline() {
        if Fonts::load().is_err() {
            return;
        }
        let placed = placed(&["a.txt"]);
        let mut selection = Selection::default();
        // From bare desktop, well clear of the icon column.
        selection.press(&placed, Point::new(100, 100), 0);
        selection.motion(&placed, Point::new(300, 260));

        let mut pixels = paint_with(&placed, &selection);
        let canvas = Canvas::new(&mut pixels, 1280, 800);

        // The four edges are drawn in the outline color...
        for (x, y) in [(200, 100), (200, 259), (100, 180), (299, 180)] {
            assert_eq!(
                canvas.get(x, y),
                wlrix_ui::palette::DEFAULT.outer_line,
                "the band edge at ({x},{y}) is not the outline color"
            );
        }
        // ...and the inside is untouched, which is what "unfilled" means.
        for (x, y) in [(200, 180), (150, 130), (250, 240)] {
            assert_eq!(
                canvas.get(x, y),
                Rgb(0),
                "({x},{y}) inside the band was painted"
            );
        }
    }

    #[test]
    fn a_band_that_has_not_moved_paints_nothing() {
        // The instant a press lands the band is zero-sized; stroking it would leave a dot.
        if Fonts::load().is_err() {
            return;
        }
        let placed = placed(&["a.txt"]);
        let mut selection = Selection::default();
        selection.press(&placed, Point::new(100, 100), 0);

        let mut pixels = paint_with(&placed, &selection);
        let canvas = Canvas::new(&mut pixels, 1280, 800);
        assert_eq!(canvas.get(100, 100), Rgb(0), "a stray dot was drawn");
    }

    #[test]
    fn the_menu_is_drawn_over_the_icons() {
        if Fonts::load().is_err() {
            return;
        }
        let placed = placed(&["a.txt"]);
        let mut fonts = Fonts::load().expect("fonts");
        // Posted right over the first icon's cell.
        let menu = Menu::new(
            Point::new(placed[0].rect.x - 20, placed[0].rect.y),
            0,
            None,
            |label| fonts.width(Face::Bold, crate::menu::LABEL_PX, label),
        );

        let mut images = Icons::default();
        let mut pixels = vec![0u8; 1280 * 800 * 4];
        {
            let mut canvas = Canvas::new(&mut pixels, 1280, 800);
            desktop(
                &mut canvas,
                &mut Frame {
                    palette: wlrix_ui::palette::DEFAULT,
                    fonts: &mut fonts,
                    images: &mut images,
                    placed: &placed,
                    selection: &Selection::default(),
                    running: &Running::default(),
                    menu: Some(&menu),
                    icon_size: 64,
                },
            );
        }
        let canvas = Canvas::new(&mut pixels, 1280, 800);

        // Every pixel of the panel is opaque, so neither the icon under it nor the wallpaper
        // below shows through. That is the whole claim: the menu is chrome over the desktop.
        let panel = menu.panel();
        for y in panel.y..panel.y + panel.h {
            for x in panel.x..panel.x + panel.w {
                assert_eq!(
                    canvas.get(x, y).0 >> 24,
                    0xff,
                    "({x},{y}) on the panel is not opaque"
                );
            }
        }
        // Just outside it, the desktop is still bare.
        assert_eq!(canvas.get(panel.x - 1, panel.y + panel.h / 2), Rgb(0));
    }

    #[test]
    fn a_hovered_row_is_highlighted_and_a_disabled_one_is_not() {
        if Fonts::load().is_err() {
            return;
        }
        let placed: Vec<Placed> = Vec::new();

        // Log Out is always enabled; Open is disabled with nothing selected.
        let row_of = |menu: &Menu, label: &str| {
            menu.entries
                .iter()
                .position(|entry| entry.label == label)
                .expect("row")
        };

        let mut fonts = Fonts::load().expect("fonts");
        let mut menu = Menu::new(Point::new(100, 100), 0, None, |label| {
            fonts.width(Face::Bold, crate::menu::LABEL_PX, label)
        });
        let log_out = row_of(&menu, "Log Out");
        let row = menu.row(log_out);
        menu.hover(Point::new(row.x + row.w / 2, row.y + row.h / 2));

        let mut images = Icons::default();
        let mut pixels = vec![0u8; 1280 * 800 * 4];
        {
            let mut canvas = Canvas::new(&mut pixels, 1280, 800);
            desktop(
                &mut canvas,
                &mut Frame {
                    palette: wlrix_ui::palette::DEFAULT,
                    fonts: &mut fonts,
                    images: &mut images,
                    placed: &placed,
                    selection: &Selection::default(),
                    running: &Running::default(),
                    menu: Some(&menu),
                    icon_size: 64,
                },
            );
        }
        let canvas = Canvas::new(&mut pixels, 1280, 800);

        // The hovered row wears the highlight; an untouched one keeps the panel face.
        let inside = |index: usize| {
            let row = menu.row(index);
            // Just inside the bevel, and clear of the label text on the left.
            (row.x + row.w - 6, row.y + row.h / 2)
        };
        let (x, y) = inside(log_out);
        assert_eq!(canvas.get(x, y), wlrix_ui::palette::DEFAULT.title_active);

        let open = row_of(&menu, "Open");
        let (x, y) = inside(open);
        assert_eq!(
            canvas.get(x, y),
            wlrix_ui::palette::DEFAULT.face,
            "a disabled row must not highlight"
        );
    }

    #[test]
    fn a_link_entry_keeps_its_drawn_icon() {
        // Only Type=Application has a running state, so a bookmark gets no carpet.
        if Fonts::load().is_err() {
            return;
        }
        let grid = grid();
        let link = vec![Placed {
            entry: Entry {
                path: "/desktop/site.desktop".into(),
                name: "site.desktop".to_owned(),
                label: "site.desktop".to_owned(),
                kind: Kind::Launcher,
                icon_names: Vec::new(),
                launcher: Some(crate::entries::Launcher {
                    is_application: false,
                    actions: Vec::new(),
                    icon: None,
                    running_icon: None,
                    identities: vec!["site".to_owned()],
                }),
            },
            spot: Spot::Slot(0),
            rect: grid.slot_rect(0),
        }];
        // Nothing can make it look running, so the two paints match.
        assert_eq!(
            paint_launcher(&link, &Running::default()),
            paint_launcher(&link, &running_with("site")),
        );
    }

    #[test]
    fn selection_beats_hover() {
        // A selected icon the pointer happens to be over must stay yellow, not flick to
        // white as the pointer crosses it.
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        selection.hover(
            &placed,
            Point::new(placed[0].rect.x + 4, placed[0].rect.y + 4),
        );
        selection.press(
            &placed,
            Point::new(placed[0].rect.x + 4, placed[0].rect.y + 4),
            0,
        );
        assert_eq!(tint_for(&selection, "a"), IconState::Selected);
    }

    #[test]
    fn an_untouched_icon_is_plain() {
        let selection = Selection::default();
        assert_eq!(tint_for(&selection, "a"), IconState::Normal);
    }

    #[test]
    fn the_default_cell_is_tall_enough_for_the_lines_it_promises() {
        // The bug this catches: a cell a few pixels too short fits only one line, so `wrap`
        // produces a second that is silently dropped and every long name reads as cut.
        let Some(mut fonts) = fonts() else { return };
        let metrics = Metrics::default();
        let label_height = metrics.cell_h - metrics.icon - LABEL_GAP;
        let line_height = fonts.line_height(Face::Regular, LABEL_PX);
        assert!(
            label_height >= line_height * LABEL_LINES as i32,
            "{label_height}px of label room fits {} lines, not {LABEL_LINES}",
            label_height / line_height.max(1)
        );
    }

    #[test]
    fn a_short_name_stays_on_one_line() {
        let Some(mut fonts) = fonts() else { return };
        assert_eq!(
            wrap(&mut fonts, "a.txt", 200, LABEL_LINES),
            vec!["a.txt".to_owned()]
        );
    }

    #[test]
    fn a_name_too_long_for_one_line_is_cut_with_an_ellipsis() {
        // When only one line fits, the label must say it was cut rather than just stopping.
        let Some(mut fonts) = fonts() else { return };
        let lines = wrap(&mut fonts, "スクリーンショット.png", 80, 1);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].ends_with('…'),
            "a one-line cut should be marked: {lines:?}"
        );
        assert!(fonts.width(Face::Regular, LABEL_PX, &lines[0]) <= 80);
    }

    #[test]
    fn a_name_with_words_in_it_breaks_between_them() {
        // What a launcher's `Name` looks like, now that labels are not only filenames.
        let Some(mut fonts) = fonts() else { return };
        let width = fonts.width(Face::Regular, LABEL_PX, "mpv Media Play");
        let lines = wrap(&mut fonts, "mpv Media Player", width, LABEL_LINES);
        assert_eq!(lines, ["mpv Media", "Player"], "broke mid-word: {lines:?}");
    }

    #[test]
    fn a_name_with_no_spaces_still_breaks_wherever_it_has_to() {
        // Filenames and CJK have no word boundary to find, so the character break stays.
        let Some(mut fonts) = fonts() else { return };
        let name = "mpvメディアプレイヤー";
        let width = fonts.width(Face::Regular, LABEL_PX, "mpvメディア");
        let lines = wrap(&mut fonts, name, width, LABEL_LINES);
        assert!(lines.len() > 1, "should have wrapped: {lines:?}");
        assert_eq!(lines.concat(), name, "lost text: {lines:?}");
    }

    #[test]
    fn a_word_break_is_given_up_rather_than_lose_the_end_of_the_name() {
        // The real case: "mpv メディアプレイヤー" breaks after "mpv" if space breaks are taken
        // greedily, which leaves the whole Japanese run for one line that cannot hold it. The
        // mid-run break is uglier and shows the entire name, which matters more.
        let Some(mut fonts) = fonts() else { return };
        let name = "mpv メディアプレイヤー";
        let width = fonts.width(Face::Regular, LABEL_PX, "mpv メディア");
        let lines = wrap(&mut fonts, name, width, LABEL_LINES);
        assert!(
            !lines.last().is_some_and(|line| line.ends_with('…')),
            "gave up the end of the name to keep the words tidy: {lines:?}"
        );
        // The mid-run break keeps every character, space included, so this is the whole name.
        assert_eq!(lines.concat(), name);
    }

    #[test]
    fn a_word_too_long_for_a_line_is_still_broken() {
        // The space break must not become the *only* break: one unbreakable word wider than
        // the cell would otherwise overflow rather than wrap.
        let Some(mut fonts) = fonts() else { return };
        let lines = wrap(
            &mut fonts,
            &format!("a {}", "x".repeat(60)),
            60,
            LABEL_LINES,
        );
        for line in &lines {
            assert!(
                fonts.width(Face::Regular, LABEL_PX, line) <= 60,
                "{line:?} is wider than the cell"
            );
        }
    }

    #[test]
    fn a_long_name_wraps_and_is_cut() {
        let Some(mut fonts) = fonts() else { return };
        let lines = wrap(&mut fonts, &"x".repeat(200), 80, LABEL_LINES);
        assert!(
            lines.len() <= LABEL_LINES,
            "wrapped to {} lines",
            lines.len()
        );
        assert!(
            lines.last().is_some_and(|line| line.ends_with('…')),
            "a cut label should say so: {lines:?}"
        );
        for line in &lines {
            assert!(
                fonts.width(Face::Regular, LABEL_PX, line) <= 80,
                "line overflows its cell: {line:?}"
            );
        }
    }

    #[test]
    fn a_multibyte_name_is_never_split_mid_character() {
        // The dev machine runs a Japanese locale, so this is the ordinary case, not an edge.
        let Some(mut fonts) = fonts() else { return };
        let lines = wrap(
            &mut fonts,
            "スクリーンショット-2026-07-30.png",
            60,
            LABEL_LINES,
        );
        // Reassembling must give back a prefix of the original: if a char had been split,
        // the string would not even be valid UTF-8 to begin with.
        for line in &lines {
            assert!(line.chars().count() > 0);
        }
    }

    #[test]
    fn painting_fills_the_background_and_draws_something() {
        let Some(mut fonts) = fonts() else { return };
        let mut pixels = vec![0u8; 1280 * 800 * 4];
        let mut canvas = Canvas::new(&mut pixels, 1280, 800);
        let placed = placed(&["a.txt", "b.txt"]);
        desktop(
            &mut canvas,
            &mut frame(&mut fonts, &placed, &Selection::default()),
        );

        // Bottom-left is bare desktop, and bare desktop is *transparent* -- see below.
        assert_eq!(canvas.get(4, 796), Rgb(0));
        // Something was drawn in the first cell.
        let cell = placed[0].rect;
        assert!(painted(&canvas, cell) > 0, "the first icon drew nothing");
    }

    #[test]
    fn bare_desktop_is_left_transparent_so_a_wallpaper_shows_through() {
        // The regression: this surface used to be cleared to an opaque gray, which hid
        // whatever `wlrix-bg` had put on the layer below. Everywhere an icon is not drawn must
        // stay fully transparent.
        let Some(mut fonts) = fonts() else { return };
        let mut pixels = vec![0xffu8; 1280 * 800 * 4];
        let mut canvas = Canvas::new(&mut pixels, 1280, 800);
        let placed = placed(&["a.txt"]);
        desktop(
            &mut canvas,
            &mut frame(&mut fonts, &placed, &Selection::default()),
        );

        // Well away from the one icon, in all four corners of the surface.
        for (x, y) in [(0, 0), (0, 799), (4, 400), (600, 796)] {
            assert_eq!(
                canvas.get(x, y),
                Rgb(0),
                "({x},{y}) should be transparent, not painted over the wallpaper"
            );
        }
        // And the icon itself is still opaque where it is solid.
        assert!(painted(&canvas, placed[0].rect) > 0);
    }

    #[test]
    fn a_glyph_edge_over_bare_desktop_stays_partly_transparent() {
        // Premultiplied source-over, not the old interpolation: an antialiased edge drawn on
        // nothing must come out partly transparent, not blended against opaque black.
        let mut pixels = vec![0u8; 4];
        let mut canvas = Canvas::new(&mut pixels, 1, 1);
        canvas.clear_transparent();
        canvas.blend(0, 0, Rgb(0xff_ffffff), 128);

        let pixel = canvas.get(0, 0).0;
        let alpha = (pixel >> 24) as u8;
        assert!(
            (127..=129).contains(&alpha),
            "half coverage should give about half alpha, got {alpha}"
        );
        // Premultiplied: no channel may exceed the alpha.
        for shift in [16, 8, 0] {
            let channel = ((pixel >> shift) & 0xff) as u8;
            assert!(channel <= alpha, "channel {channel} exceeds alpha {alpha}");
        }
    }

    /// How many pixels inside `rect` were painted at all.
    fn painted(canvas: &Canvas, rect: LayoutRect) -> usize {
        let mut count = 0;
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                if canvas.get(x, y) != Rgb(0) {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn a_dragged_icon_is_drawn_under_the_pointer_and_leaves_a_ghost() {
        let Some(mut fonts) = fonts() else { return };
        let placed = placed(&["a.txt"]);
        let mut selection = Selection::default();
        let start = Point::new(placed[0].rect.x + 8, placed[0].rect.y + 8);
        selection.press(&placed, start, 0);
        selection.motion(&placed, Point::new(start.x - 300, start.y + 200));

        let mut pixels = vec![0u8; 1280 * 800 * 4];
        let mut canvas = Canvas::new(&mut pixels, 1280, 800);
        desktop(&mut canvas, &mut frame(&mut fonts, &placed, &selection));

        // The icon follows the pointer...
        let moved = LayoutRect::new(
            placed[0].rect.x - 300,
            placed[0].rect.y + 200,
            placed[0].rect.w,
            placed[0].rect.h,
        );
        assert!(
            painted(&canvas, moved) > 0,
            "the icon should follow the pointer"
        );
        // ...and leaves its ghost behind, as IRIX did, until it is dropped.
        assert!(
            painted(&canvas, placed[0].rect) > 0,
            "the cell it came from should hold its ghost"
        );
    }

    #[test]
    fn the_drag_image_holds_the_dragged_icons_and_nothing_else() {
        let Some(mut fonts) = fonts() else { return };
        let placed = placed(&["a.txt", "b.txt", "c.txt"]);
        let mut selection = Selection::default();
        // Band the first two, then drag them.
        selection.press(&placed, Point::new(40, 40), 0);
        selection.motion(
            &placed,
            Point::new(
                placed[1].rect.x + 1,
                placed[1].rect.y + placed[1].rect.h - 1,
            ),
        );
        selection.release(&placed, &grid(), false);
        let start = Point::new(placed[0].rect.x + 8, placed[0].rect.y + 8);
        selection.press(&placed, start, 10_000);
        selection.motion(&placed, Point::new(start.x - 200, start.y));

        let bounds = drag_bounds(&placed, &selection).expect("two icons are being dragged");
        assert_eq!(bounds.w, placed[0].rect.w);
        assert_eq!(
            bounds.h,
            placed[1].rect.y + placed[1].rect.h - placed[0].rect.y
        );

        let mut pixels = vec![0u8; (bounds.w * bounds.h * 4) as usize];
        let mut canvas = Canvas::new(&mut pixels, bounds.w, bounds.h);
        drag_image(
            &mut canvas,
            &mut frame(&mut fonts, &placed, &selection),
            bounds,
        );
        let top = LayoutRect::new(0, 0, bounds.w, placed[0].rect.h);
        let bottom = LayoutRect::new(0, placed[1].rect.y - bounds.y, bounds.w, placed[1].rect.h);
        assert!(painted(&canvas, top) > 0);
        assert!(painted(&canvas, bottom) > 0);
    }

    #[test]
    fn nothing_dragged_has_no_drag_image() {
        let placed = placed(&["a.txt"]);
        assert_eq!(drag_bounds(&placed, &Selection::default()), None);
    }

    /// How many pixels in `rect` are `color`.
    fn pixels_of(canvas: &Canvas, rect: LayoutRect, color: Rgb) -> usize {
        (rect.y..rect.y + rect.h)
            .flat_map(|y| (rect.x..rect.x + rect.w).map(move |x| (x, y)))
            .filter(|&(x, y)| canvas.get(x, y) == color)
            .count()
    }

    #[test]
    fn the_dumpster_turns_blue_under_a_drag_and_the_ghost_dark_gray() {
        let Some(mut fonts) = fonts() else { return };
        let mut placed = placed(&["a.txt", "trash"]);
        // The second cell becomes the Dumpster, drawn from the code-drawn bin: no theme names,
        // so the test does not depend on what this machine has installed.
        placed[1].entry.kind = Kind::Dumpster;
        placed[1].entry.name = crate::entries::DUMPSTER.to_owned();
        let palette = wlrix_ui::palette::resolve(None).0;

        let mut selection = Selection::default();
        let start = Point::new(placed[0].rect.x + 8, placed[0].rect.y + 8);
        selection.press(&placed, start, 0);
        // Onto the Dumpster's cell.
        let over = Point::new(placed[1].rect.x + 8, placed[1].rect.y + 8);
        selection.motion(&placed, over);

        let mut pixels = vec![0u8; 1280 * 800 * 4];
        let mut canvas = Canvas::new(&mut pixels, 1280, 800);
        desktop(&mut canvas, &mut frame(&mut fonts, &placed, &selection));

        assert!(
            pixels_of(&canvas, placed[1].rect, palette.icon_tint_drop) > 0,
            "the Dumpster should be royal blue while it would take the drop"
        );
        assert!(
            pixels_of(&canvas, placed[0].rect, palette.icon_tint_ghost) > 0,
            "the dragged icon should leave a dark gray ghost"
        );
    }

    #[test]
    fn an_irix_symbol_is_tinted_with_its_carpet_and_an_ordinary_icon_is_not() {
        let Some(mut fonts) = fonts() else { return };
        let dir = std::env::temp_dir().join(format!("wlrix-desktop-tint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A white body in each: an IRIX drawing in the carpet's frame, without the namespace as
        // exported, and an ordinary 48px icon.
        let irix = dir.join("irix.svg");
        std::fs::write(
            &irix,
            r##"<svg width="50pt" height="50pt" viewBox="0 0 50 50"><g transform="scale(0.5)"><rect x="0" y="0" width="12" height="12" fill="#fff"/></g></svg>"##,
        )
        .unwrap();
        let plain = dir.join("plain.svg");
        std::fs::write(
            &plain,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48"><rect x="18" y="0" width="12" height="12" fill="#fff"/></svg>"##,
        )
        .unwrap();

        let grid = grid();
        let mut placed = launcher("irix", irix.to_str(), None);
        placed.extend(launcher("plain", plain.to_str(), None));
        placed[1].entry.name = "plain".to_owned();
        placed[1].rect = grid.slot_rect(1);
        let mut selection = Selection::default();
        selection.select_all(vec!["irix".to_owned(), "plain".to_owned()]);

        let mut pixels = vec![0u8; 1280 * 800 * 4];
        let mut canvas = Canvas::new(&mut pixels, 1280, 800);
        desktop(&mut canvas, &mut frame(&mut fonts, &placed, &selection));
        let palette = wlrix_ui::palette::DEFAULT;

        // The IRIX body sits in the icon square's top-left corner, and is selection yellow.
        let square = |rect: LayoutRect| LayoutRect::new(rect.x + (rect.w - 64) / 2, rect.y, 64, 64);
        let corner = square(placed[0].rect);
        assert!(
            pixels_of(
                &canvas,
                LayoutRect::new(corner.x, corner.y, 6, 6),
                palette.icon_tint_selected
            ) > 0,
            "the IRIX symbol should be tinted"
        );
        // The ordinary one, shrunk onto the carpet, keeps its white.
        let top = square(placed[1].rect);
        assert!(
            pixels_of(
                &canvas,
                LayoutRect::new(top.x + 24, top.y + 4, 16, 12),
                Rgb(0xffff_ffff)
            ) > 0,
            "the ordinary icon should keep its own white"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
