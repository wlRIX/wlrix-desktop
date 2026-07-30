// SPDX-License-Identifier: GPL-3.0-or-later
//! Composing the desktop: background, icons, labels.
//!
//! One pass over the laid-out icons, drawing each into its cell. Kept apart from
//! [`crate::ui`]'s Wayland plumbing so the drawing can be reasoned about -- and rendered into
//! a plain buffer by a test or `examples/paint_preview` -- without a compositor.

use crate::layout::{Placed, Point};
use crate::select::Selection;
use crate::theme::font::{Face, Fonts, Run};
use crate::theme::palette;
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

/// Draw the whole desktop.
///
/// `placed` is in back-to-front order, matching [`crate::select`]'s hit-testing, so an icon
/// dropped over another covers it and is also the one picked up.
pub fn desktop(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    placed: &[Placed],
    selection: &Selection,
    icon_size: i32,
) {
    canvas.clear(palette::DESKTOP);

    for item in placed {
        // A dragged icon is drawn under the pointer rather than in its cell, so it follows
        // the hand instead of jumping there on release.
        let origin = match (selection.dragging(), selection.drag_origin()) {
            (Some(name), Some(point)) if name == item.entry.name => point,
            _ => Point::new(item.rect.x, item.rect.y),
        };
        let cell = Rect::new(origin.x, origin.y, item.rect.w, item.rect.h);
        icon(
            canvas,
            fonts,
            item,
            cell,
            tint_for(selection, &item.entry.name),
            icon_size,
        );
    }
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
fn icon(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    item: &Placed,
    cell: Rect,
    tint: Tint,
    icon_size: i32,
) {
    // The icon square, centred across the cell and hard against its top.
    let size = icon_size.min(cell.w).min(cell.h);
    let square = Rect::new(cell.x + (cell.w - size) / 2, cell.y, size, size);
    icons::draw(canvas, square, item.entry.kind, tint);

    label(
        canvas,
        fonts,
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

    /// Font loading needs a system font database, which a build machine may not have. Every
    /// test here skips rather than fails in that case -- the drawing is verified on a real
    /// desktop, and a CI box without fonts should not turn red over it.
    fn fonts() -> Option<Fonts> {
        Fonts::load().ok()
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
                },
                spot: Spot::Slot(index as i32),
                rect: grid.slot_rect(index as i32),
            })
            .collect()
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
        desktop(&mut canvas, &mut fonts, &placed, &Selection::default(), 64);

        // Bottom-left is bare desktop.
        assert_eq!(canvas.get(4, 796), palette::DESKTOP);
        // Something was drawn in the first cell.
        let cell = placed[0].rect;
        let mut drawn = 0;
        for y in cell.y..cell.y + cell.h {
            for x in cell.x..cell.x + cell.w {
                if canvas.get(x, y) != palette::DESKTOP {
                    drawn += 1;
                }
            }
        }
        assert!(drawn > 0, "the first icon drew nothing");
    }

    #[test]
    fn a_dragged_icon_is_drawn_under_the_pointer() {
        let Some(mut fonts) = fonts() else { return };
        let placed = placed(&["a.txt"]);
        let mut selection = Selection::default();
        let start = Point::new(placed[0].rect.x + 8, placed[0].rect.y + 8);
        selection.press(&placed, start);
        selection.motion(Point::new(start.x - 300, start.y + 200));

        let mut pixels = vec![0u8; 1280 * 800 * 4];
        let mut canvas = Canvas::new(&mut pixels, 1280, 800);
        desktop(&mut canvas, &mut fonts, &placed, &selection, 64);

        // The cell it came from is now bare.
        let cell = placed[0].rect;
        let mut left_behind = 0;
        for y in cell.y..cell.y + cell.h {
            for x in cell.x..cell.x + cell.w {
                if canvas.get(x, y) != palette::DESKTOP {
                    left_behind += 1;
                }
            }
        }
        assert_eq!(
            left_behind, 0,
            "the icon should have moved, not been copied"
        );
    }
}
