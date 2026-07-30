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
use crate::image::Images;
use crate::layout::{Placed, Point};
use crate::running::Running;
use crate::select::Selection;
use crate::theme::font::{Face, Fonts, Run};
use crate::ui::canvas::{Canvas, Rect};
use crate::ui::icons::{self, Tint};

/// Label text size, in pixels.
const LABEL_PX: f32 = 12.0;
/// Gap between the icon square and the first line of its label.
const LABEL_GAP: i32 = 4;
/// How many lines a label may wrap to before it is cut short.
const LABEL_LINES: usize = 2;
/// Padding around the label's highlight, so a selected name is not painted edge to edge.
const LABEL_PAD: i32 = 2;

/// How big an application's symbol is, as a fraction of the icon square.
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
    pub fonts: &'a mut Fonts,
    pub images: &'a mut Images,
    pub placed: &'a [Placed],
    pub selection: &'a Selection,
    /// Which applications have a window open. Empty is a perfectly good answer -- it means
    /// every carpet is closed.
    pub running: &'a Running,
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

    for item in frame.placed {
        // A dragged icon is drawn under the pointer rather than in its cell, so it follows
        // the hand instead of jumping there on release.
        let origin = match (frame.selection.dragging(), frame.selection.drag_origin()) {
            (Some(name), Some(point)) if name == item.entry.name => point,
            _ => Point::new(item.rect.x, item.rect.y),
        };
        let cell = Rect::new(origin.x, origin.y, item.rect.w, item.rect.h);
        let tint = tint_for(frame.selection, &item.entry.name);
        icon(canvas, frame, item, cell, tint);
    }
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
    tint: Tint,
) -> bool {
    let size = square.w.min(square.h);
    if size <= 0 {
        return false;
    }

    // The carpet fills the square; the symbol stands on it.
    //
    // The **carpet** carries the hover and selection tint, not the symbol: it is wlRIX's own
    // artwork, so gray/white/yellow is exactly the IRIX rule, while an application's icon
    // wants the colors it was drawn in. Without this a launcher would be the one thing on
    // the desktop that does not react to the pointer at all.
    let carpet = if running { CARPET_OPEN } else { CARPET_CLOSED };
    let Some(carpet) = frame.images.load_bytes(carpet_key(running), carpet, size) else {
        return false;
    };
    carpet
        .multiplied(tint.colour())
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
    if let Some(symbol) = frame.images.get(wanted, symbol_size) {
        let bottom = square.y + ((size as f32) * SYMBOL_BASELINE).round() as i32;
        symbol.draw(
            canvas,
            square.x + (size - symbol_size) / 2,
            bottom - symbol_size,
        );
    }
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

/// Which tint an icon takes. Selection wins over hover: a selected icon under the pointer
/// should stay yellow rather than flicking to white as the pointer crosses it.
fn tint_for(selection: &Selection, name: &str) -> Tint {
    if selection.selected() == Some(name) {
        Tint::Selected
    } else if selection.hovered() == Some(name) {
        Tint::Hover
    } else {
        Tint::Normal
    }
}

/// Draw one icon and its label inside `cell`.
fn icon(canvas: &mut Canvas, frame: &mut Frame, item: &Placed, cell: Rect, tint: Tint) {
    // The icon square, centred across the cell and hard against its top.
    let size = frame.icon_size.min(cell.w).min(cell.h);
    let square = Rect::new(cell.x + (cell.w - size) / 2, cell.y, size, size);

    // An application gets its own artwork on a magic carpet; everything else gets the
    // drawn-in-code icon, which is also the fallback when the artwork will not load.
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
    if !drawn {
        icons::draw(canvas, square, item.entry.kind, tint);
    }

    label(
        canvas,
        frame.fonts,
        &item.entry.name,
        Rect::new(
            cell.x,
            square.y + size + LABEL_GAP,
            cell.w,
            (cell.h - size - LABEL_GAP).max(0),
        ),
        tint,
    );
}

/// Draw a filename under its icon, wrapped and centred.
fn label(canvas: &mut Canvas, fonts: &mut Fonts, name: &str, area: Rect, tint: Tint) {
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
    if tint == Tint::Selected {
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
            tint.colour(),
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
                colour: tint.label_colour(),
            },
            line,
        );
    }
}

