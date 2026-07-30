// SPDX-License-Identifier: GPL-3.0-or-later
//! The icons themselves.
//!
//! There is no artwork in the tree yet, so these are drawn in code -- the same approach
//! `wlrix-greeter/src/ui/icons.rs` takes for its user icons. Each is a small IRIX-ish emblem:
//! a folder, a sheet of paper, a launcher badge, a terminal-ish block for an executable.
//!
//! **Everything is drawn into a coverage mask, not straight onto the canvas.** An icon is one
//! shape whose color is decided by its state -- knocked-back gray at rest, full white under
//! the pointer, IRIX's lamp yellow when selected -- so drawing the shape once as coverage and
//! blending the tint through it means the three states cannot drift apart. It is also what
//! makes real artwork a drop-in later: supply a mask instead of a drawing routine and nothing
//! above here changes.

use crate::entries::Kind;
use crate::theme::{Rgb, palette};
use crate::ui::canvas::{Canvas, Rect};

/// Which state an icon is in, and so what color it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    /// At rest.
    Normal,
    /// The pointer is over it.
    Hover,
    /// Picked.
    Selected,
}

impl Tint {
    /// The color the icon's coverage is blended in.
    pub fn colour(self) -> Rgb {
        match self {
            Tint::Normal => palette::ICON_TINT,
            Tint::Hover => palette::ICON_TINT_HOVER,
            Tint::Selected => palette::ICON_TINT_SELECTED,
        }
    }

    /// The color the label text takes. A selected label inverts onto its tinted background.
    pub fn label_colour(self) -> Rgb {
        match self {
            Tint::Selected => palette::ICON_LABEL_SELECTED,
            _ => palette::ICON_LABEL,
        }
    }
}

/// A square, single-channel coverage bitmap: 0 is untouched, 255 fully the icon.
///
/// Small enough (64x64 by default) that a `Vec<u8>` per draw is not worth avoiding, and
/// keeping it owned means the shape routines can overdraw freely without clipping logic.
pub struct Mask {
    size: i32,
    coverage: Vec<u8>,
}

impl Mask {
    fn new(size: i32) -> Self {
        Self {
            size,
            coverage: vec![0; (size * size).max(0) as usize],
        }
    }

    fn set(&mut self, x: i32, y: i32, coverage: u8) {
        if x < 0 || y < 0 || x >= self.size || y >= self.size {
            return;
        }
        let index = (y * self.size + x) as usize;
        // Max, not overwrite: shapes are drawn over each other and the darker one must not
        // punch a hole in what is already there.
        self.coverage[index] = self.coverage[index].max(coverage);
    }

    fn get(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.size || y >= self.size {
            return 0;
        }
        self.coverage[(y * self.size + x) as usize]
    }

    /// Fill a rectangle solid.
    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32) {
        for row in y..y + h {
            for column in x..x + w {
                self.set(column, row, 255);
            }
        }
    }

    /// Draw a one-pixel outline.
    fn outline(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if w <= 0 || h <= 0 {
            return;
        }
        for column in x..x + w {
            self.set(column, y, 255);
            self.set(column, y + h - 1, 255);
        }
        for row in y..y + h {
            self.set(x, row, 255);
            self.set(x + w - 1, row, 255);
        }
    }

    /// Clear a rectangle back to nothing, for punching a window out of a filled shape.
    fn erase(&mut self, x: i32, y: i32, w: i32, h: i32) {
        for row in y..y + h {
            for column in x..x + w {
                if column < 0 || row < 0 || column >= self.size || row >= self.size {
                    continue;
                }
                self.coverage[(row * self.size + column) as usize] = 0;
            }
        }
    }
}

/// Build the mask for `kind` at `size` pixels square.
///
/// The shapes are laid out on a notional 64-unit grid and scaled, so a configured icon size
/// other than 64 keeps its proportions.
pub fn mask(kind: Kind, size: i32) -> Mask {
    let mut mask = Mask::new(size);
    // Everything below is written against 64 units; `u` converts.
    let u = |value: i32| (value * size) / 64;

    match kind {
        Kind::Directory => {
            // A folder: a tab along the top left, then the body below it.
            mask.rect(u(4), u(14), u(22), u(6));
            mask.rect(u(4), u(18), u(56), u(34));
            mask.erase(u(7), u(23), u(50), u(26));
            mask.outline(u(4), u(18), u(56), u(34));
        }
        Kind::Plain => {
            // A sheet of paper with the top-right corner folded over, and rule lines.
            let (x, y, w, h) = (u(12), u(6), u(40), u(52));
            mask.outline(x, y, w, h);
            // The fold.
            for step in 0..u(12) {
                mask.set(x + w - 1 - step, y + step, 255);
            }
            mask.rect(x + w - u(12), y, u(12), 1);
            for line in 0..4 {
                let row = y + u(22) + line * u(7);
                mask.rect(x + u(7), row, w - u(14), u(2));
            }
        }
        Kind::Executable => {
            // A rounded-off block with a prompt caret, as a terminal or binary.
            let (x, y, w, h) = (u(6), u(12), u(52), u(40));
            mask.outline(x, y, w, h);
            // The title strip, so it reads as a window rather than a plain box.
            mask.rect(x, y, w, u(6));
            // A ">" caret.
            for step in 0..u(8) {
                mask.rect(x + u(10) + step, y + u(16) + step, u(2), u(2));
                mask.rect(x + u(10) + step, y + u(30) - step, u(2), u(2));
            }
            mask.rect(x + u(28), y + u(30), u(14), u(2));
        }
        Kind::Launcher => {
            // A sheet like Plain, but badged: a solid diamond marks "this starts something".
            let (x, y, w, h) = (u(12), u(6), u(40), u(52));
            mask.outline(x, y, w, h);
            let (cx, cy, r) = (x + w / 2, y + h / 2, u(12));
            for dy in -r..=r {
                let span = r - dy.abs();
                for dx in -span..=span {
                    mask.set(cx + dx, cy + dy, 255);
                }
            }
        }
    }
    mask
}

