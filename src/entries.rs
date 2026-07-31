// SPDX-License-Identifier: GPL-3.0-or-later
//! What is on the desktop: one entry per file, and enough about each to pick an icon.
//!
//! Deliberately shallow for ordinary files: there is no MIME sniffing, and the kind of a file
//! comes from its metadata and its extension, which is all the drawn-in-code icons need to tell
//! apart. Reading file contents to choose a picture would mean opening everything on the
//! desktop on every rescan, for a distinction the user cannot see.
//!
//! A `.desktop` file is the exception, because it *does* say what it should look like. Those
//! are parsed here, once per rescan, into a [`Launcher`] -- the icon to draw, the icon to draw
//! while running, and the app ids to match a window against.

use std::path::{Path, PathBuf};

use crate::desktop_entry::{DesktopEntry, EntryType};

/// Which icon an entry gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A directory: the folder icon.
    Directory,
    /// A `.desktop` launcher.
    Launcher,
    /// Anything with the execute bit: a script or a binary.
    Executable,
    /// Everything else.
    Plain,
}

/// What a `.desktop` launcher says that the desktop needs in order to draw it.
///
/// Parsed once per rescan rather than per frame -- re-reading every desktop file to paint a
/// hover would be absurd -- and refreshed whenever the directory changes, so editing a
/// launcher or `chmod +x`-ing it takes effect without a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    /// Only `Type=Application` entries stand on a magic carpet; a `Link` has no running state.
    pub is_application: bool,
    /// `Icon=`: the symbol to draw.
    pub icon: Option<String>,
    /// `X-WLRIX-Running-Icon=`: the symbol to draw instead while it is running.
    pub running_icon: Option<String>,
    /// The app ids its windows might use; see [`crate::running`].
    pub identities: Vec<String>,
}

/// One file on the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    /// The file name, and the entry's identity: positions are remembered under it, and
    /// selection and hit-testing name icons by it. Deliberately *not* what the label shows --
    /// see [`label`](Self::label).
    pub name: String,
    /// What the label under the icon reads. The file name, except for a `.desktop` launcher
    /// that gives a `Name`, which is the whole point of a launcher: `mpv.desktop` should read
    /// "mpv Media Player", and "mpv メディアプレイヤー" to someone running under Japanese.
    ///
    /// Kept apart from [`name`](Self::name) rather than replacing it, because identity must not
    /// move when the words do. Two launchers can share a `Name`, and a user changing `LANG`
    /// would otherwise find every launcher had forgotten where it was sitting.
    pub label: String,
    pub kind: Kind,
    /// Set for a `.desktop` file that parses. `None` for everything else, and for a launcher
    /// too broken to read.
    pub launcher: Option<Launcher>,
}

impl Entry {
    /// Classify one path, as [`read`] would. `None` for anything that should not show.
    ///
    /// Public so `examples/explain_open` can ask about a single file without reading a whole
    /// directory to find it.
    pub fn at(path: &Path) -> Option<Self> {
        Self::from_path(path)
    }

    /// Classify one directory entry. `None` for anything that should not show.
    fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_owned();
        // Dotfiles are hidden, as everywhere else on Unix.
        if name.starts_with('.') {
            return None;
        }

        // `metadata` follows symlinks, which is what the user means: a link to a directory
        // should look like a directory. A dangling link has no metadata and is skipped.
        let metadata = std::fs::metadata(path).ok()?;
        let kind = if metadata.is_dir() {
            Kind::Directory
        } else if path.extension().is_some_and(|ext| ext == "desktop") {
            Kind::Launcher
        } else if is_executable(&metadata) {
            Kind::Executable
        } else {
            Kind::Plain
        };

        // Read once here rather than per frame; the watch re-runs this whenever the file
        // changes, so an edited or newly-executable launcher updates on its own.
        let desktop = (kind == Kind::Launcher)
            .then(|| DesktopEntry::from_path(path))
            .flatten();

        // A launcher too broken to parse, or one with no `Name`, falls back to its file name.
        // Better a label reading `app.desktop` than a nameless icon.
        let label = desktop
            .as_ref()
            .and_then(|desktop| desktop.name.clone())
            .unwrap_or_else(|| name.clone());

        let launcher = desktop.map(|desktop| Launcher {
            is_application: desktop.entry_type == EntryType::Application,
            identities: crate::running::identities(&desktop, path),
            icon: desktop.icon,
            running_icon: desktop.running_icon,
        });

        Some(Self {
            path: path.to_path_buf(),
            name,
            label,
            kind,
            launcher,
        })
    }
}

/// Whether any of the three execute bits is set.
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

