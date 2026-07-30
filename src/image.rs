// SPDX-License-Identifier: GPL-3.0-or-later
//! Turning icon files into pixels.
//!
//! Two sources, one answer: an SVG goes through `resvg`, a PNG through `png`, and both come
//! back as an [`Image`] of premultiplied `Argb8888` at the size asked for -- the same form the
//! `wl_shm` buffer wants, so drawing one is a straight composite with no conversion.
//!
//! SVG is rendered *at* the requested size rather than rendered once and scaled, so an icon is
//! crisp at whatever the configured icon size happens to be. A PNG is scaled, since there is
//! nothing else to be done with it; the theme lookup asks for the nearest size first, so the
//! scaling is usually slight.
//!
//! Loading is cached by (path, size). An icon is redrawn on every hover and every drag frame,
//! and re-rasterizing an SVG at 60Hz would be absurd.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::theme::Rgb;
use crate::ui::canvas::Canvas;

/// A decoded icon: premultiplied `Argb8888`, ready to composite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: i32,
    pub height: i32,
    /// `width * height` pixels, packed as native-endian ARGB with the color channels already
    /// multiplied by alpha.
    pixels: Vec<u32>,
}

impl Image {
    /// One pixel, or fully transparent off the edge.
    pub fn get(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return 0;
        }
        self.pixels[(y * self.width + x) as usize]
    }

    /// Draw the image with its top-left at `(x, y)`, compositing over what is there.
    ///
    /// Source-over in premultiplied space, matching [`Canvas::blend`] -- an icon's antialiased
    /// edge has to stay partly transparent so the wallpaper shows through it.
    pub fn draw(&self, canvas: &mut Canvas, x: i32, y: i32) {
        for row in 0..self.height {
            for column in 0..self.width {
                let pixel = self.get(column, row);
                let alpha = (pixel >> 24) as u8;
                if alpha == 0 {
                    continue;
                }
                canvas.blend_premultiplied(x + column, y + row, pixel);
            }
        }
    }

    /// The image with every color channel multiplied by `tint`, alpha untouched.
    ///
    /// Multiply rather than replace, because replacing flattens a drawing into one solid
    /// shape. The magic carpet is a white body with a black outline and a shadow: multiplied
    /// by yellow the body goes yellow and the outline stays black, which is the tint reading
    /// as a highlight. Replaced by yellow it would be a yellow blob.
    ///
    /// This is what gives a launcher the same gray/white/yellow states as the drawn-in-code
    /// icons **without** recoloring the application's own artwork, which wants its own
    /// colors: the carpet is tinted, the symbol standing on it is not.
    pub fn multiplied(&self, tint: Rgb) -> Image {
        let (r, g, b) = tint.channels();
        let pixels = self
            .pixels
            .iter()
            .map(|pixel| {
                // Premultiplied in, premultiplied out: scaling a channel down can never take
                // it above the alpha it started under.
                let scale = |channel: u32, by: u8| (channel * u32::from(by) + 127) / 255;
                (pixel & 0xff00_0000)
                    | (scale((pixel >> 16) & 0xff, r) << 16)
                    | (scale((pixel >> 8) & 0xff, g) << 8)
                    | scale(pixel & 0xff, b)
            })
            .collect();
        Image {
            width: self.width,
            height: self.height,
            pixels,
        }
    }
}

/// Icons already loaded, keyed by what was asked for.
#[derive(Default)]
pub struct Images {
    /// `None` records a file that failed to load, so a broken icon is not retried every frame.
    cache: HashMap<(PathBuf, i32), Option<Image>>,
    /// `Icon=` names already resolved to files, including the ones that resolved to nothing.
    resolved: HashMap<String, Option<PathBuf>>,
}

impl Images {
    /// Load `path` at `size` square, from the cache when it has been seen before.
    pub fn load(&mut self, path: &Path, size: i32) -> Option<&Image> {
        let key = (path.to_path_buf(), size);
        if !self.cache.contains_key(&key) {
            let loaded = decode(path, size);
            if loaded.is_none() {
                eprintln!("wlrix-desktop: could not load the icon {}", path.display());
            }
            self.cache.insert(key.clone(), loaded);
        }
        self.cache.get(&key)?.as_ref()
    }

