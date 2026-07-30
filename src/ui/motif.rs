// SPDX-License-Identifier: GPL-3.0-or-later
//! Motif's raised and sunken edges.
//!
//! Every panel, button and field in the Indigo Magic look is a rectangle with a beveled
//! edge: two light sides and two dark, so it reads as standing out of the screen or
//! pressed into it. Which pair goes where is the only difference between raised and
//! sunken, and each surface carries its *own* light/dark pair -- which is why the palette
//! has a shadow pair per surface rather than one global one.
//!
//! The edge is `thickness` nested rings. Motif's convention: the top and left sides take
//! the light color, the bottom and right take the dark, and the two off-diagonal corner
//! pixels go to the light side.
//!
//! **Copied from `wlrix-greeter/src/ui/motif.rs`, and kept in step by hand.** The two repos
//! build standalone -- there is no shared crate to depend on -- so the drawing primitives are
//! duplicated rather than shared. If a third consumer ever wants them, that is the moment to
//! pull this and its neighbors out into a `wlrix-ui` crate instead of copying a third time.

use crate::theme::Rgb;
use crate::ui::canvas::{Canvas, Rect};

/// A bevel drawn light-top-left, dark-bottom-right (raised) or the reverse (sunken).
#[derive(Clone, Copy)]
pub struct Bevel {
    pub top_left: Rgb,
    pub bottom_right: Rgb,
    pub thickness: i32,
}

impl Bevel {
    /// Standing out of the screen: light above and left.
    pub fn raised(top_shadow: Rgb, bottom_shadow: Rgb, thickness: i32) -> Self {
        Self {
            top_left: top_shadow,
            bottom_right: bottom_shadow,
            thickness,
        }
    }

    /// Pressed into the screen: the light and dark swap, so the highlight is below.
    pub fn sunken(top_shadow: Rgb, bottom_shadow: Rgb, thickness: i32) -> Self {
        Self {
            top_left: bottom_shadow,
            bottom_right: top_shadow,
            thickness,
        }
    }
}

/// Draw a beveled edge just inside `rect`, leaving the interior untouched.
pub fn draw(canvas: &mut Canvas, rect: Rect, bevel: Bevel) {
    for ring in 0..bevel.thickness {
        let r = rect.inset(ring);
        if r.w <= 0 || r.h <= 0 {
            break;
        }
        let (top, bottom) = (r.top(), r.bottom() - 1);
        let (left, right) = (r.left(), r.right() - 1);

        // Top and left edges: the light side.
        canvas.fill_rect(Rect::new(r.x, r.y, r.w, 1), bevel.top_left);
        canvas.fill_rect(Rect::new(r.x, r.y, 1, r.h), bevel.top_left);
        // Bottom and right edges: the dark side.
        canvas.fill_rect(Rect::new(r.x, bottom, r.w, 1), bevel.bottom_right);
        canvas.fill_rect(Rect::new(right, r.y, 1, r.h), bevel.bottom_right);
        // The two corners the edges disagree on go to the light side, Motif's way.
        canvas.put(right, top, bevel.top_left);
        canvas.put(left, bottom, bevel.top_left);
    }
}

/// A beveled panel: fill the interior, then edge it.
pub fn panel(canvas: &mut Canvas, rect: Rect, fill: Rgb, bevel: Bevel) {
    canvas.fill_rect(rect, fill);
    draw(canvas, rect, bevel);
}

/// A single-pixel outline in `color`, drawn *around* `rect`'s outermost ring.
///
/// The dialog's black keyline sits outside the raised bevel, so it is drawn separately
/// from [`draw`].
pub fn outline(canvas: &mut Canvas, rect: Rect, colour: Rgb) {
    canvas.stroke_rect(rect, colour);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::palette;

    const LIGHT: Rgb = Rgb(0xff_e0e0e0);
    const DARK: Rgb = Rgb(0xff_606060);

    fn canvas(buf: &mut Vec<u8>, size: i32) -> Canvas<'_> {
        *buf = vec![0u8; (size * size * 4) as usize];
        Canvas::new(buf, size, size)
    }

    #[test]
    fn raised_puts_light_top_left_dark_bottom_right() {
        let mut buf = Vec::new();
        let mut c = canvas(&mut buf, 8);
        let rect = Rect::new(0, 0, 8, 8);
        panel(&mut c, rect, palette::FACE, Bevel::raised(LIGHT, DARK, 2));
        // A pixel on the top edge is light, one on the bottom edge is dark.
        assert_eq!(c.get(3, 0), LIGHT);
        assert_eq!(c.get(3, 7), DARK);
        assert_eq!(c.get(0, 3), LIGHT);
        assert_eq!(c.get(7, 3), DARK);
        // The interior kept the fill.
        assert_eq!(c.get(4, 4), palette::FACE);
    }

    #[test]
    fn sunken_is_the_reverse() {
        let mut buf = Vec::new();
        let mut c = canvas(&mut buf, 8);
        panel(
            &mut c,
            Rect::new(0, 0, 8, 8),
            palette::FACE,
            Bevel::sunken(LIGHT, DARK, 2),
        );
        // Now the top is dark and the bottom light -- pressed in.
        assert_eq!(c.get(3, 0), DARK);
        assert_eq!(c.get(3, 7), LIGHT);
    }

    #[test]
    fn thickness_draws_that_many_rings() {
        let mut buf = Vec::new();
        let mut c = canvas(&mut buf, 10);
        draw(
            &mut c,
            Rect::new(0, 0, 10, 10),
            Bevel::raised(LIGHT, DARK, 3),
        );
        // Three light rings deep on the top edge, then the untouched interior.
        assert_eq!(c.get(5, 0), LIGHT);
        assert_eq!(c.get(5, 1), LIGHT);
        assert_eq!(c.get(5, 2), LIGHT);
        assert_eq!(c.get(5, 3), Rgb(0));
    }
}