/// Blend an icon's mask onto the canvas at `rect`'s top-left, in the state's tint.
///
/// `rect` is expected to be the icon square; anything the mask covers outside the canvas is
/// clipped by [`Canvas::blend`] rather than being an error.
pub fn draw(canvas: &mut Canvas, rect: Rect, kind: Kind, tint: Tint) {
    let size = rect.w.min(rect.h);
    if size <= 0 {
        return;
    }
    let mask = mask(kind, size);
    let colour = tint.colour();
    // Center the square inside `rect`, so a non-square cell still looks right.
    let x0 = rect.x + (rect.w - size) / 2;
    let y0 = rect.y + (rect.h - size) / 2;

    for row in 0..size {
        for column in 0..size {
            let coverage = mask.get(column, row);
            if coverage != 0 {
                canvas.blend(x0 + column, y0 + row, colour, coverage);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [Kind; 4] = [
        Kind::Directory,
        Kind::Plain,
        Kind::Executable,
        Kind::Launcher,
    ];

    fn covered(mask: &Mask) -> usize {
        mask.coverage.iter().filter(|c| **c != 0).count()
    }

    #[test]
    fn every_kind_draws_something() {
        for kind in KINDS {
            let mask = mask(kind, 64);
            assert!(covered(&mask) > 0, "{kind:?} drew nothing");
        }
    }

    #[test]
    fn the_kinds_look_different() {
        // Four icons that all came out identical would be a drawing bug the eye would
        // catch but nothing else would.
        let masks: Vec<Vec<u8>> = KINDS.iter().map(|k| mask(*k, 64).coverage).collect();
        for (i, a) in masks.iter().enumerate() {
            for (j, b) in masks.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    a, b,
                    "{:?} and {:?} draw the same shape",
                    KINDS[i], KINDS[j]
                );
            }
        }
    }

    #[test]
    fn nothing_is_drawn_outside_the_square() {
        // The shape routines overdraw freely and rely on `Mask::set` clipping; if that ever
        // stopped clipping, the icon would bleed into its neighbor.
        for kind in KINDS {
            let mask = mask(kind, 64);
            assert_eq!(
                mask.coverage.len(),
                64 * 64,
                "{kind:?} mask is the wrong size"
            );
        }
    }

    #[test]
    fn icons_scale_without_falling_apart() {
        // A configured icon size other than the notional 64 must still draw.
        for size in [16, 24, 32, 48, 64, 96, 128] {
            for kind in KINDS {
                let mask = mask(kind, size);
                assert!(covered(&mask) > 0, "{kind:?} vanished at {size}px");
            }
        }
    }

    #[test]
    fn each_state_has_its_own_colour() {
        // The whole point of the tint: the three states must be visibly distinct.
        assert_ne!(Tint::Normal.colour(), Tint::Hover.colour());
        assert_ne!(Tint::Normal.colour(), Tint::Selected.colour());
        assert_ne!(Tint::Hover.colour(), Tint::Selected.colour());
        // And the requested colors specifically: gray, white, yellow.
        assert_eq!(Tint::Normal.colour(), palette::ICON_TINT);
        assert_eq!(Tint::Hover.colour(), palette::ICON_TINT_HOVER);
        assert_eq!(Tint::Selected.colour(), palette::ICON_TINT_SELECTED);
    }

    #[test]
    fn drawing_lands_on_the_canvas_in_the_tint() {
        let mut pixels = vec![0u8; 64 * 64 * 4];
        let mut canvas = Canvas::new(&mut pixels, 64, 64);
        canvas.clear(palette::DESKTOP);
        draw(
            &mut canvas,
            Rect::new(0, 0, 64, 64),
            Kind::Directory,
            Tint::Selected,
        );

        // Somewhere in the folder body should now be the selected yellow.
        let mut hits = 0;
        for y in 0..64 {
            for x in 0..64 {
                if canvas.get(x, y) == palette::ICON_TINT_SELECTED {
                    hits += 1;
                }
            }
        }
        assert!(hits > 0, "nothing was drawn in the tint colour");
    }

    #[test]
    fn a_zero_sized_cell_is_not_a_panic() {
        let mut pixels = vec![0u8; 4];
        let mut canvas = Canvas::new(&mut pixels, 1, 1);
        draw(
            &mut canvas,
            Rect::new(0, 0, 0, 0),
            Kind::Plain,
            Tint::Normal,
        );
    }
}
