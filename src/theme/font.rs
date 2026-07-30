// SPDX-License-Identifier: GPL-3.0-or-later
//! Text, rasterized with cosmic-text.
//!
//! IRIX's `FontPalette` asks for Helvetica, which is proprietary. Rather than name a free
//! stand-in and hope it is installed, this asks cosmic-text for a sans-serif face and lets it
//! resolve a concrete family from the system font database. The reason for cosmic-text over a
//! single loaded face is **fallback and shaping**: a filename in Japanese (or any script the
//! primary family lacks) renders through the system's CJK font instead of as missing-glyph
//! boxes, and shaping gives correct kerning and complex-script layout.
//!
//! **Copied from `wlrix-greeter/src/theme/font.rs`, and kept in step by hand.** The two repos
//! build standalone -- there is no shared crate to depend on -- so the drawing primitives are
//! duplicated rather than shared. If a third consumer ever wants them, that is the moment to
//! pull this and its neighbors out into a `wlrix-ui` crate instead of copying a third time.

use std::collections::HashMap;

use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight,
};

use crate::theme::Rgb;
use crate::ui::canvas::Canvas;

/// Line height as a multiple of the font size -- typical sans leading, used when a face's own
/// metrics are unavailable.
const LINE_FACTOR: f32 = 1.3;

/// The faces available. The desktop draws icon labels in `Regular`; the other two come with
/// the module, which is shared verbatim with the greeter (see the note at the top).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    /// Icon labels.
    Regular,
    Bold,
    Italic,
}

/// A text run's fixed parameters: which face and size, where, and in what color.
///
/// Grouped so [`Fonts::draw`] takes one description of the run rather than a long list
/// of positional arguments.
#[derive(Clone, Copy)]
pub struct Run {
    pub face: Face,
    pub px: f32,
    /// The run's left edge.
    pub x: i32,
    /// The baseline the glyphs sit on.
    pub baseline: i32,
    pub colour: Rgb,
}

/// A face's vertical metrics at one size: the top-to-baseline distance and the line box.
#[derive(Clone, Copy)]
struct LineMetrics {
    ascent: i32,
    line_height: i32,
}

/// The desktop's fonts. Shaping and rasterization go through cosmic-text; vertical metrics are
/// cached per (face, size) so the per-frame layout code can ask for them freely.
pub struct Fonts {
    system: FontSystem,
    swash: SwashCache,
    /// Keyed by face and pixel size (as its bit pattern, so it can be hashed).
    metrics: HashMap<(Face, u32), LineMetrics>,
}

impl Fonts {
    /// Ready cosmic-text over the system font database. Errs, rather than drawing nothing, if
    /// the system has no fonts at all -- a login screen that explains why beats a blank one.
    pub fn load() -> Result<Self, String> {
        let mut system = FontSystem::new();
        ensure_system_dirs(system.db_mut());
        if system.db().is_empty() {
            return Err("no fonts found: cosmic-text's system font database is empty".to_string());
        }
        Ok(Self {
            system,
            swash: SwashCache::new(),
            metrics: HashMap::new(),
        })
    }

    /// What the labels are drawn with, for a one-line note at startup.
    pub fn family(&self) -> &str {
        "system sans-serif (cosmic-text)"
    }

    /// How many faces cosmic-text found, for the startup log -- a small number means the
    /// process is not seeing the system font directories.
    pub fn face_count(&self) -> usize {
        self.system.db().len()
    }

    /// Whether `sample` shapes to real glyphs rather than `.notdef` tofu -- i.e. whether the
    /// font environment can render it. Logged at startup for a script (CJK) the base sans
    /// lacks, so a login screen full of boxes is diagnosable from the log alone.
    pub fn can_render(&mut self, sample: &str) -> bool {
        let buffer = self.shape(Face::Regular, 16.0, sample);
        buffer
            .layout_runs()
            .flat_map(|run| run.glyphs)
            .all(|glyph| glyph.glyph_id != 0)
    }

    /// The distance from the top of a line to the baseline, so callers can turn a box
    /// top into a baseline for [`draw`].
    pub fn ascent(&mut self, face: Face, px: f32) -> i32 {
        self.line_metrics(face, px).ascent
    }

    /// A line's natural height, for spacing rows.
    pub fn line_height(&mut self, face: Face, px: f32) -> i32 {
        self.line_metrics(face, px).line_height
    }

    /// How wide `text` is at this face and size, in whole pixels.
    pub fn width(&mut self, face: Face, px: f32, text: &str) -> i32 {
        if text.is_empty() {
            return 0;
        }
        let buffer = self.shape(face, px, text);
        buffer
            .layout_runs()
            .next()
            .map(|run| run.line_w.ceil() as i32)
            .unwrap_or(0)
    }