    /// Load artwork the program carries itself, cached under a made-up `key`.
    ///
    /// The magic carpets are `include_bytes!`d rather than read from a data directory, so they
    /// cannot go missing from an installed desktop -- but they still want the same cache as
    /// everything else, since they are re-rasterized on every frame otherwise.
    pub fn load_bytes(&mut self, key: &Path, bytes: &[u8], size: i32) -> Option<&Image> {
        let entry = (key.to_path_buf(), size);
        if !self.cache.contains_key(&entry) {
            let loaded = (size > 0).then(|| from_svg(bytes, size)).flatten();
            if loaded.is_none() {
                eprintln!("wlrix-desktop: could not rasterize {}", key.display());
            }
            self.cache.insert(entry.clone(), loaded);
        }
        self.cache.get(&entry)?.as_ref()
    }

    /// Find the file an `Icon=` value names.
    ///
    /// An absolute path is taken as-is; anything else is a theme lookup. `size` is a request,
    /// not a promise -- a theme may only have one size, and the result is scaled to fit.
    pub fn resolve(&mut self, icon: &str, size: i32) -> Option<PathBuf> {
        if icon.is_empty() {
            return None;
        }
        // The path case is not worth caching: it is one `is_file` call.
        let candidate = Path::new(icon);
        if candidate.is_absolute() {
            return candidate.is_file().then(|| candidate.to_path_buf());
        }

        if let Some(found) = self.resolved.get(icon) {
            return found.clone();
        }
        let found = freedesktop_icons::lookup(icon)
            .with_size(size.clamp(1, u16::MAX as i32) as u16)
            // `hicolor` is the fallback every theme inherits, and the lookup covers
            // `/usr/share/pixmaps` too -- which is where `Icon=Alacritty` actually lives.
            .with_cache()
            .find();
        if found.is_none() {
            eprintln!("wlrix-desktop: no icon found for {icon:?}");
        }
        self.resolved.insert(icon.to_owned(), found.clone());
        found
    }

    /// Resolve and load in one step.
    pub fn get(&mut self, icon: &str, size: i32) -> Option<&Image> {
        let path = self.resolve(icon, size)?;
        self.load(&path, size)
    }

    /// Forget everything. Called when the icon size changes, which invalidates every entry.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.resolved.clear();
    }
}

/// Decode one file at `size`, picking the reader by what the bytes actually are.
///
/// The extension is a hint, not evidence -- themes do ship a `.png` that is really an SVG --
/// so the content is sniffed first and the extension only breaks ties.
fn decode(path: &Path, size: i32) -> Option<Image> {
    if size <= 0 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return from_png(&bytes, size);
    }
    from_svg(&bytes, size)
}

/// Rasterize an SVG at exactly `size` square.
///
/// Aspect ratio is kept: a non-square drawing is scaled to fit and centred, so a wide icon
/// does not come out stretched.
fn from_svg(bytes: &[u8], size: i32) -> Option<Image> {
    use resvg::tiny_skia;
    use resvg::usvg;

    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;

    let drawing = tree.size();
    if drawing.width() <= 0.0 || drawing.height() <= 0.0 {
        return None;
    }
    let scale = (size as f32 / drawing.width()).min(size as f32 / drawing.height());
    let offset_x = (size as f32 - drawing.width() * scale) / 2.0;
    let offset_y = (size as f32 - drawing.height() * scale) / 2.0;

    let mut pixmap = tiny_skia::Pixmap::new(size as u32, size as u32)?;
    let transform =
        tiny_skia::Transform::from_translate(offset_x, offset_y).pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // tiny-skia hands back premultiplied RGBA; the canvas wants premultiplied ARGB.
    let pixels = pixmap
        .pixels()
        .iter()
        .map(|pixel| {
            (u32::from(pixel.alpha()) << 24)
                | (u32::from(pixel.red()) << 16)
                | (u32::from(pixel.green()) << 8)
                | u32::from(pixel.blue())
        })
        .collect();

    Some(Image {
        width: size,
        height: size,
        pixels,
    })
}

