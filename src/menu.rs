// SPDX-License-Identifier: GPL-3.0-or-later
//! The desktop menu: the 4Dwm menu posted by right-clicking the desktop.
//!
//! This module owns the contents, the layout and the hit-testing; the drawing lives in
//! [`crate::ui::paint`] and the pointer wiring in [`crate::ui`]. Same split, and the same
//! geometry, as the compositor's window menu (`wlrix-compositor/src/menu.rs`) -- the two menus
//! are the same object in the user's eyes and should not drift apart.
//!
//! Unlike the window menu this one has a **header**: a centered "Desktop" title with a groove
//! under it, as IRIX's did.
//!
//! Which items can be chosen depends on the selection, and the enabled flags are worked out
//! once, when the menu is posted. What is drawn and what a click does therefore cannot
//! disagree -- and a selection that changes while the menu is open cannot leave a row that
//! looks disabled but acts, or the reverse.
//!
//! ## Launcher actions
//!
//! A `.desktop` file can offer more than one way in -- Steam's lists Store, Library, Friends
//! and the rest as `[Desktop Action …]` groups. When **exactly one** launcher is selected and
//! it has any, they are added below the fixed items, after a third separator.
//!
//! Exactly one, because an action belongs to a particular file: "Store" means nothing applied
//! to three selected icons at once, and offering it would be offering to do something the
//! desktop cannot describe.

use crate::entries::LauncherAction;
use crate::layout::{Point, Rect};

/// Height of an ordinary item row.
const ITEM_H: i32 = 22;
/// Height of a separator row.
const SEPARATOR_H: i32 = 7;
/// Height of the title row at the top.
const HEADER_H: i32 = 24;
/// The narrowest the panel gets: wide enough for the longest fixed label, "Change Permissions".
///
/// It used to be the only width, because every label was known at compile time. An action's
/// label is neither -- it comes from a file, in the user's language -- so the panel is now
/// measured and this is the floor, which keeps the menu identical when there are no actions.
const WIDTH: i32 = 186;
/// Margin between the panel edge and the rows.
const MARGIN: i32 = 3;
/// Left inset of an item's label.
pub const LABEL_INSET: i32 = 14;
/// Label size in logical pixels.
pub const LABEL_PX: f32 = 14.0;
/// Bevel thickness of the panel and of a highlighted row.
pub const BEVEL: i32 = 2;

/// What choosing an item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// End the session and return to the greeter.
    LogOut,
    /// Open every selected item, as a double-click would.
    Open,
    /// Duplicate the selection. Not implemented yet, so always disabled.
    MakeCopy,
    /// Make a symlink to the selection. Not implemented yet, so always disabled.
    MakeReference,
    /// Move the selection to the trash.
    Remove,
    /// Edit the selection's permissions. Not implemented yet, so always disabled.
    ChangePermissions,
    /// Create a directory on the desktop. Not implemented yet, so always disabled.
    AddNewDirectory,
    /// Select every icon.
    SelectAll,
    /// Run one of the selected launcher's `[Desktop Action …]` groups, by its position in
    /// [`Menu::actions`]. Ask [`Menu::action_target`] which file and which id that is.
    ///
    /// A position rather than the id itself so this stays `Copy` and the menu keeps one copy
    /// of the strings.
    Run(usize),
}

/// What a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The title at the top. Not choosable.
    Header,
    /// An etched groove between groups. Not choosable.
    Separator,
    /// Something that can be chosen, if enabled.
    Item(Action),
}

/// One row of the menu.
///
/// The label is owned rather than `&'static str`: most rows are still fixed text, but an
/// action's comes out of a file at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    kind: Kind,
    pub label: String,
    pub enabled: bool,
    height: i32,
}

impl Entry {
    fn header(label: &str) -> Self {
        Self {
            kind: Kind::Header,
            label: label.to_owned(),
            enabled: false,
            height: HEADER_H,
        }
    }

    fn separator() -> Self {
        Self {
            kind: Kind::Separator,
            label: String::new(),
            enabled: false,
            height: SEPARATOR_H,
        }
    }