    /// Draw `text` with the given face, size, position and color. The run is shaped, then each
    /// covered pixel is blended onto the canvas at the run's color -- cosmic-text hands back
    /// coverage in the alpha channel, which is exactly what [`Canvas::blend`] wants.
    pub fn draw(&mut self, canvas: &mut Canvas, run: Run, text: &str) {
        if text.is_empty() {
            return;
        }
        let buffer = self.shape(run.face, run.px, text);
        // Place the buffer so its baseline lands on `run.baseline`. cosmic-text draws from the
        // buffer top; the first line's baseline sits `line_y` below it.
        let baseline = buffer
            .layout_runs()
            .next()
            .map(|line| line.line_y)
            .unwrap_or(run.px);
        let dy = run.baseline - baseline.round() as i32;
        let (r, g, b) = run.colour.channels();
        let colour = run.colour;

        buffer.draw(
            &mut self.system,
            &mut self.swash,
            Color::rgba(r, g, b, 255),
            |x, y, w, h, pixel| {
                let coverage = pixel.a();
                if coverage == 0 {
                    return;
                }
                for row in 0..h as i32 {
                    for col in 0..w as i32 {
                        canvas.blend(run.x + x + col, dy + y + row, colour, coverage);
                    }
                }
            },
        );
    }

    /// Vertical metrics for a face at a size, shaped once from a reference string and cached.
    fn line_metrics(&mut self, face: Face, px: f32) -> LineMetrics {
        let key = (face, px.to_bits());
        if let Some(metrics) = self.metrics.get(&key) {
            return *metrics;
        }
        // A reference with both an ascender and a descender, so the line box is realistic.
        let buffer = self.shape(face, px, "Ag");
        let metrics = buffer
            .layout_runs()
            .next()
            .map(|run| LineMetrics {
                ascent: (run.line_y - run.line_top).ceil() as i32,
                line_height: run.line_height.ceil() as i32,
            })
            .unwrap_or(LineMetrics {
                ascent: px.ceil() as i32,
                line_height: (px * LINE_FACTOR).ceil() as i32,
            });
        self.metrics.insert(key, metrics);
        metrics
    }

    /// Shape one line of `text` into an unwrapped buffer.
    ///
    /// cosmic-text keeps the weight/slant when it falls back for an uncovered glyph, so a CJK
    /// name in the italic full-name face matches a *Latin* italic font (which has no CJK) and
    /// comes out as `.notdef` tofu -- even though an upright CJK face covers it. When a styled
    /// shaping leaves any glyph missing, reshape upright in the regular face, which has the
    /// widest coverage: a readable upright name beats an italic row of boxes. Latin names shape
    /// cleanly in italic and keep it.
    fn shape(&mut self, face: Face, px: f32, text: &str) -> Buffer {
        let buffer = self.shape_with(attrs(face), px, text);
        if face != Face::Regular && has_missing_glyphs(&buffer) {
            return self.shape_with(attrs(Face::Regular), px, text);
        }
        buffer
    }

    /// Shape `text` with explicit attributes into an unwrapped buffer.
    fn shape_with(&mut self, attrs: Attrs, px: f32, text: &str) -> Buffer {
        let metrics = Metrics::new(px, (px * LINE_FACTOR).ceil());
        let mut buffer = Buffer::new(&mut self.system, metrics);
        buffer.set_text(&mut self.system, text, attrs, Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.system, false);
        buffer
    }
}

/// Whether shaping left any glyph as `.notdef` -- i.e. no font in the fallback chain covered it.
fn has_missing_glyphs(buffer: &Buffer) -> bool {
    buffer
        .layout_runs()
        .flat_map(|run| run.glyphs)
        .any(|glyph| glyph.glyph_id == 0)
}

/// Make sure the standard system font directories are in the database.
///
/// cosmic-text's default system scan is fontconfig-driven, and fontconfig resolves its font
/// directories from the environment -- `HOME`, XDG config, `/etc/fonts`. That resolution can
/// come up short in a sparse session environment and drop directories, most visibly the system
/// CJK font, so a non-Latin filename renders as tofu even though the font is installed. (The
/// greeter hit exactly this under greetd, which is where the workaround comes from.) Loading
/// the well-known directories directly makes this independent of that. Directories fontconfig
/// already covered are skipped, so faces are not loaded twice.
fn ensure_system_dirs(db: &mut cosmic_text::fontdb::Database) {
    use cosmic_text::fontdb::Source;

    const DIRS: &[&str] = &["/usr/share/fonts", "/usr/local/share/fonts"];
    for dir in DIRS {
        let path = std::path::Path::new(dir);
        if !path.is_dir() {
            continue;
        }
        let already = db.faces().any(|face| match &face.source {
            Source::File(file) => file.starts_with(path),
            _ => false,
        });
        if !already {
            db.load_fonts_dir(path);
        }
    }
}

/// The cosmic-text attributes for a face: a sans-serif family, plus the weight or slant. The
/// concrete family (and any per-glyph fallback) is left to cosmic-text's font matching.
fn attrs(face: Face) -> Attrs<'static> {
    let base = Attrs::new().family(Family::SansSerif);
    match face {
        Face::Regular => base,
        Face::Bold => base.weight(Weight::BOLD),
        Face::Italic => base.style(Style::Italic),
    }
}