/// Everything on the desktop, in a stable order.
///
/// Sorted so a rescan produces the same sequence every time: entries without a remembered
/// position are laid out in list order, and an unsorted read would shuffle them whenever the
/// directory's internal order changed. A missing directory is not an error -- the desktop is
/// simply empty until it appears.
///
/// By *label*, so the order on screen is the order of the words underneath the icons; a
/// launcher sorted under a file name nobody is shown looks like no order at all. The file name
/// breaks ties, since two launchers may carry the same `Name` and the sort still has to be
/// total for the layout to be stable.
pub fn read(dir: &Path) -> Vec<Entry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read_dir
        .flatten()
        .filter_map(|entry| Entry::from_path(&entry.path()))
        .collect();
    entries.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.name.cmp(&b.name)));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// A throwaway directory to populate. Named for the test so two can run at once.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wlrix-desktop-test-{test}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("make the scratch directory");
        dir
    }

    #[test]
    fn kinds_are_told_apart() {
        let dir = scratch("kinds");
        fs::create_dir(dir.join("folder")).unwrap();
        fs::write(dir.join("notes.txt"), "hello").unwrap();
        fs::write(dir.join("app.desktop"), "[Desktop Entry]").unwrap();
        let script = dir.join("run.sh");
        fs::write(&script, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let entries = read(&dir);
        let kind = |name: &str| entries.iter().find(|e| e.name == name).map(|e| e.kind);
        assert_eq!(kind("folder"), Some(Kind::Directory));
        assert_eq!(kind("notes.txt"), Some(Kind::Plain));
        assert_eq!(kind("app.desktop"), Some(Kind::Launcher));
        assert_eq!(kind("run.sh"), Some(Kind::Executable));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_launcher_is_labelled_with_its_name_not_its_file_name() {
        let dir = scratch("labels");
        fs::write(
            dir.join("mpv.desktop"),
            "[Desktop Entry]\nType=Application\nName=mpv Media Player\nExec=mpv\n",
        )
        .unwrap();
        // No `Name`, so there is nothing to show but the file name.
        fs::write(
            dir.join("bare.desktop"),
            "[Desktop Entry]\nType=Application\nExec=bare\n",
        )
        .unwrap();
        // Not a desktop file at all, and not a launcher: the file name, as always.
        fs::write(dir.join("notes.txt"), "hello").unwrap();

        let entries = read(&dir);
        let label = |name: &str| {
            entries
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.label.as_str())
        };
        assert_eq!(label("mpv.desktop"), Some("mpv Media Player"));
        assert_eq!(label("bare.desktop"), Some("bare.desktop"));
        assert_eq!(label("notes.txt"), Some("notes.txt"));

        // The identity stays the file name throughout, which is what positions are keyed on.
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"mpv.desktop"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_launcher_still_gets_a_label() {
        // Nothing parses out of this, so there is no `Name` to show and no `Launcher` either.
        // An icon with no words under it would look like a bug in the desktop.
        let dir = scratch("broken-label");
        fs::write(dir.join("junk.desktop"), "not a desktop file at all\n").unwrap();

        let entries = read(&dir);
        assert_eq!(entries[0].label, "junk.desktop");
        assert!(entries[0].launcher.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dotfiles_stay_hidden() {
        let dir = scratch("dotfiles");
        fs::write(dir.join(".hidden"), "x").unwrap();
        fs::write(dir.join("shown"), "x").unwrap();

        let entries = read(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "shown");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn entries_come_back_sorted() {
        // Layout gives unplaced entries cells in list order, so an unstable read would move
        // icons around on every rescan.
        let dir = scratch("sorted");
        for name in ["zebra", "apple", "mango"] {
            fs::write(dir.join(name), "x").unwrap();
        }

        let names: Vec<String> = read(&dir).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["apple", "mango", "zebra"]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sort_follows_the_label_not_the_file_name() {
        // What the user sees is `Apple`, `Mango`, `Zebra`; sorting by file name would put them
        // in an order with no visible logic to it.
        let dir = scratch("sorted-by-label");
        for (file, name) in [("z.desktop", "Apple"), ("a.desktop", "Zebra")] {
            fs::write(
                dir.join(file),
                format!("[Desktop Entry]\nType=Application\nName={name}\nExec=x\n"),
            )
            .unwrap();
        }
        fs::write(dir.join("Mango"), "x").unwrap();

        let labels: Vec<String> = read(&dir).into_iter().map(|e| e.label).collect();
        assert_eq!(labels, ["Apple", "Mango", "Zebra"]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_launchers_sharing_a_name_still_sort_stably() {
        // Labels are not unique -- two copies of a launcher carry the same `Name` -- so the
        // file name breaks the tie and keeps the order total.
        let dir = scratch("same-label");
        for file in ["b.desktop", "a.desktop"] {
            fs::write(
                dir.join(file),
                "[Desktop Entry]\nType=Application\nName=Same\nExec=x\n",
            )
            .unwrap();
        }

        let names: Vec<String> = read(&dir).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["a.desktop", "b.desktop"]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_directory_is_an_empty_desktop() {
        assert!(read(Path::new("/nonexistent/wlrix-desktop-test")).is_empty());
    }
}
