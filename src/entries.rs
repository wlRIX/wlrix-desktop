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
    /// What the label shows, and the key positions are remembered under. The file name, so
    /// moving the desktop directory keeps every icon where the user left it.
    pub name: String,
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
        let launcher = (kind == Kind::Launcher)
            .then(|| DesktopEntry::from_path(path))
            .flatten()
            .map(|desktop| Launcher {
                is_application: desktop.entry_type == EntryType::Application,
                identities: crate::running::identities(&desktop, path),
                icon: desktop.icon,
                running_icon: desktop.running_icon,
            });

        Some(Self {
            path: path.to_path_buf(),
            name,
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
/// Sorted by name so a rescan produces the same sequence every time: entries without a
/// remembered position are laid out in list order, and an unsorted read would shuffle them
/// whenever the directory's internal order changed. A missing directory is not an error --
/// the desktop is simply empty until it appears.
pub fn read(dir: &Path) -> Vec<Entry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read_dir
        .flatten()
        .filter_map(|entry| Entry::from_path(&entry.path()))
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
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
    fn a_missing_directory_is_an_empty_desktop() {
        assert!(read(Path::new("/nonexistent/wlrix-desktop-test")).is_empty());
    }
}