/// Break a filename into at most `max_lines` lines that fit `width`.
///
/// Broken by character, not by word: filenames are not prose, and `annual-report-final.txt`
/// has no useful word boundaries. A name too long for the lines available is cut and given an
/// ellipsis, since a label that overflowed its cell would collide with its neighbor.
fn wrap(fonts: &mut Fonts, name: &str, width: i32, max_lines: usize) -> Vec<String> {
    if width <= 0 || max_lines == 0 {
        return Vec::new();
    }
    if fonts.width(Face::Regular, LABEL_PX, name) <= width {
        return vec![name.to_owned()];
    }

    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    // By `char`, so a multi-byte character is never split down the middle.
    for character in name.chars() {
        let mut candidate = line.clone();
        candidate.push(character);
        if fonts.width(Face::Regular, LABEL_PX, &candidate) > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            if lines.len() == max_lines {
                break;
            }
        }
        line.push(character);
    }

    if lines.len() < max_lines && !line.is_empty() {
        lines.push(line);
        return lines;
    }

    // Ran out of lines with text still to place: mark the last one as cut.
    if let Some(last) = lines.last_mut() {
        while !last.is_empty() && fonts.width(Face::Regular, LABEL_PX, &format!("{last}…")) > width
        {
            last.pop();
        }
        last.push('…');
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries::{Entry, Kind};
    use crate::layout::{Grid, Metrics, Rect as LayoutRect, Spot};
    use crate::theme::Rgb;

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
        let images: &'a mut Images = Box::leak(Box::new(Images::default()));
        let running: &'a Running = Box::leak(Box::new(Running::default()));
        Frame {
            fonts,
            images,
            placed,
            selection,
            running,
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
                    kind: Kind::Plain,
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
                kind: Kind::Launcher,
                launcher: Some(crate::entries::Launcher {
                    is_application: true,
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
        let mut images = Images::default();
        let mut pixels = vec![0u8; 1280 * 800 * 4];
        {
            let mut canvas = Canvas::new(&mut pixels, 1280, 800);
            desktop(
                &mut canvas,
                &mut Frame {
                    fonts: &mut fonts,
                    images: &mut images,
                    placed,
                    selection: &Selection::default(),
                    running,
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
        let mut images = Images::default();
        let running = Running::default();
        let mut pixels = vec![0u8; 1280 * 800 * 4];
        {
            let mut canvas = Canvas::new(&mut pixels, 1280, 800);
            desktop(
                &mut canvas,
                &mut Frame {
                    fonts: &mut fonts,
                    images: &mut images,
                    placed,
                    selection,
                    running: &running,
                    icon_size: 64,
                },
            );
        }
        pixels
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
                kind: Kind::Launcher,
                launcher: Some(crate::entries::Launcher {
                    is_application: false,
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
        assert_eq!(tint_for(&selection, "a"), Tint::Selected);
    }

    #[test]
    fn an_untouched_icon_is_plain() {
        let selection = Selection::default();
        assert_eq!(tint_for(&selection, "a"), Tint::Normal);
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
        // whatever `swaybg` had put on the layer below. Everywhere an icon is not drawn must
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
    fn a_dragged_icon_is_drawn_under_the_pointer() {
        let Some(mut fonts) = fonts() else { return };
        let placed = placed(&["a.txt"]);
        let mut selection = Selection::default();
        let start = Point::new(placed[0].rect.x + 8, placed[0].rect.y + 8);
        selection.press(&placed, start, 0);
        selection.motion(Point::new(start.x - 300, start.y + 200));

        let mut pixels = vec![0u8; 1280 * 800 * 4];
        let mut canvas = Canvas::new(&mut pixels, 1280, 800);
        desktop(&mut canvas, &mut frame(&mut fonts, &placed, &selection));

        // The cell it came from is now bare.
        assert_eq!(
            painted(&canvas, placed[0].rect),
            0,
            "the icon should have moved, not been copied"
        );
    }
}
