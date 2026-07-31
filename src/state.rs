// SPDX-License-Identifier: GPL-3.0-or-later
//! Where the icons were left: the machine-written half of the settings.
//!
//! ```toml
//! # $XDG_STATE_HOME/wlrix/desktop-icons.toml
//! snap_to_grid = true
//!
//! [[icon]]
//! name = "notes.txt"
//! slot = 3
//!
//! [[icon]]
//! name = "photo.png"
//! x = 412
//! y = 260
//! ```
//!
//! Two files describe the desktop, in the same shape the compositor uses for displays (see
//! its `outputs.rs`): [`crate::config`] is hand-edited and holds *defaults*, this one is
//! written by the program and holds *what is true right now*. They merge per field with this
//! one winning, so a snap toggle flipped at runtime outlives a restart while a hand-set cell
//! size stays hand-set.
//!
//! An icon is keyed by file name, not path: the desktop directory can move -- or the user can
//! point `XDG_DESKTOP_DIR` somewhere else -- without every icon forgetting its place.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::entries::Entry;
use crate::layout::{Grid, Placed, Point, Spot};

/// The state file as it is written.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StateFile {
    /// The live snap setting. Absent on the first run, when the config's default stands.
    #[serde(skip_serializing_if = "Option::is_none")]
    snap_to_grid: Option<bool>,
    #[serde(default, rename = "icon", skip_serializing_if = "Vec::is_empty")]
    icons: Vec<IconState>,
}

/// One icon's remembered position. Either a slot or a free point, never both -- but the file
/// is not trusted to say so, and [`IconState::spot`] resolves a malformed entry rather than
/// refusing to load the rest.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IconState {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    y: Option<i32>,
}

impl IconState {
    fn spot(&self) -> Option<Spot> {
        // A slot wins if both are present: it is the more specific claim, and an entry with
        // both is a file someone edited by hand.
        if let Some(slot) = self.slot.filter(|slot| *slot >= 0) {
            return Some(Spot::Slot(slot));
        }
        match (self.x, self.y) {
            (Some(x), Some(y)) => Some(Spot::Free(Point::new(x, y))),
            _ => None,
        }
    }

    fn from_spot(name: &str, spot: Spot) -> Self {
        match spot {
            Spot::Slot(slot) => Self {
                name: name.to_owned(),
                slot: Some(slot),
                x: None,
                y: None,
            },
            Spot::Free(point) => Self {
                name: name.to_owned(),
                slot: None,
                x: Some(point.x),
                y: Some(point.y),
            },
        }
    }
}

/// The remembered desktop, in memory.
#[derive(Debug, Default, Clone)]
pub struct State {
    /// Keyed by file name. A `BTreeMap` so the file is written in a stable order and a diff
    /// of it shows only what actually moved.
    spots: BTreeMap<String, Spot>,
    snap_to_grid: Option<bool>,
    /// Set when something has changed and the file is due a rewrite.
    dirty: bool,
}

impl State {
    /// Read the state file. A missing or broken one yields an empty state: like the config,
    /// a bad state file is reported and ignored rather than being fatal.
    pub fn load() -> Self {
        let Some(path) = crate::xdg::state_path() else {
            return Self::default();
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(err) => {
                eprintln!("wlrix-desktop: could not read {}: {err}", path.display());
                return Self::default();
            }
        };
        match toml::from_str::<StateFile>(&text) {
            Ok(file) => Self::from_file(file),
            Err(err) => {
                eprintln!("wlrix-desktop: {} is not valid: {err}", path.display());
                Self::default()
            }
        }
    }

    fn from_file(file: StateFile) -> Self {
        let spots = file
            .icons
            .iter()
            .filter_map(|icon| icon.spot().map(|spot| (icon.name.clone(), spot)))
            .collect();
        Self {
            spots,
            snap_to_grid: file.snap_to_grid,
            dirty: false,
        }
    }

