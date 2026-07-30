// SPDX-License-Identifier: GPL-3.0-or-later
//! A place to draw pixels.
//!
//! Wraps the `wl_shm` buffer the compositor shares with us -- a flat run of bytes in
//! `Argb8888` -- and offers just enough to paint the dialog: rectangles, single pixels,
//! and blending a glyph's coverage over what is already there.
//!
//! Coordinates are the buffer's own, top-left origin. Everything is clipped to the
//! canvas, so a widget that lays out slightly off the edge draws less rather than
//! panicking.
//!
//! **Copied from `wlrix-greeter/src/ui/canvas.rs`, and kept in step by hand.** The two repos
//! build standalone -- there is no shared crate to depend on -- so the drawing primitives are
//! duplicated rather than shared. If a third consumer ever wants them, that is the moment to
//! pull this and its neighbors out into a `wlrix-ui` crate instead of copying a third time.

use crate::theme::Rgb;

/// A borrowed drawing surface over a shm buffer.
pub struct Canvas<'a> {
    pixels: &'a mut [u8],
    width: i32,
    height: i32,
}

impl<'a> Canvas<'a> {
    /// Wrap a buffer. `pixels` must hold `width * height` pixels of four bytes each.
    pub fn new(pixels: &'a mut [u8], width: i32, height: i32) -> Self {
        debug_assert!(pixels.len() >= (width * height * 4) as usize);
        Self {
            pixels,
            width,
            height,
        }
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    /// The whole canvas as a rectangle, for clipping against.
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Paint every pixel one color.
    pub fn clear(&mut self, colour: Rgb) {
        let bytes = colour.0.to_ne_bytes();
        for pixel in self.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&bytes);
        }
    }

    /// Set one pixel, if it is on the canvas.
    pub fn put(&mut self, x: i32, y: i32, colour: Rgb) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let offset = ((y * self.width + x) * 4) as usize;
        self.pixels[offset..offset + 4].copy_from_slice(&colour.0.to_ne_bytes());
    }

    /// Read one pixel. Off-canvas reads as opaque black, which callers never rely on --
    /// blending only ever reads pixels it is about to write.
    pub fn get(&self, x: i32, y: i32) -> Rgb {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return Rgb(0xff00_0000);
        }
        let offset = ((y * self.width + x) * 4) as usize;
        let bytes = [
            self.pixels[offset],
            self.pixels[offset + 1],
            self.pixels[offset + 2],
            self.pixels[offset + 3],
        ];
        Rgb(u32::from_ne_bytes(bytes))
    }

    /// Fill a rectangle, clipped to the canvas.
    pub fn fill_rect(&mut self, rect: Rect, colour: Rgb) {
        let clipped = rect.intersect(self.bounds());
        for y in clipped.top()..clipped.bottom() {
            for x in clipped.left()..clipped.right() {
                self.put(x, y, colour);
            }
        }
    }

    /// A one-pixel outline just inside `rect`.
    pub fn stroke_rect(&mut self, rect: Rect, colour: Rgb) {
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        self.fill_rect(Rect::new(rect.x, rect.y, rect.w, 1), colour);
        self.fill_rect(Rect::new(rect.x, rect.bottom() - 1, rect.w, 1), colour);
        self.fill_rect(Rect::new(rect.x, rect.y, 1, rect.h), colour);
        self.fill_rect(Rect::new(rect.right() - 1, rect.y, 1, rect.h), colour);
    }

    /// Erase the canvas to fully transparent.
    ///
    /// **This is what the desktop clears to**, not a color: the icons sit on a layer surface
    /// with a wallpaper (`swaybg` and friends) on the layer below, so painting an opaque
    /// background would hide it. Where nothing is drawn, the wallpaper -- or the compositor's
    /// own desktop gray when there is none -- shows through.
    pub fn clear_transparent(&mut self) {
        self.pixels.fill(0);
    }

    /// Blend `color` over one pixel by `coverage` (0 = leave alone, 255 = opaque).
    ///
    /// This is the text and icon path: a glyph or an icon mask is an 8-bit coverage bitmap,
    /// laid over whatever is already there.
    ///
    /// Source-over in **premultiplied** alpha, which is what `wl_shm`'s `Argb8888` wants and
    /// what a partly transparent canvas needs -- an antialiased glyph edge over bare desktop
    /// has to end up partly transparent, not blended against opaque black. On an already
    /// opaque pixel this reduces to the plain interpolation it replaced, so nothing that was
    /// drawn on a solid background changes.
    ///
    /// (This is where the desktop's copy of `canvas.rs` diverges from the greeter's, which
    /// only ever paints an opaque dialog. See the note at the top of the file.)
    pub fn blend(&mut self, x: i32, y: i32, colour: Rgb, coverage: u8) {
        if coverage == 0 {
            return;
        }
        let (sr, sg, sb) = colour.channels();
        let dst = self.get(x, y).0.to_be_bytes();
        let alpha = u32::from(coverage);
        let inverse = 255 - alpha;

        // `(v * 255 + 127) / 255` rounds rather than truncating; truncating loses a level per
        // composite, which shows as text going muddy where it overlaps.
        let over = |src: u8, dst: u8| -> u32 {
            let src = u32::from(src) * alpha;
            let dst = u32::from(dst) * inverse;
            (src + dst + 127) / 255
        };

        let a = over(255, dst[0]);
        let r = over(sr, dst[1]);
        let g = over(sg, dst[2]);
        let b = over(sb, dst[3]);
        self.put(x, y, Rgb((a << 24) | (r << 16) | (g << 8) | b));
    }

    /// Composite one already-premultiplied ARGB pixel over what is there.
    ///
    /// [`blend`](Self::blend) takes a straight color and a coverage, which suits a glyph or an
    /// icon mask -- one color, varying alpha. A loaded image is neither: every pixel has its
    /// own color *and* its own alpha, already multiplied together by the decoder. So the
    /// source needs no scaling here, only the destination does.
    pub fn blend_premultiplied(&mut self, x: i32, y: i32, pixel: u32) {
        let source = pixel.to_be_bytes();
        let alpha = u32::from(source[0]);
        if alpha == 0 {
            return;
        }
        if alpha == 255 {
            // Opaque: nothing underneath survives, so skip the arithmetic.
            self.put(x, y, Rgb(pixel));
            return;
        }
        let inverse = 255 - alpha;
        let dst = self.get(x, y).0.to_be_bytes();
        let over = |src: u8, dst: u8| -> u32 {
            (u32::from(src) * 255 + u32::from(dst) * inverse + 127) / 255
        };
        let a = over(source[0], dst[0]);
        let r = over(source[1], dst[1]);
        let g = over(source[2], dst[2]);
        let b = over(source[3], dst[3]);
        self.put(x, y, Rgb((a << 24) | (r << 16) | (g << 8) | b));
    }
}

