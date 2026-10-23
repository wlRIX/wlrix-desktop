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
//! installed icon usually is. The default is wlrix; see [`crate::config::AppearanceConfig`].
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
    /// Whether each symbol file looked at is drawn in the carpet's frame; see
    /// [`is_carpet_frame`].
    registered: HashMap<PathBuf, bool>,
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
        let found = self.find(icon, size);
        if found.is_none() && !icon.is_empty() {
            eprintln!("wlrix-desktop: no icon found for {icon:?}");
        }
        found
    }

    /// The first of several candidate names that resolves, for a file type's icon.
    ///
    /// Missing names are not reported one by one: a type's list runs from specific to generic,
    /// and most types have no icon of their own, so `text-markdown` failing on the way to
    /// `text-x-generic` is the list working. Only a list that finds nothing at all is worth a
    /// line, and the caller still draws something then.
    pub fn first(&mut self, icons: &[String], size: i32) -> Option<PathBuf> {
        let found = icons.iter().find_map(|icon| self.find(icon, size));
        if found.is_none() && !icons.is_empty() {
            eprintln!("wlrix-desktop: no icon found for any of {icons:?}");
        }
        found
    }

    /// [`Self::resolve`], without the report.
    fn find(&mut self, icon: &str, size: i32) -> Option<PathBuf> {
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

    /// Load a file already resolved, at `size`.
    pub fn load(&mut self, path: &Path, size: i32) -> Option<&Image> {
        self.images.load(path, size)
    }

    /// Whether the file at `path` is an IRIX drawing in the magic carpet's own frame, to be laid
    /// over the carpet 1:1 rather than shrunk onto it. See [`is_carpet_frame`].
    pub fn is_registered(&mut self, path: &Path) -> bool {
        if let Some(&known) = self.registered.get(path) {
            return known;
        }
        let known = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
            && read_head(path).is_some_and(|head| is_carpet_frame(&head));
        self.registered.insert(path.to_path_buf(), known);
        known
    }

    /// [`Self::first`] and load in one step.
    pub fn get_first(&mut self, icons: &[String], size: i32) -> Option<&Image> {
        let path = self.first(icons, size)?;
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
        self.registered.clear();
    }
}

/// The first few kilobytes of a file, which is where an SVG's root element is.
fn read_head(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut head = Vec::with_capacity(4096);
    std::fs::File::open(path)
        .ok()?
        .take(4096)
        .read_to_end(&mut head)
        .ok()?;
    Some(String::from_utf8_lossy(&head).into_owned())
}

/// Whether an SVG is in the IRIX carpet frame: a 50pt square root over a `0 0 50 50` view box.
///
/// Every IRIX icon export has that root, drawn at `scale(0.5)` in a 100-unit frame -- the magic
/// carpets and an application's own symbol alike. IRIX laid the symbol over its carpet 1:1, in
/// register, which is how a symbol can draw on the carpet itself: the Writer's pencil erases
/// lines written on the carpet's paper. Any other icon, an application's ordinary theme icon, is
/// shrunk and stood on the carpet instead. The icon catalog's `LegacySvg` in wlrix-apps applies
/// the same rule, kept in step by hand.
pub fn is_carpet_frame(svg: &str) -> bool {
    let Some(start) = svg.find("<svg") else {
        return false;
    };
    let Some(end) = svg[start..].find('>') else {
        return false;
    };
    let root = &svg[start..start + end];
    let numbers: Option<Vec<&str>> = attribute(root, "viewBox").map(|view| {
        view.split(|c: char| c.is_whitespace() || c == ',')
            .filter(|part| !part.is_empty())
            .collect()
    });
    attribute(root, "width") == Some("50pt")
        && attribute(root, "height") == Some("50pt")
        && numbers.as_deref() == Some(&["0", "0", "50", "50"][..])
}

/// The value of `name` in an element's attribute text, either quote style.
fn attribute<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = element;
    while let Some(at) = rest.find(name) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + name.len()..].trim_start();
        rest = &rest[at + name.len()..];
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let Some(value) = after.strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        let quote = value.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let value = &value[1..];
        return value.find(quote).map(|close| &value[..close]);
    }
    None
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
    fn an_irix_export_is_in_the_carpet_frame() {
        assert!(is_carpet_frame(
            r#"<svg width="50pt" height="50pt" viewBox="0 0 50 50" xml:space="preserve"><g/></svg>"#
        ));
        assert!(is_carpet_frame(
            "<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox='0,0,50,50' height='50pt' width='50pt'>"
        ));
        // The carpets themselves are, which is the frame everything is registered against.
        assert!(is_carpet_frame(include_str!(
            "../assets/generic.exec.closed.svg"
        )));
    }

    #[test]
    fn anything_else_is_not() {
        for svg in [
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48"></svg>"#,
            r#"<svg width="50pt" height="50pt" viewBox="0 0 100 100"></svg>"#,
            r#"<svg width="50px" height="50px" viewBox="0 0 50 50"></svg>"#,
            r#"<svg viewBox="0 0 50 50"></svg>"#,
            // `stroke-width` is not `width`.
            r#"<svg stroke-width="50pt" height="50pt" viewBox="0 0 50 50"></svg>"#,
            "not svg at all",
        ] {
            assert!(!is_carpet_frame(svg), "{svg}");
        }
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