    fn item(action: Action, label: &str, enabled: bool) -> Self {
        Self {
            kind: Kind::Item(action),
            label: label.to_owned(),
            enabled,
            height: ITEM_H,
        }
    }

    /// Whether this row is the title, drawn centered with a groove beneath it.
    pub fn is_header(&self) -> bool {
        self.kind == Kind::Header
    }

    /// Whether this row is a groove rather than something to choose.
    pub fn is_separator(&self) -> bool {
        self.kind == Kind::Separator
    }

    /// The action this row would take, if it is an enabled item.
    pub fn action(&self) -> Option<Action> {
        match self.kind {
            Kind::Item(action) => Some(action),
            _ => None,
        }
    }
}

/// The launcher whose actions a menu is offering.
///
/// Carries the file's identity as well as its actions, so choosing a row says *which* file to
/// run it from without the caller having to work out the selection again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actions<'a> {
    /// The entry the actions belong to, by the name the desktop knows it under.
    pub entry: &'a str,
    pub items: &'a [LauncherAction],
}

/// A posted desktop menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    /// The panel's top-left, already clamped onto the surface.
    origin: Point,
    /// Measured when the menu was built, since an action's label is not known until then.
    width: i32,
    pub entries: Vec<Entry>,
    /// The row the pointer is over, if it is over a choosable one.
    pub hovered: Option<usize>,
    /// The launcher the [`Action::Run`] rows belong to, and their ids in row order.
    actions: Option<(String, Vec<String>)>,
}

impl Menu {
    /// Build the menu, with each item enabled according to what is selected.
    ///
    /// `selected` is how many icons are picked: Open and Remove act on the selection, so with
    /// nothing selected there is nothing for them to do. `actions` is the selected launcher's
    /// `[Desktop Action …]` groups, when exactly one launcher is selected and it has any.
    ///
    /// `measure` gives the width of a label. Passed in rather than measured here so this module
    /// stays free of font machinery and its hit-testing stays a matter of arithmetic -- and so
    /// a test can size the panel without loading a font.
    pub fn new(
        origin: Point,
        selected: usize,
        actions: Option<Actions>,
        mut measure: impl FnMut(&str) -> i32,
    ) -> Self {
        let has_selection = selected > 0;
        let mut entries = vec![
            Entry::header("Desktop"),
            Entry::item(Action::LogOut, "Log Out", true),
            Entry::separator(),
            Entry::item(Action::Open, "Open", has_selection),
            // The three below are the menu's shape from IRIX, kept so the layout is right and
            // the work to come has somewhere to land. Disabled until they do something.
            Entry::item(Action::MakeCopy, "Make Copy", false),
            Entry::item(Action::MakeReference, "Make Reference", false),
            Entry::item(Action::Remove, "Remove", has_selection),
            Entry::separator(),
            Entry::item(Action::ChangePermissions, "Change Permissions", false),
            Entry::item(Action::AddNewDirectory, "Add New Directory", false),
            Entry::item(Action::SelectAll, "Select All", true),
        ];

        // The launcher's own items go last, behind a separator of their own: they belong to one
        // file, and the rows above belong to the desktop.
        let actions = actions
            .filter(|actions| !actions.items.is_empty())
            .map(|actions| {
                entries.push(Entry::separator());
                for (index, item) in actions.items.iter().enumerate() {
                    entries.push(Entry::item(Action::Run(index), &item.name, true));
                }
                (
                    actions.entry.to_owned(),
                    actions.items.iter().map(|item| item.id.clone()).collect(),
                )
            });

        let width = width_for(&entries, &mut measure);
        Self {
            origin,
            width,
            entries,
            hovered: None,
            actions,
        }
    }

    /// Which file and which `[Desktop Action …]` id an [`Action::Run`] row means.
    pub fn action_target(&self, index: usize) -> Option<(&str, &str)> {
        let (entry, ids) = self.actions.as_ref()?;
        Some((entry, ids.get(index)?))
    }

    /// The panel rectangle, bevel included.
    pub fn panel(&self) -> Rect {
        panel_rect(&self.entries, self.origin, self.width)
    }

