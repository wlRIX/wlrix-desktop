// SPDX-License-Identifier: GPL-3.0-or-later
//! Turning an `Icon=` name into pixels.
//!
//! [`wlrix_ui::image::Images`] decodes a *path*. Getting from a name to a path is the
//! icon-theme spec -- `index.theme` parsing, theme inheritance, size and scale directories,
//! the `/usr/share/pixmaps` fallback -- and that is this component's business, because it is
//! this component that reads `.desktop` files. So the lookup stays here and only the decoding
//! and caching are shared.
//!
//! # A theme has to be named
//!
//! `freedesktop_icons::lookup(name)` with no theme searches `hicolor` and `/usr/share/pixmaps`,
//! and most of what a `.desktop` file names is in neither. `Icon=Alacritty` happens to be in
//! `/usr/share/pixmaps` and is what made the themeless call look sufficient; `Icon=firefox`,
//! `Icon=org.gnome.Nautilus` and almost everything else lives in an installed *theme*, so it
//! resolved to nothing and the launcher was drawn as a bare magic carpet with no symbol on it.
//!
//! So [`Icons::set_theme`] names the theme searched first, and the bare lookup stays behind it as
//! the fallback -- `hicolor` is what every theme inherits, and it is where a launcher's own
//! installed icon usually is. The default is Adwaita; see [`crate::config::AppearanceConfig`].
//!
//! Found while building `wlrix-tray`, which hit the same thing: fcitx5 publishes
//! `IconName = "input-keyboard-symbolic"`, which is in every installed theme *except* hicolor.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wlrix_ui::image::{Image, Images};

/// The decoded-image cache, plus the name-to-file lookup in front of it.
#[derive(Default)]
pub struct Icons {
    images: Images,
    /// `Icon=` names already resolved to files, including the ones that resolved to nothing.
    resolved: HashMap<String, Option<PathBuf>>,
    /// The theme searched before `hicolor`. See the module comment.
    theme: String,
}

impl Icons {
    /// Set the icon theme to search first.
    ///
    /// Empty means "no named theme", which is what a user gets from `icon_theme = ""`. Clearing
    /// on a change is not optional: the negative entries in `resolved` are the ones a new theme
    /// would find, so keeping them would make the setting appear to do nothing.
    pub fn set_theme(&mut self, theme: &str) {
        if self.theme != theme {
            self.theme = theme.to_owned();
            self.clear();
        }
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
        let wanted = size.clamp(1, u16::MAX as i32) as u16;
        let found = self
            .in_theme(icon, wanted)
            // No theme named, or the named one does not have it. `hicolor` is the fallback every
            // theme inherits, and the lookup covers `/usr/share/pixmaps` too -- which is where
            // `Icon=Alacritty` actually lives.
            .or_else(|| {
                freedesktop_icons::lookup(icon)
                    .with_size(wanted)
                    .with_cache()
                    .find()
            });
        if found.is_none() {
            eprintln!("wlrix-desktop: no icon found for {icon:?}");
        }
        self.resolved.insert(icon.to_owned(), found.clone());
        found
    }

    /// Look `icon` up in the configured theme, if there is one.
    fn in_theme(&self, icon: &str, size: u16) -> Option<PathBuf> {
        if self.theme.is_empty() {
            return None;
        }
        freedesktop_icons::lookup(icon)
            .with_size(size)
            .with_theme(&self.theme)
            .with_cache()
            .find()
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

    /// Forget everything. Called when the icon size or the theme changes, either of which
    /// invalidates every entry -- including the negative ones, since a name that resolved to
    /// nothing under one theme may resolve under the next.
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
    fn no_named_theme_goes_straight_to_the_fallback() {
        // `icon_theme = ""` has to mean the old behavior exactly, or turning the setting off
        // would be a third thing rather than the way back.
        let icons = Icons::default();
        assert!(icons.theme.is_empty());
        assert_eq!(icons.in_theme("input-keyboard-symbolic", 64), None);
    }

    #[test]
    fn changing_the_theme_forgets_what_the_last_one_could_not_find() {
        // The negative entries are exactly the ones a new theme would resolve, so keeping them
        // would make the setting appear to do nothing at all.
        let mut icons = Icons::default();
        assert_eq!(icons.resolve("wlrix-no-such-icon-anywhere", 64), None);
        assert_eq!(icons.resolved.len(), 1, "the miss was cached");

        icons.set_theme("Adwaita");
        assert!(
            icons.resolved.is_empty(),
            "and dropped when the theme changed"
        );

        // Setting the same theme again is a no-op, so a SIGHUP for an unrelated key does not
        // throw away every decode the desktop is currently drawing from.
        icons.resolve("wlrix-no-such-icon-anywhere", 64);
        icons.set_theme("Adwaita");
        assert_eq!(icons.resolved.len(), 1);
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