/// Decode a PNG and scale it to `size` square.
fn from_png(bytes: &[u8], size: i32) -> Option<Image> {
    // `Cursor`, because png 0.18's decoder wants `BufRead + Seek` and a bare slice is neither.
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // Expand palettes and low bit depths, and narrow 16-bit channels to 8. Themes ship all of
    // these -- `/usr/share/pixmaps/archlinux-logo.png` is 8-bit *colormap* -- and handling only
    // plain RGB/RGBA meant those icons silently failed to load.
    decoder.set_transformations(png::Transformations::normalize_to_color8());

    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;

    let (width, height) = (info.width as i32, info.height as i32);
    if width <= 0 || height <= 0 {
        return None;
    }
    // After normalizing there are only these four, all 8 bits per channel.
    let (channels, grey) = match info.color_type {
        png::ColorType::Rgba => (4, false),
        png::ColorType::Rgb => (3, false),
        png::ColorType::GrayscaleAlpha => (2, true),
        png::ColorType::Grayscale => (1, true),
        png::ColorType::Indexed => return None,
    };

    // Nearest-neighbor, then premultiply. The theme lookup asks for the size we want, so the
    // scaling here is usually small or absent; a nicer filter is not worth the code.
    let mut pixels = Vec::with_capacity((size * size) as usize);
    for row in 0..size {
        let source_y = (row * height / size).clamp(0, height - 1);
        for column in 0..size {
            let source_x = (column * width / size).clamp(0, width - 1);
            let offset = ((source_y * width + source_x) * channels) as usize;
            // Gray expands to three equal channels; the alpha is the last one when there is
            // one, and opaque otherwise.
            let (red, green, blue) = if grey {
                let grey = buffer[offset];
                (grey, grey, grey)
            } else {
                (buffer[offset], buffer[offset + 1], buffer[offset + 2])
            };
            let alpha = match channels {
                4 | 2 => buffer[offset + channels as usize - 1],
                _ => 255,
            };
            let premultiply = |channel: u8| (u32::from(channel) * u32::from(alpha) + 127) / 255;
            pixels.push(
                (u32::from(alpha) << 24)
                    | (premultiply(red) << 16)
                    | (premultiply(green) << 8)
                    | premultiply(blue),
            );
        }
    }

    Some(Image {
        width: size,
        height: size,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 SVG: a solid red square filling its whole viewBox.
    const RED_SQUARE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
        <rect x="0" y="0" width="10" height="10" fill="#ff0000"/></svg>"##;

    /// A wide SVG, for the aspect-ratio check.
    const WIDE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 10">
        <rect x="0" y="0" width="20" height="10" fill="#00ff00"/></svg>"##;

    fn alpha(pixel: u32) -> u8 {
        (pixel >> 24) as u8
    }

    #[test]
    fn an_svg_rasterizes_at_the_size_asked_for() {
        let image = from_svg(RED_SQUARE, 32).expect("should rasterize");
        assert_eq!((image.width, image.height), (32, 32));
        // The middle is opaque red.
        let middle = image.get(16, 16);
        assert_eq!(alpha(middle), 255);
        assert_eq!((middle >> 16) & 0xff, 0xff, "red channel");
        assert_eq!(middle & 0xff, 0x00, "blue channel");
    }

    #[test]
    fn the_same_svg_at_two_sizes_gives_two_sizes() {
        // Rendered at the requested size, not rendered once and scaled.
        for size in [16, 24, 48, 64, 128] {
            let image = from_svg(RED_SQUARE, size).expect("should rasterize");
            assert_eq!((image.width, image.height), (size, size));
        }
    }

    #[test]
    fn a_wide_svg_is_letterboxed_rather_than_stretched() {
        let image = from_svg(WIDE, 40).expect("should rasterize");
        assert_eq!((image.width, image.height), (40, 40));
        // 2:1 into a square: the drawing occupies the middle 20 rows, so the top is empty.
        assert_eq!(alpha(image.get(20, 2)), 0, "the top should be empty");
        assert_eq!(alpha(image.get(20, 20)), 255, "the middle should be drawn");
        assert_eq!(alpha(image.get(20, 37)), 0, "the bottom should be empty");
    }

    #[test]
    fn the_magic_carpets_load() {
        // The two files this whole feature rests on. If they ever stop parsing, every
        // application icon quietly loses its base.
        for name in ["generic.exec.closed.svg", "generic.exec.open.svg"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join(name);
            let image = decode(&path, 64).unwrap_or_else(|| panic!("{name} should decode"));
            assert_eq!((image.width, image.height), (64, 64));
            let drawn = image
                .pixels
                .iter()
                .filter(|pixel| alpha(**pixel) != 0)
                .count();
            assert!(drawn > 0, "{name} rasterized to nothing");
        }
    }

    #[test]
    fn the_two_carpets_look_different() {
        // Open and closed must actually differ, or the running state is invisible.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let closed = decode(&dir.join("generic.exec.closed.svg"), 64).expect("closed");
        let open = decode(&dir.join("generic.exec.open.svg"), 64).expect("open");
        assert_ne!(closed, open);
    }

    /// A tiny PNG in the given color type, built here so the test does not depend on what
    /// happens to be installed.
    fn png_bytes(colour: png::ColorType, depth: png::BitDepth) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 4, 4);
            encoder.set_color(colour);
            encoder.set_depth(depth);
            if colour == png::ColorType::Indexed {
                // A four-color palette, and pixels indexing into it.
                encoder.set_palette(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
            }
            let mut writer = encoder.write_header().expect("header");
            let data: Vec<u8> = match colour {
                png::ColorType::Indexed => (0..16).map(|i| i % 4).collect(),
                png::ColorType::Grayscale => vec![128; 16],
                png::ColorType::GrayscaleAlpha => vec![128; 32],
                png::ColorType::Rgb => vec![200; 48],
                png::ColorType::Rgba => vec![200; 64],
            };
            writer.write_image_data(&data).expect("write");
        }
        out
    }

    #[test]
    fn every_png_flavour_a_theme_ships_decodes() {
        // The one that caught this: `/usr/share/pixmaps/archlinux-logo.png` is 8-bit
        // *colormap*, and rejecting palettes meant it silently failed to load. Greyscale and
        // 16-bit turn up in themes too.
        for (colour, depth) in [
            (png::ColorType::Rgba, png::BitDepth::Eight),
            (png::ColorType::Rgb, png::BitDepth::Eight),
            (png::ColorType::Indexed, png::BitDepth::Eight),
            (png::ColorType::Grayscale, png::BitDepth::Eight),
            (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight),
        ] {
            let bytes = png_bytes(colour, depth);
            let image = from_png(&bytes, 16)
                .unwrap_or_else(|| panic!("{colour:?} at {depth:?} failed to decode"));
            assert_eq!((image.width, image.height), (16, 16), "{colour:?}");
            assert!(
                image.pixels.iter().any(|pixel| alpha(*pixel) != 0),
                "{colour:?} decoded to nothing"
            );
        }
    }

    #[test]
    fn a_real_palette_png_from_the_system_decodes() {
        // Belt and braces against the synthetic one above: if this file is not installed the
        // test skips rather than failing on a machine that simply lacks it.
        let path = Path::new("/usr/share/pixmaps/archlinux-logo.png");
        if !path.is_file() {
            return;
        }
        let image = decode(path, 48).expect("a palette PNG from the system should decode");
        assert_eq!((image.width, image.height), (48, 48));
        assert!(image.pixels.iter().any(|pixel| alpha(*pixel) != 0));
    }

    #[test]
    fn nonsense_does_not_panic() {
        assert!(decode(Path::new("/nonexistent/icon.svg"), 32).is_none());
        assert!(from_svg(b"not an svg at all", 32).is_none());
        assert!(from_png(b"\x89PNG\r\n\x1a\ngarbage", 32).is_none());
        // A zero or negative size is a configuration accident, not a crash.
        assert!(decode(Path::new("/nonexistent"), 0).is_none());
    }

    #[test]
    fn multiplying_keeps_the_shape_and_the_dark_parts() {
        let image = from_svg(RED_SQUARE, 16).expect("should rasterize");
        let tinted = image.multiplied(Rgb(0xff_ffff00));
        assert_eq!((tinted.width, tinted.height), (16, 16));
        // Same coverage: a tint must not change what is drawn, only its color.
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(alpha(tinted.get(x, y)), alpha(image.get(x, y)));
            }
        }
        // Red multiplied by yellow keeps its red and loses its (already absent) blue.
        let middle = tinted.get(8, 8);
        assert_eq!((middle >> 16) & 0xff, 0xff, "red");
        assert_eq!(middle & 0xff, 0x00, "blue");
    }

    #[test]
    fn multiplying_by_white_changes_nothing() {
        // The hover state is `ICON_TINT_HOVER`, which is white -- so hovering must leave the
        // artwork exactly as drawn rather than shifting it a channel.
        let carpet = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("generic.exec.closed.svg");
        let image = decode(&carpet, 48).expect("carpet");
        assert_eq!(image.multiplied(Rgb(0xff_ffffff)), image);
    }

    #[test]
    fn multiplying_keeps_dark_parts_dark_and_brightens_light_ones() {
        // The whole reason for multiply over replace. Replacing would make the carpet one flat
        // yellow shape; multiplying turns its white body yellow while its outline and shadow
        // stay dark, which is what reads as a highlight rather than a blob.
        let carpet = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("generic.exec.closed.svg");
        let image = decode(&carpet, 48).expect("carpet");
        let tinted = image.multiplied(Rgb(0xff_ffff00));

        // These are *premultiplied*, so a channel is already scaled by its alpha -- the
        // carpet's shadow is 75%-opacity black, which is alpha 191 with a red of 0. Comparing
        // the channel to a flat threshold would call that "dark" only by accident, so the
        // brightness worth testing is the channel *relative to* its alpha.
        let brightness = |pixel: u32| {
            let alpha = (pixel >> 24) & 0xff;
            (alpha > 0).then(|| (((pixel >> 16) & 0xff) * 255) / alpha)
        };
        let mut checked_dark = false;
        let mut checked_light = false;

        for y in 0..48 {
            for x in 0..48 {
                let (before, after) = (image.get(x, y), tinted.get(x, y));
                let (Some(was), Some(now)) = (brightness(before), brightness(after)) else {
                    continue;
                };
                if was < 40 {
                    assert!(now < 40, "a dark pixel was brightened at {x},{y}");
                    checked_dark = true;
                }
                if was > 200 {
                    // Yellow keeps red and green and drops blue, so the body stays bright.
                    assert!(now > 200, "a light pixel was darkened at {x},{y}");
                    assert_eq!(after & 0xff, 0, "blue should be gone at {x},{y}");
                    checked_light = true;
                }
            }
        }
        assert!(checked_dark, "found no dark pixel to check (the shadow?)");
        assert!(checked_light, "found no light pixel to check (the body?)");
    }

    #[test]
    fn a_failed_load_is_remembered_rather_than_retried() {
        let mut images = Images::default();
        let missing = Path::new("/nonexistent/icon.svg");
        assert!(images.load(missing, 32).is_none());
        assert!(images.load(missing, 32).is_none());
        assert_eq!(images.cache.len(), 1, "should have cached the failure");
    }

    #[test]
    fn loading_twice_reuses_the_first_result() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let carpet = dir.join("generic.exec.closed.svg");
        let mut images = Images::default();
        assert!(images.load(&carpet, 48).is_some());
        assert!(images.load(&carpet, 48).is_some());
        assert_eq!(images.cache.len(), 1);
        // A different size is a different entry, not a replacement.
        assert!(images.load(&carpet, 64).is_some());
        assert_eq!(images.cache.len(), 2);
    }

    #[test]
    fn an_absolute_path_resolves_to_itself() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let carpet = dir.join("generic.exec.closed.svg");
        let mut images = Images::default();
        assert_eq!(
            images.resolve(&carpet.to_string_lossy(), 48),
            Some(carpet.clone())
        );
        // ...and one that is not there resolves to nothing rather than a wrong guess.
        assert_eq!(images.resolve("/nonexistent/icon.png", 48), None);
        assert_eq!(images.resolve("", 48), None);
    }
}