    /// The rectangle of row `index`, inset from the panel edges.
    pub fn row(&self, index: usize) -> Rect {
        row_rect(&self.entries, self.origin, self.width, index)
    }

    /// The row under `point`, whether or not it can be chosen. `None` outside the panel.
    fn row_at(&self, point: Point) -> Option<usize> {
        if !self.panel().contains(point) {
            return None;
        }
        (0..self.entries.len()).find(|&index| self.row(index).contains(point))
    }

    /// The action `point` would choose: a row that is both an item and enabled.
    pub fn action_at(&self, point: Point) -> Option<Action> {
        let entry = self.entries.get(self.row_at(point)?)?;
        entry.enabled.then(|| entry.action()).flatten()
    }

    /// Whether `point` is anywhere on the panel, so a press there belongs to the menu.
    pub fn contains(&self, point: Point) -> bool {
        self.panel().contains(point)
    }

    /// Track the pointer, highlighting the row it is over. Returns whether the highlight
    /// moved, so the caller can avoid redrawing for nothing.
    pub fn hover(&mut self, point: Point) -> bool {
        let hovered = self
            .row_at(point)
            .filter(|&index| self.entries[index].enabled);
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        changed
    }

    /// Place a menu so it fits inside `area`, preferring `at` as its top-left.
    ///
    /// A menu posted near the bottom-right corner would otherwise run off the surface, and
    /// the rows past the edge could never be reached.
    pub fn clamp(at: Point, panel: Rect, area: Rect) -> Point {
        let x = at.x.min(area.x + area.w - panel.w).max(area.x);
        let y = at.y.min(area.y + area.h - panel.h).max(area.y);
        Point::new(x, y)
    }

    /// The panel size this menu would have, for placing it before it is built.
    pub fn size_for(
        selected: usize,
        actions: Option<Actions>,
        measure: impl FnMut(&str) -> i32,
    ) -> Rect {
        Menu::new(Point::new(0, 0), selected, actions, measure).panel()
    }
}

/// How wide the panel has to be for these rows.
///
/// [`WIDTH`] is the floor, so a menu with no action rows is laid out exactly as it always was.
/// Anything longer widens the panel rather than being cut: an action's label is a translated
/// string out of a file, and "Change Permissions" is no bound on it at all.
///
/// The inset is counted on both sides so a long label does not sit flush against the right
/// bevel, even though only the left one is drawn from.
fn width_for(entries: &[Entry], measure: &mut impl FnMut(&str) -> i32) -> i32 {
    let widest = entries
        .iter()
        .map(|entry| measure(&entry.label))
        .max()
        .unwrap_or(0);
    WIDTH.max(widest + 2 * (MARGIN + LABEL_INSET))
}

/// The panel rectangle for a set of rows placed at `origin`.
///
/// A free function so the geometry can be tested without a menu, and so `size_for` can ask
/// for it before there is one.
fn panel_rect(entries: &[Entry], origin: Point, width: i32) -> Rect {
    let height: i32 = entries.iter().map(|entry| entry.height).sum();
    Rect::new(origin.x, origin.y, width, height + 2 * MARGIN)
}

