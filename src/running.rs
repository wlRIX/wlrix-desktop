// SPDX-License-Identifier: GPL-3.0-or-later
//! Which applications are running, and so which magic carpets stand open.
//!
//! Two sources, because neither alone is right:
//!
//! - **Windows**, from `ext-foreign-toplevel-list-v1`. This is the honest answer -- it counts
//!   what is actually on screen, however it was started, and it notices an application quitting
//!   without anyone telling us. But it says nothing during the second or two between a
//!   double-click and the first window mapping.
//! - **Processes we launched**, which cover exactly that gap. Not sufficient on their own: most
//!   `Exec` lines run a wrapper that execs the real program and exits, and an application
//!   started from the toolchest or a terminal was never ours to begin with.
//!
//! A launcher is running if either says so.
//!
//! ## Matching a window to a launcher
//!
//! A window carries an `app_id`; a `.desktop` file does not, quite. The spec's convention is
//! that `app_id` equals the desktop file's basename, and `StartupWMClass` exists for the
//! applications that break it -- though only 17 of the 193 files on this machine declare one.
//! So three candidates are tried, most reliable first, and compared case-insensitively because
//! `Alacritty` and `alacritty` are the same application by any sensible reading.

use std::collections::HashMap;
use std::path::Path;

use crate::desktop_entry::DesktopEntry;

/// The names a launcher's windows might appear under, lowercased.
///
/// Three, in descending order of how much they can be trusted:
///
/// 1. `StartupWMClass`, which is the application saying so itself.
/// 2. The desktop file's basename, which is the convention `app_id` is *supposed* to follow.
/// 3. The `Exec` line's program name, which is a guess -- but a good one, since an application
///    that sets neither of the above usually names its window after its binary.
pub fn identities(entry: &DesktopEntry, path: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut push = |name: &str| {
        let name = name.trim().to_lowercase();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    };

    if let Some(class) = entry.startup_wm_class.as_deref() {
        push(class);
    }
    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
        push(stem);
    }
    if let Some(program) = entry
        .exec
        .as_deref()
        .and_then(|exec| exec.split_whitespace().next())
        .and_then(|program| Path::new(program).file_name())
        .and_then(|program| program.to_str())
    {
        push(program);
    }
    names
}

/// The application ids with a window open right now, lowercased.
#[derive(Debug, Default, Clone)]
pub struct Running {
    /// Keyed by the toplevel handle's protocol id, since one application may have several
    /// windows and each closes separately.
    windows: HashMap<u32, String>,
}

impl Running {
    /// Record or update a window's application id.
    pub fn window(&mut self, id: u32, app_id: &str) {
        self.windows.insert(id, app_id.trim().to_lowercase());
    }

    /// A window has closed.
    pub fn closed(&mut self, id: u32) {
        self.windows.remove(&id);
    }

    /// Whether any open window matches one of `identities`.
    pub fn any(&self, identities: &[String]) -> bool {
        self.windows
            .values()
            .any(|app_id| identities.iter().any(|name| name == app_id))
    }

    /// How many windows are being tracked. For the startup log and tests.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(text: &str) -> DesktopEntry {
        DesktopEntry::parse(text).expect("should parse")
    }

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/desktop/{name}"))
    }

    #[test]
    fn startup_wm_class_is_preferred() {
        let entry =
            entry("[Desktop Entry]\nType=Application\nExec=alacritty\nStartupWMClass=Alacritty\n");
        let names = identities(&entry, &path("Alacritty.desktop"));
        assert_eq!(names.first().map(String::as_str), Some("alacritty"));
    }

    #[test]
    fn the_file_basename_and_exec_are_the_fallbacks() {
        // No StartupWMClass, which is the common case: 176 of 193 files on this machine.
        let entry = entry("[Desktop Entry]\nType=Application\nExec=/usr/bin/foot --title x\n");
        let names = identities(&entry, &path("org.example.Foot.desktop"));
        assert!(names.contains(&"org.example.foot".to_string()), "{names:?}");
        assert!(names.contains(&"foot".to_string()), "{names:?}");
    }

    #[test]
    fn identities_are_lowercased_and_deduplicated() {
        let entry =
            entry("[Desktop Entry]\nType=Application\nExec=Alacritty\nStartupWMClass=ALACRITTY\n");
        let names = identities(&entry, &path("alacritty.desktop"));
        assert_eq!(names, ["alacritty"], "all three names are the same one");
    }

    #[test]
    fn an_entry_with_nothing_to_go_on_yields_only_its_filename() {
        let entry = entry("[Desktop Entry]\nType=Application\n");
        assert_eq!(identities(&entry, &path("thing.desktop")), ["thing"]);
    }

    #[test]
    fn a_matching_window_counts_as_running() {
        let mut running = Running::default();
        assert!(!running.any(&["alacritty".to_string()]));

        running.window(1, "Alacritty");
        assert!(
            running.any(&["alacritty".to_string()]),
            "case should not matter"
        );
        assert!(!running.any(&["firefox".to_string()]));
    }

    #[test]
    fn closing_the_last_window_stops_it_counting() {
        let mut running = Running::default();
        running.window(1, "alacritty");
        running.window(2, "alacritty");

        // Two windows of the same application: closing one is not closing the application.
        running.closed(1);
        assert!(running.any(&["alacritty".to_string()]));
        running.closed(2);
        assert!(!running.any(&["alacritty".to_string()]));
    }

    #[test]
    fn any_of_the_identities_is_enough() {
        // The window used the Exec name while the file is named something else entirely.
        let entry = entry("[Desktop Entry]\nType=Application\nExec=foot\n");
        let names = identities(&entry, &path("org.example.Terminal.desktop"));

        let mut running = Running::default();
        running.window(1, "foot");
        assert!(running.any(&names));
    }

    #[test]
    fn an_unrelated_window_does_not_light_up_a_launcher() {
        let entry = entry("[Desktop Entry]\nType=Application\nExec=htop\n");
        let names = identities(&entry, &path("htop.desktop"));

        let mut running = Running::default();
        running.window(1, "org.mozilla.firefox");
        assert!(!running.any(&names));
    }
}