/// A rectangle in canvas pixels. Right/bottom are exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn left(&self) -> i32 {
        self.x
    }
    pub fn top(&self) -> i32 {
        self.y
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// Whether a point is inside. Used for hit-testing, so the exclusive edges matter:
    /// the pixel at `right()`/`bottom()` belongs to the next rectangle along.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left() && x < self.right() && y >= self.top() && y < self.bottom()
    }

    /// Shrink on every side by `by`. Negative grows it.
    pub fn inset(&self, by: i32) -> Rect {
        Rect::new(
            self.x + by,
            self.y + by,
            (self.w - by * 2).max(0),
            (self.h - by * 2).max(0),
        )
    }

    /// The overlap of two rectangles, empty if they do not touch.
    pub fn intersect(&self, other: Rect) -> Rect {
        let left = self.left().max(other.left());
        let top = self.top().max(other.top());
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect::new(left, top, (right - left).max(0), (bottom - top).max(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_uses_exclusive_edges() {
        let r = Rect::new(10, 10, 20, 20); // covers x 10..30, y 10..30
        assert!(r.contains(10, 10));
        assert!(r.contains(29, 29));
        assert!(!r.contains(30, 30)); // the next rect's corner
        assert!(!r.contains(9, 10));
    }

    #[test]
    fn intersect_of_disjoint_rects_is_empty() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 20, 10, 10);
        let hit = a.intersect(b);
        assert_eq!(hit.w, 0);
        assert_eq!(hit.h, 0);
    }

    #[test]
    fn fill_and_read_back() {
        let mut buf = vec![0u8; 4 * 4 * 4];
        let mut canvas = Canvas::new(&mut buf, 4, 4);
        canvas.fill_rect(Rect::new(1, 1, 2, 2), Rgb(0xff_123456));
        assert_eq!(canvas.get(1, 1), Rgb(0xff_123456));
        assert_eq!(canvas.get(0, 0), Rgb(0)); // untouched
        assert_eq!(canvas.get(3, 3), Rgb(0));
    }

    #[test]
    fn drawing_off_canvas_is_clipped_not_a_panic() {
        let mut buf = vec![0u8; 4 * 4 * 4];
        let mut canvas = Canvas::new(&mut buf, 4, 4);
        // Straddles every edge; must touch only the in-bounds pixels.
        canvas.fill_rect(Rect::new(-5, -5, 20, 20), Rgb(0xff_ffffff));
        canvas.put(100, 100, Rgb(0xff_ffffff));
        assert_eq!(canvas.get(0, 0), Rgb(0xff_ffffff));
        assert_eq!(canvas.get(3, 3), Rgb(0xff_ffffff));
    }

    #[test]
    fn blend_extremes_match_the_endpoints() {
        let mut buf = vec![0u8; 4]; // one pixel
        let mut canvas = Canvas::new(&mut buf, 1, 1);
        canvas.put(0, 0, Rgb(0xff00_0000));
        canvas.blend(0, 0, Rgb(0xffff_ffff), 0);
        assert_eq!(
            canvas.get(0, 0),
            Rgb(0xff00_0000),
            "zero coverage leaves it"
        );
        canvas.blend(0, 0, Rgb(0xffff_ffff), 255);
        assert_eq!(
            canvas.get(0, 0),
            Rgb(0xffff_ffff),
            "full coverage replaces it"
        );
    }
}
