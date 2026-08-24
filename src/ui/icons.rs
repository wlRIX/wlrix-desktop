// SPDX-License-Identifier: GPL-3.0-or-later
//! The icons themselves.
//!
//! There is no artwork in the tree yet, so these are drawn in code -- the same approach
//! `wlrix-greeter/src/ui/icons.rs` takes for its user icons. Each is a small IRIX-ish emblem:
//! a folder, a sheet of paper, a launcher badge, a terminal-ish block for an executable.
//!
//! **Everything is drawn into a coverage mask, not straight onto the canvas** -- see
//! [`wlrix_ui::mask`] for why. What a folder or a launcher badge looks like is this
//! component's business, so the shapes stay here and only the bitmap they are drawn into is
//! shared.

use crate::entries::Kind;
use wlrix_ui::Rgb;
use wlrix_ui::canvas::{Canvas, Rect};
use wlrix_ui::mask::Mask;
use wlrix_ui::palette::Palette;

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
    pub fn color(self, palette: &Palette) -> Rgb {
        match self {
            Tint::Normal => palette.icon_tint,
            Tint::Hover => palette.icon_tint_hover,
            Tint::Selected => palette.icon_tint_selected,
        }
    }

    /// The color the label text takes. A selected label inverts onto its tinted background.
    pub fn label_color(self, palette: &Palette) -> Rgb {
        match self {
            Tint::Selected => palette.icon_label_selected,
            _ => palette.icon_label,
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
pub fn draw(canvas: &mut Canvas, palette: &Palette, rect: Rect, kind: Kind, tint: Tint) {
    let size = rect.w.min(rect.h);
    if size <= 0 {
        return;
    }
    // Center the square inside `rect`, so a non-square cell still looks right.
    let x0 = rect.x + (rect.w - size) / 2;
    let y0 = rect.y + (rect.h - size) / 2;
    mask(kind, size).draw(canvas, Rect::new(x0, y0, size, size), tint.color(palette));
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

    fn coverage(mask: &Mask) -> Vec<u8> {
        (0..mask.size())
            .flat_map(|y| (0..mask.size()).map(move |x| (x, y)))
            .map(|(x, y)| mask.get(x, y))
            .collect()
    }

    fn covered(mask: &Mask) -> usize {
        coverage(mask).iter().filter(|c| **c != 0).count()
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
        let masks: Vec<Vec<u8>> = KINDS.iter().map(|k| coverage(&mask(*k, 64))).collect();
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
            assert_eq!(mask.size(), 64, "{kind:?} mask is the wrong size");
            assert_eq!(coverage(&mask).len(), 64 * 64);
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
    fn each_state_has_its_own_color() {
        // The whole point of the tint: the three states must be visibly distinct, in every
        // scheme -- a palette that gave two states one color would make hover invisible.
        for palette in wlrix_ui::palette::ALL {
            assert_ne!(Tint::Normal.color(palette), Tint::Hover.color(palette));
            assert_ne!(Tint::Normal.color(palette), Tint::Selected.color(palette));
            assert_ne!(Tint::Hover.color(palette), Tint::Selected.color(palette));
        }
        // And the requested colors specifically, in the default scheme: gray, white, yellow.
        let p = wlrix_ui::palette::DEFAULT;
        assert_eq!(Tint::Normal.color(p), Rgb(0xff_aaaaaa));
        assert_eq!(Tint::Hover.color(p), Rgb(0xff_ffffff));
        assert_eq!(Tint::Selected.color(p), Rgb(0xff_ffff00));
    }

    #[test]
    fn drawing_lands_on_the_canvas_in_the_tint() {
        let p = wlrix_ui::palette::DEFAULT;
        let mut pixels = vec![0u8; 64 * 64 * 4];
        let mut canvas = Canvas::new(&mut pixels, 64, 64);
        canvas.clear(p.desktop);
        draw(
            &mut canvas,
            p,
            Rect::new(0, 0, 64, 64),
            Kind::Directory,
            Tint::Selected,
        );

        // Somewhere in the folder body should now be the selected yellow.
        let mut hits = 0;
        for y in 0..64 {
            for x in 0..64 {
                if canvas.get(x, y) == p.icon_tint_selected {
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
            wlrix_ui::palette::DEFAULT,
            Rect::new(0, 0, 0, 0),
            Kind::Plain,
            Tint::Normal,
        );
    }
}