/// The rectangle of row `index`, inset from the panel edges by the margin.
fn row_rect(entries: &[Entry], origin: Point, width: i32, index: usize) -> Rect {
    let top: i32 = entries.iter().take(index).map(|entry| entry.height).sum();
    let height = entries.get(index).map(|entry| entry.height).unwrap_or(0);
    Rect::new(
        origin.x + MARGIN,
        origin.y + MARGIN + top,
        width - 2 * MARGIN,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the font: a fixed width per character.
    ///
    /// Enough to test that a long label widens the panel, without loading a font -- and
    /// narrow enough that every fixed label still fits inside [`WIDTH`], so the menu tested
    /// here is laid out exactly as the real one is.
    fn measure(label: &str) -> i32 {
        label.chars().count() as i32 * 8
    }

    fn menu(selected: usize) -> Menu {
        Menu::new(Point::new(100, 100), selected, None, measure)
    }

    /// A launcher's actions, as `Actions` wants them.
    fn actions(ids: &[(&str, &str)]) -> Vec<LauncherAction> {
        ids.iter()
            .map(|(id, name)| LauncherAction {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
            })
            .collect()
    }

    /// A menu with one launcher selected, offering `items`.
    fn menu_with(items: &[LauncherAction]) -> Menu {
        Menu::new(
            Point::new(100, 100),
            1,
            Some(Actions {
                entry: "steam.desktop",
                items,
            }),
            measure,
        )
    }

    /// The middle of row `index`.
    fn middle(menu: &Menu, index: usize) -> Point {
        let row = menu.row(index);
        Point::new(row.x + row.w / 2, row.y + row.h / 2)
    }

    /// The index of the row with this action.
    fn row_of(menu: &Menu, action: Action) -> usize {
        menu.entries
            .iter()
            .position(|entry| entry.action() == Some(action))
            .unwrap_or_else(|| panic!("{action:?} is not in the menu"))
    }

    #[test]
    fn the_menu_starts_with_a_centered_header() {
        let menu = menu(0);
        assert!(menu.entries[0].is_header());
        assert_eq!(menu.entries[0].label, "Desktop");
        assert_eq!(
            menu.entries[0].action(),
            None,
            "the header is not choosable"
        );
    }

    #[test]
    fn the_items_are_in_the_order_irix_had_them() {
        let menu = menu(1);
        let labels: Vec<&str> = menu
            .entries
            .iter()
            .filter(|entry| !entry.is_separator() && !entry.is_header())
            .map(|entry| entry.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Log Out",
                "Open",
                "Make Copy",
                "Make Reference",
                "Remove",
                "Change Permissions",
                "Add New Directory",
                "Select All",
            ]
        );
    }

    #[test]
    fn there_are_two_separators_in_the_right_places() {
        let menu = menu(0);
        let separators: Vec<usize> = menu
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.is_separator())
            .map(|(index, _)| index)
            .collect();
        assert_eq!(separators.len(), 2);
        // One after Log Out, one after Remove.
        assert_eq!(separators[0], row_of(&menu, Action::LogOut) + 1);
        assert_eq!(separators[1], row_of(&menu, Action::Remove) + 1);
    }

    #[test]
    fn open_and_remove_need_a_selection() {
        let empty = menu(0);
        for action in [Action::Open, Action::Remove] {
            assert!(
                !empty.entries[row_of(&empty, action)].enabled,
                "{action:?} should be disabled with nothing selected"
            );
        }

        let picked = menu(3);
        for action in [Action::Open, Action::Remove] {
            assert!(
                picked.entries[row_of(&picked, action)].enabled,
                "{action:?} should be enabled with a selection"
            );
        }
    }

    #[test]
    fn log_out_and_select_all_are_always_available() {
        for selected in [0, 1, 5] {
            let menu = menu(selected);
            for action in [Action::LogOut, Action::SelectAll] {
                assert!(menu.entries[row_of(&menu, action)].enabled, "{action:?}");
            }
        }
    }

    #[test]
    fn the_unimplemented_items_are_drawn_but_never_chosen() {
        // They are in the menu so its shape is right and the work to come has somewhere to
        // land; clicking one must do nothing at all.
        let menu = menu(3);
        for action in [
            Action::MakeCopy,
            Action::MakeReference,
            Action::ChangePermissions,
            Action::AddNewDirectory,
        ] {
            let index = row_of(&menu, action);
            assert!(
                !menu.entries[index].enabled,
                "{action:?} should be disabled"
            );
            assert_eq!(
                menu.action_at(middle(&menu, index)),
                None,
                "{action:?} should not be choosable"
            );
        }
    }

    #[test]
    fn clicking_a_row_chooses_its_action() {
        let menu = menu(2);
        for action in [
            Action::LogOut,
            Action::Open,
            Action::Remove,
            Action::SelectAll,
        ] {
            let index = row_of(&menu, action);
            assert_eq!(menu.action_at(middle(&menu, index)), Some(action));
        }
    }

    #[test]
    fn the_header_and_separators_choose_nothing() {
        let menu = menu(2);
        for (index, entry) in menu.entries.iter().enumerate() {
            if entry.is_header() || entry.is_separator() {
                assert_eq!(menu.action_at(middle(&menu, index)), None, "row {index}");
            }
        }
    }

    #[test]
    fn a_click_outside_the_panel_chooses_nothing() {
        let menu = menu(2);
        let panel = menu.panel();
        for point in [
            Point::new(panel.x - 1, panel.y + 10),
            Point::new(panel.x + panel.w + 1, panel.y + 10),
            Point::new(panel.x + 10, panel.y - 1),
            Point::new(panel.x + 10, panel.y + panel.h + 1),
        ] {
            assert!(!menu.contains(point), "{point:?} should be off the panel");
            assert_eq!(menu.action_at(point), None);
        }
    }

    #[test]
    fn rows_stack_in_order_inside_the_panel() {
        let menu = menu(1);
        let panel = menu.panel();
        let mut previous_bottom = panel.y;
        for index in 0..menu.entries.len() {
            let row = menu.row(index);
            assert!(
                row.y >= previous_bottom,
                "row {index} overlaps the one above"
            );
            assert!(row.x > panel.x, "row {index} is not inset from the panel");
            assert!(
                row.x + row.w < panel.x + panel.w,
                "row {index} runs past the panel"
            );
            assert!(
                row.y + row.h <= panel.y + panel.h,
                "row {index} runs past the bottom"
            );
            previous_bottom = row.y + row.h;
        }
    }

    #[test]
    fn hovering_tracks_only_the_rows_that_can_be_chosen() {
        let mut menu = menu(0);
        let log_out = row_of(&menu, Action::LogOut);
        assert!(menu.hover(middle(&menu, log_out)));
        assert_eq!(menu.hovered, Some(log_out));
        // The same row again is not a change, so nothing redraws.
        assert!(!menu.hover(middle(&menu, log_out)));

        // A disabled row does not highlight.
        let open = row_of(&menu, Action::Open);
        assert!(menu.hover(middle(&menu, open)));
        assert_eq!(menu.hovered, None, "Open is disabled with no selection");

        // Nor does the header, a separator, or anywhere off the panel.
        menu.hover(middle(&menu, log_out));
        assert!(menu.hover(Point::new(0, 0)));
        assert_eq!(menu.hovered, None);
    }

    #[test]
    fn a_launchers_actions_are_added_after_a_separator_of_their_own() {
        let items = actions(&[("Store", "Store"), ("Library", "Library")]);
        let menu = menu_with(&items);

        let plain = self::menu(1);
        // Everything the plain menu had, unchanged, and then the new rows.
        assert_eq!(menu.entries.len(), plain.entries.len() + 3);
        let tail: Vec<&str> = menu.entries[plain.entries.len()..]
            .iter()
            .map(|entry| entry.label.as_str())
            .collect();
        assert_eq!(tail, ["", "Store", "Library"]);
        assert!(menu.entries[plain.entries.len()].is_separator());
    }

    #[test]
    fn choosing_an_action_row_says_which_file_and_which_id() {
        let items = actions(&[("Store", "Store"), ("Library", "Library")]);
        let menu = menu_with(&items);

        let library = menu
            .entries
            .iter()
            .position(|entry| entry.label == "Library")
            .expect("Library row");
        let Some(Action::Run(index)) = menu.action_at(middle(&menu, library)) else {
            panic!("Library should be a Run row");
        };
        assert_eq!(
            menu.action_target(index),
            Some(("steam.desktop", "Library"))
        );
    }

    #[test]
    fn an_action_is_named_by_its_id_not_its_label() {
        // The two differ under any locale but the file's own, and it is the id that has to
        // reach the launcher.
        let items = actions(&[("Store", "ストア")]);
        let menu = menu_with(&items);
        let row = menu
            .entries
            .iter()
            .position(|entry| entry.label == "ストア")
            .expect("the translated row");
        let Some(Action::Run(index)) = menu.action_at(middle(&menu, row)) else {
            panic!("should be a Run row");
        };
        assert_eq!(menu.action_target(index).map(|(_, id)| id), Some("Store"));
    }

    #[test]
    fn a_launcher_with_no_actions_leaves_the_menu_as_it_was() {
        // No trailing separator hanging off the bottom with nothing under it.
        let empty = Menu::new(
            Point::new(100, 100),
            1,
            Some(Actions {
                entry: "plain.desktop",
                items: &[],
            }),
            measure,
        );
        assert_eq!(empty.entries, self::menu(1).entries);
        assert_eq!(empty.action_target(0), None);
    }

    #[test]
    fn a_long_action_label_widens_the_panel_instead_of_being_cut() {
        let items = actions(&[("Long", "Open a Private Browsing Window Somewhere Else")]);
        let menu = menu_with(&items);
        let row = menu
            .entries
            .iter()
            .position(|entry| entry.label.starts_with("Open a Private"))
            .expect("the long row");
        let label_width = measure(&menu.entries[row].label);
        assert!(
            menu.panel().w > WIDTH,
            "the panel should have grown past {WIDTH}"
        );
        assert!(
            menu.row(row).x + LABEL_INSET + label_width <= menu.panel().x + menu.panel().w,
            "the label runs past the panel"
        );
    }

    #[test]
    fn short_action_labels_leave_the_panel_the_width_it_always_was() {
        // The floor: adding actions must not make the ordinary menu a different shape.
        let items = actions(&[("Store", "Store")]);
        assert_eq!(menu_with(&items).panel().w, WIDTH);
        assert_eq!(menu(0).panel().w, WIDTH);
    }

    #[test]
    fn the_panel_grows_downward_for_the_rows_it_gained() {
        let items = actions(&[("Store", "Store"), ("Library", "Library")]);
        let menu = menu_with(&items);
        assert_eq!(
            menu.panel().h,
            self::menu(1).panel().h + SEPARATOR_H + 2 * ITEM_H
        );
    }

    #[test]
    fn size_for_agrees_with_the_menu_it_describes() {
        // Clamping happens before the menu exists, so a disagreement would post it in the
        // wrong place -- and only for launchers with actions, which is the easiest kind of
        // bug to miss.
        let items = actions(&[("Store", "Store"), ("Library", "Library")]);
        let described = Menu::size_for(
            1,
            Some(Actions {
                entry: "steam.desktop",
                items: &items,
            }),
            measure,
        );
        let built = menu_with(&items).panel();
        assert_eq!((described.w, described.h), (built.w, built.h));
    }

    #[test]
    fn a_menu_posted_near_a_corner_is_pulled_back_on_screen() {
        // Otherwise the rows past the edge could never be reached.
        let area = Rect::new(0, 0, 1280, 800);
        let panel = Menu::size_for(1, None, measure);

        let corner = Menu::clamp(Point::new(1270, 790), panel, area);
        assert!(corner.x + panel.w <= area.w, "runs off the right");
        assert!(corner.y + panel.h <= area.h, "runs off the bottom");

        // Somewhere with room is left where it was asked for.
        assert_eq!(
            Menu::clamp(Point::new(40, 40), panel, area),
            Point::new(40, 40)
        );
    }

    #[test]
    fn a_menu_bigger_than_the_screen_still_starts_on_it() {
        // Clamping must not chase the far edge so hard that it pushes the near one off, which
        // would put the header and the first rows out of reach instead of the last ones.
        let panel = Menu::size_for(0, None, measure);
        let area = Rect::new(0, 0, panel.w / 2, panel.h / 2);
        let placed = Menu::clamp(Point::new(100, 40), panel, area);
        assert_eq!(placed, Point::new(area.x, area.y));
    }

    #[test]
    fn a_menu_is_pushed_only_as_far_as_it_needs() {
        // Wide enough for the panel but not much: it should sit against the right edge, not
        // jump to the left one.
        let panel = Menu::size_for(0, None, measure);
        let area = Rect::new(0, 0, panel.w + 14, 800);
        let placed = Menu::clamp(Point::new(100, 40), panel, area);
        assert_eq!(placed.x, 14, "should rest against the right edge");
    }
}
