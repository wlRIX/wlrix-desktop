// SPDX-License-Identifier: GPL-3.0-or-later
//! Turning an `Icon=` name into pixels.
//!
//! [`wlrix_ui::image::Images`] decodes a *path*. Getting from a name to a path is the
//! icon-theme spec -- `index.theme` parsing, theme inheritance, size and scale directories,
//! the `/usr/share/pixmaps` fallback -- and that is this component's business, because it is
//! this component that reads `.desktop` files. So the lookup stays here and only the decoding
//! and caching are shared.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wlrix_ui::image::{Image, Images};

/// The decoded-image cache, plus the name-to-file lookup in front of it.
#[derive(Default)]
pub struct Icons {
    images: Images,
    /// `Icon=` names already resolved to files, including the ones that resolved to nothing.
    resolved: HashMap<String, Option<PathBuf>>,
}

impl Icons {
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
        self.images.load(&path, size)
    }

    /// Load artwork the program carries itself, cached under a made-up `key`.
    ///
    /// The magic carpets are `include_bytes!`d rather than read from a data directory, so they
    /// cannot go missing from an installed desktop -- but they still want the same cache as
    /// everything else, since they are re-rasterized on every frame otherwise.
    pub fn load_bytes(&mut self, key: &Path, bytes: &[u8], size: i32) -> Option<&Image> {
        self.images.load_bytes(key, bytes, size)
    }

    /// Forget everything. Called when the icon size changes, which invalidates every entry.
    pub fn clear(&mut self) {
        self.images.clear();
        self.resolved.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_path_resolves_to_itself() {
        let carpet = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("generic.exec.closed.svg");
        let mut icons = Icons::default();
        assert_eq!(
            icons.resolve(&carpet.to_string_lossy(), 48),
            Some(carpet.clone())
        );
        // ...and one that is not there resolves to nothing rather than a wrong guess.
        assert_eq!(icons.resolve("/nonexistent/icon.png", 48), None);
        assert_eq!(icons.resolve("", 48), None);
    }

    #[test]
    fn the_magic_carpets_load() {
        // The two files this whole feature rests on. If they ever stop parsing, every
        // application icon quietly loses its base.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let mut icons = Icons::default();
        let mut drawn = Vec::new();
        for name in ["generic.exec.closed.svg", "generic.exec.open.svg"] {
            let path = dir.join(name);
            let image = icons
                .images
                .load(&path, 64)
                .unwrap_or_else(|| panic!("{name} should decode"));
            assert_eq!((image.width, image.height), (64, 64));
            drawn.push(image.clone());
        }
        // Open and closed must actually differ, or the running state is invisible.
        assert_ne!(drawn[0], drawn[1]);
    }
}