    fn to_file(&self) -> StateFile {
        StateFile {
            snap_to_grid: self.snap_to_grid,
            icons: self
                .spots
                .iter()
                .map(|(name, spot)| IconState::from_spot(name, *spot))
                .collect(),
        }
    }

    /// Where `name` was left, if anywhere.
    pub fn spot(&self, name: &str) -> Option<Spot> {
        self.spots.get(name).copied()
    }

    /// Lay `entries` out on `grid`, and remember where every one of them landed.
    ///
    /// The pinning is deliberately *here* rather than at the call site, because leaving it to
    /// the caller is a real bug with a confusing symptom. An icon whose position is only
    /// implied by its place in the list is not placed at all: `arrange` hands it the next free
    /// cell every time, so moving or deleting the icon above it makes everything below slide
    /// up to close the gap. Pinning what the first layout decided is what makes a gap stay a
    /// gap.
    pub fn arrange(&mut self, grid: &Grid, entries: &[Entry]) -> Vec<Placed> {
        // Scoped so the read borrow ends before the write-back below.
        let placed = {
            let spots = &self.spots;
            crate::layout::arrange(grid, entries, &|name| spots.get(name).copied())
        };
        for item in &placed {
            self.set_spot(&item.entry.name, item.spot);
        }
        placed
    }

    /// Remember where `name` is now.
    pub fn set_spot(&mut self, name: &str, spot: Spot) {
        if self.spots.get(name) == Some(&spot) {
            return;
        }
        self.spots.insert(name.to_owned(), spot);
        self.dirty = true;
    }

    /// The live snap setting, falling back to the config's default on a first run.
    pub fn snap_to_grid(&self, default: bool) -> bool {
        self.snap_to_grid.unwrap_or(default)
    }

    pub fn set_snap_to_grid(&mut self, snap: bool) {
        if self.snap_to_grid == Some(snap) {
            return;
        }
        self.snap_to_grid = Some(snap);
        self.dirty = true;
    }

    /// Whether anything has changed since the last save.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Drop positions for files that are no longer on the desktop.
    ///
    /// Without this the file grows forever. It is a trade: a file deleted and restored later
    /// loses its place, which is the right way round -- remembering every file that ever sat
    /// on the desktop is worse than forgetting one that left.
    pub fn retain_only(&mut self, present: &[String]) {
        let before = self.spots.len();
        self.spots.retain(|name, _| present.contains(name));
        if self.spots.len() != before {
            self.dirty = true;
        }
    }

    /// Write the state out, if anything changed.
    ///
    /// Best-effort: a missing state directory or an I/O error is reported and swallowed --
    /// losing icon positions is not worth failing over. The file is replaced atomically, so
    /// a crash mid-write cannot leave a half-written file the next start would reject.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = crate::xdg::state_path() else {
            eprintln!("wlrix-desktop: no state directory; not saving icon positions");
            // Marked clean regardless, so a machine with no state directory does not retry
            // on every change for the life of the process.
            self.dirty = false;
            return;
        };
        let text = match toml::to_string(&self.to_file()) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("wlrix-desktop: could not serialize icon positions: {err}");
                return;
            }
        };
        match write_replace(&path, text.as_bytes()) {
            Ok(()) => self.dirty = false,
            Err(err) => eprintln!("wlrix-desktop: could not write {}: {err}", path.display()),
        }
    }
}

/// Replace a file atomically: write a sibling temp file, then rename over the target,
/// creating the parent directory if need be.
fn write_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(state: &State) -> State {
        let text = toml::to_string(&state.to_file()).expect("serialize");
        State::from_file(toml::from_str(&text).expect("deserialize"))
    }

    fn grid() -> Grid {
        Grid::new(
            crate::layout::Rect::new(0, 0, 1280, 800),
            crate::layout::Metrics::default(),
        )
    }

    fn entry(name: &str) -> Entry {
        Entry {
            path: format!("/desktop/{name}").into(),
            name: name.to_owned(),
            label: name.to_owned(),
            kind: crate::entries::Kind::Plain,
            launcher: None,
        }
    }

    #[test]
    fn laying_out_pins_every_icon_not_just_the_dragged_ones() {
        // The contract that makes the three tests below work at all.
        let mut state = State::default();
        let entries = [entry("a"), entry("b"), entry("c")];
        state.arrange(&grid(), &entries);
        assert_eq!(state.spot("a"), Some(Spot::Slot(0)));
        assert_eq!(state.spot("b"), Some(Spot::Slot(1)));
        assert_eq!(state.spot("c"), Some(Spot::Slot(2)));
    }

    #[test]
    fn moving_one_icon_leaves_a_gap_instead_of_pulling_the_others_up() {
        // The bug from the KVM: only dragged icons had a remembered spot, so the untouched
        // ones below were re-handed the first free cells and slid up into the vacancy.
        let mut state = State::default();
        let entries = [entry("a"), entry("b"), entry("c"), entry("d")];
        let grid = grid();
        state.arrange(&grid, &entries);

        // Drag `b` off to a free position, as a drop would.
        state.set_spot("b", Spot::Free(Point::new(400, 300)));
        let after = state.arrange(&grid, &entries);

        assert_eq!(after[0].spot, Spot::Slot(0), "a should not have moved");
        assert_eq!(after[2].spot, Spot::Slot(2), "c should not have slid up");
        assert_eq!(after[3].spot, Spot::Slot(3), "d should not have slid up");
        assert_eq!(after[1].spot, Spot::Free(Point::new(400, 300)));
    }

    #[test]
    fn deleting_a_file_leaves_a_gap_too() {
        // Same failure, different trigger: cell 1 falling vacant must not pull 2 and 3 up.
        let mut state = State::default();
        let grid = grid();
        state.arrange(&grid, &[entry("a"), entry("b"), entry("c"), entry("d")]);

        let survivors = [entry("a"), entry("c"), entry("d")];
        state.retain_only(&survivors.iter().map(|e| e.name.clone()).collect::<Vec<_>>());
        let after = state.arrange(&grid, &survivors);

        assert_eq!(after[0].spot, Spot::Slot(0));
        assert_eq!(after[1].spot, Spot::Slot(2), "c should stay where it was");
        assert_eq!(after[2].spot, Spot::Slot(3), "d should stay where it was");
    }

    #[test]
    fn a_new_file_takes_a_free_cell_without_disturbing_anything() {
        // `aa` sorts between `a` and `b`, so unpinned it would take cell 1 and push `b` and
        // `c` down a cell each.
        let mut state = State::default();
        let grid = grid();
        state.arrange(&grid, &[entry("a"), entry("b"), entry("c")]);

        let grown = [entry("a"), entry("aa"), entry("b"), entry("c")];
        let after = state.arrange(&grid, &grown);
        assert_eq!(after[0].spot, Spot::Slot(0), "a stays");
        assert_eq!(after[2].spot, Spot::Slot(1), "b stays");
        assert_eq!(after[3].spot, Spot::Slot(2), "c stays");
        assert_eq!(after[1].spot, Spot::Slot(3), "aa takes the first free cell");
    }

    #[test]
    fn laying_out_the_same_desktop_twice_changes_nothing() {
        // Pinning must not make every redraw dirty the state file.
        let mut state = State::default();
        let grid = grid();
        let entries = [entry("a"), entry("b")];
        state.arrange(&grid, &entries);
        let mut state = round_trip(&state);

        state.arrange(&grid, &entries);
        assert!(
            !state.is_dirty(),
            "an unchanged layout should not be rewritten"
        );
    }

    #[test]
    fn slots_and_free_points_survive_a_round_trip() {
        let mut state = State::default();
        state.set_spot("notes.txt", Spot::Slot(3));
        state.set_spot("photo.png", Spot::Free(Point::new(412, 260)));
        state.set_snap_to_grid(true);

        let back = round_trip(&state);
        assert_eq!(back.spot("notes.txt"), Some(Spot::Slot(3)));
        assert_eq!(
            back.spot("photo.png"),
            Some(Spot::Free(Point::new(412, 260)))
        );
        assert!(back.snap_to_grid(false));
    }

    #[test]
    fn an_unset_snap_falls_back_to_the_config() {
        // First run: the state file has no opinion, so the hand-edited default stands.
        let state = State::default();
        assert!(state.snap_to_grid(true));
        assert!(!state.snap_to_grid(false));
    }

    #[test]
    fn a_set_snap_beats_the_config() {
        let mut state = State::default();
        state.set_snap_to_grid(false);
        assert!(!state.snap_to_grid(true), "the state file should win");
    }

    #[test]
    fn only_real_changes_mark_it_dirty() {
        let mut state = State::default();
        assert!(!state.is_dirty());
        state.set_spot("a", Spot::Slot(1));
        assert!(state.is_dirty());

        let mut state = round_trip(&state);
        assert!(!state.is_dirty(), "a freshly loaded state is clean");
        // Setting the same value again is not a change, so a redraw that re-asserts every
        // position does not rewrite the file.
        state.set_spot("a", Spot::Slot(1));
        assert!(!state.is_dirty());
        state.set_spot("a", Spot::Slot(2));
        assert!(state.is_dirty());
    }

    #[test]
    fn positions_for_departed_files_are_dropped() {
        let mut state = State::default();
        state.set_spot("stays", Spot::Slot(0));
        state.set_spot("goes", Spot::Slot(1));
        let mut state = round_trip(&state);

        state.retain_only(&["stays".to_owned()]);
        assert_eq!(state.spot("stays"), Some(Spot::Slot(0)));
        assert_eq!(state.spot("goes"), None);
        assert!(state.is_dirty());
    }

    #[test]
    fn retaining_everything_changes_nothing() {
        let mut state = State::default();
        state.set_spot("a", Spot::Slot(0));
        let mut state = round_trip(&state);
        state.retain_only(&["a".to_owned()]);
        assert!(!state.is_dirty(), "no change means no rewrite");
    }

    #[test]
    fn a_malformed_entry_is_skipped_not_fatal() {
        // Half a free position, and a negative slot: neither is usable, but the rest of the
        // file must still load.
        let file: StateFile = toml::from_str(
            "[[icon]]\nname = \"broken\"\nx = 10\n\
             \n[[icon]]\nname = \"negative\"\nslot = -1\n\
             \n[[icon]]\nname = \"fine\"\nslot = 2\n",
        )
        .expect("parse");
        let state = State::from_file(file);
        assert_eq!(state.spot("broken"), None);
        assert_eq!(state.spot("negative"), None);
        assert_eq!(state.spot("fine"), Some(Spot::Slot(2)));
    }

    #[test]
    fn a_slot_wins_over_a_stray_free_point() {
        let file: StateFile =
            toml::from_str("[[icon]]\nname = \"both\"\nslot = 5\nx = 1\ny = 2\n").unwrap();
        let state = State::from_file(file);
        assert_eq!(state.spot("both"), Some(Spot::Slot(5)));
    }

    #[test]
    fn the_file_is_written_in_a_stable_order() {
        // So a diff of the state file shows only what moved, not a reshuffle.
        let mut state = State::default();
        for name in ["zebra", "apple", "mango"] {
            state.set_spot(name, Spot::Slot(0));
        }
        let text = toml::to_string(&state.to_file()).unwrap();
        let apple = text.find("apple").unwrap();
        let mango = text.find("mango").unwrap();
        let zebra = text.find("zebra").unwrap();
        assert!(apple < mango && mango < zebra);
    }
}
