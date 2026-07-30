// SPDX-License-Identifier: GPL-3.0-or-later
//! Hover, selection, and dragging an icon.
//!
//! Pure state: this takes pointer positions and gives back what changed, so the whole
//! interaction can be exercised without a compositor. [`crate::ui`] owns turning Wayland
//! events into these calls and redrawing when one reports a change.
//!
//! The drag rules mirror the compositor's minimized-icon tiles
//! (`wlrix-compositor/src/minimized.rs`): a press arms a possible drag, and only once the
//! pointer has moved past a threshold does it become a move rather than a click. Without
//! that, a click with a shaky hand nudges the icon a pixel and the user never gets a clean
//! selection.

use crate::layout::{Grid, Placed, Point, Spot};

/// How far the pointer must travel after a press before it counts as a drag.
const DRAG_THRESHOLD: f64 = 6.0;

/// An in-progress drag.
#[derive(Debug, Clone, PartialEq)]
struct Drag {
    /// The file being moved, by name -- the entry list can be rebuilt underneath a drag when
    /// the directory changes, and an index would then point at the wrong icon.
    name: String,
    /// Where in the cell the press landed, so the icon tracks the pointer without jumping.
    grab: Point,
    /// Where the press started, to measure the threshold against.
    press: Point,
    /// The latest pointer position.
    current: Point,
    /// Whether the threshold has been passed.
    moved: bool,
}

/// What is hovered, what is selected, and what is being dragged.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Selection {
    hovered: Option<String>,
    selected: Option<String>,
    drag: Option<Drag>,
}

/// What a drop settled on, for the caller to remember and save.
#[derive(Debug, Clone, PartialEq)]
pub struct Dropped {
    pub name: String,
    pub spot: Spot,
}

impl Selection {
    /// The file under the pointer, if any.
    pub fn hovered(&self) -> Option<&str> {
        self.hovered.as_deref()
    }

    /// The picked file, if any.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The file being dragged, once the drag threshold has been passed.
    ///
    /// `None` while a press is merely armed, so a click does not make the icon jump.
    pub fn dragging(&self) -> Option<&str> {
        self.drag
            .as_ref()
            .filter(|drag| drag.moved)
            .map(|drag| drag.name.as_str())
    }

    /// Where the dragged icon's cell should be drawn right now.
    pub fn drag_origin(&self) -> Option<Point> {
        let drag = self.drag.as_ref().filter(|drag| drag.moved)?;
        Some(Point::new(
            drag.current.x - drag.grab.x,
            drag.current.y - drag.grab.y,
        ))
    }

    /// Forget a file that is no longer on the desktop.
    ///
    /// Called after a rescan: a hovered or selected file that has been deleted must not leave
    /// a highlight pointing at nothing.
    pub fn retain_only(&mut self, present: &[String]) {
        let gone =
            |name: &Option<String>| name.as_ref().is_some_and(|name| !present.contains(name));
        if gone(&self.hovered) {
            self.hovered = None;
        }
        if gone(&self.selected) {
            self.selected = None;
        }
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| !present.contains(&drag.name))
        {
            self.drag = None;
        }
    }

    /// Update the hover from a pointer position. Returns whether anything changed.
    pub fn hover(&mut self, placed: &[Placed], point: Point) -> bool {
        // A drag owns the pointer: the icon under it is the one being carried, and letting
        // hover wander to whatever is underneath would flicker.
        if self.drag.is_some() {
            return false;
        }
        let under = icon_at(placed, point).map(|item| item.entry.name.clone());
        if under == self.hovered {
            return false;
        }
        self.hovered = under;
        true
    }

    /// The pointer left the surface.
    pub fn leave(&mut self) -> bool {
        if self.hovered.is_none() {
            return false;
        }
        self.hovered = None;
        true
    }

    /// A press. Selects whatever is under the pointer -- or clears the selection if that is
    /// bare desktop -- and arms a possible drag. Returns whether anything changed.
    pub fn press(&mut self, placed: &[Placed], point: Point) -> bool {
        let Some(item) = icon_at(placed, point) else {
            // Bare desktop: deselect.
            let changed = self.selected.is_some();
            self.selected = None;
            self.drag = None;
            return changed;
        };

        let changed = self.selected.as_deref() != Some(item.entry.name.as_str());
        self.selected = Some(item.entry.name.clone());
        self.drag = Some(Drag {
            name: item.entry.name.clone(),
            grab: Point::new(point.x - item.rect.x, point.y - item.rect.y),
            press: point,
            current: point,
            moved: false,
        });
        changed
    }

    /// Pointer motion while a button is held. Returns whether the icon moved.
    pub fn motion(&mut self, point: Point) -> bool {
        let Some(drag) = self.drag.as_mut() else {
            return false;
        };
        drag.current = point;
        if drag.moved {
            return true;
        }
        // Squared distance, to keep it in integers.
        let dx = f64::from(point.x - drag.press.x);
        let dy = f64::from(point.y - drag.press.y);
        if dx * dx + dy * dy >= DRAG_THRESHOLD * DRAG_THRESHOLD {
            drag.moved = true;
            return true;
        }
        false
    }

    /// A release. If the press turned into a drag, says where the icon landed.
    ///
    /// A release that never passed the threshold was a click: the selection made on press
    /// stands, and nothing moves.
    pub fn release(&mut self, grid: &Grid, snap: bool) -> Option<Dropped> {
        let drag = self.drag.take()?;
        if !drag.moved {
            return None;
        }
        let origin = Point::new(drag.current.x - drag.grab.x, drag.current.y - drag.grab.y);
        let spot = if snap {
            // Snap by the cell's center, not its corner: dropping an icon so it mostly
            // covers a cell should choose that cell.
            let centre = Point::new(
                origin.x + grid.metrics.cell_w / 2,
                origin.y + grid.metrics.cell_h / 2,
            );
            Spot::Slot(grid.nearest_slot(centre))
        } else {
            Spot::Free(grid.clamp(origin))
        };
        Some(Dropped {
            name: drag.name,
            spot,
        })
    }
}

/// The icon under `point`, topmost first.
///
/// Later entries win, so a freely-placed icon dropped over another is the one you grab --
/// which matches what is drawn, since painting goes in the same order.
fn icon_at(placed: &[Placed], point: Point) -> Option<&Placed> {
    placed.iter().rev().find(|item| item.rect.contains(point))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries::{Entry, Kind};
    use crate::layout::{Metrics, Rect};

    fn grid() -> Grid {
        Grid::new(Rect::new(0, 0, 1280, 800), Metrics::default())
    }

    fn placed(names: &[&str]) -> Vec<Placed> {
        let grid = grid();
        names
            .iter()
            .enumerate()
            .map(|(index, name)| Placed {
                entry: Entry {
                    path: format!("/desktop/{name}").into(),
                    name: (*name).to_owned(),
                    kind: Kind::Plain,
                },
                spot: Spot::Slot(index as i32),
                rect: grid.slot_rect(index as i32),
            })
            .collect()
    }

    fn centre(item: &Placed) -> Point {
        Point::new(item.rect.x + item.rect.w / 2, item.rect.y + item.rect.h / 2)
    }

    #[test]
    fn hovering_tracks_the_icon_under_the_pointer() {
        let placed = placed(&["a", "b"]);
        let mut selection = Selection::default();

        assert!(selection.hover(&placed, centre(&placed[0])));
        assert_eq!(selection.hovered(), Some("a"));
        // Same icon again is not a change, so nothing redraws.
        assert!(!selection.hover(&placed, centre(&placed[0])));

        assert!(selection.hover(&placed, centre(&placed[1])));
        assert_eq!(selection.hovered(), Some("b"));

        // Bare desktop.
        assert!(selection.hover(&placed, Point::new(5, 400)));
        assert_eq!(selection.hovered(), None);
    }

    #[test]
    fn clicking_an_icon_selects_it_and_the_desktop_deselects() {
        let placed = placed(&["a", "b"]);
        let mut selection = Selection::default();

        selection.press(&placed, centre(&placed[0]));
        assert_eq!(selection.selected(), Some("a"));
        selection.release(&grid(), false);

        selection.press(&placed, centre(&placed[1]));
        assert_eq!(selection.selected(), Some("b"));
        selection.release(&grid(), false);

        selection.press(&placed, Point::new(5, 400));
        assert_eq!(selection.selected(), None);
    }

    #[test]
    fn a_click_does_not_move_the_icon() {
        // The whole reason for the threshold: a press and release at nearly the same place
        // is a selection, not a one-pixel nudge.
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        let start = centre(&placed[0]);

        selection.press(&placed, start);
        selection.motion(Point::new(start.x + 2, start.y + 1));
        assert_eq!(selection.dragging(), None, "2px is not a drag");
        assert_eq!(selection.release(&grid(), false), None);
    }

    #[test]
    fn moving_past_the_threshold_becomes_a_drag() {
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        let start = centre(&placed[0]);

        selection.press(&placed, start);
        // Left and down, away from the corner: dragging the top-right icon further right
        // would hit the clamp, which is its own test below.
        selection.motion(Point::new(start.x - 40, start.y + 40));
        assert_eq!(selection.dragging(), Some("a"));

        let dropped = selection
            .release(&grid(), false)
            .expect("should have moved");
        assert_eq!(dropped.name, "a");
        let Spot::Free(point) = dropped.spot else {
            panic!("free placement expected with snap off");
        };
        // The icon keeps its grab offset, so it lands 40px from where it started.
        assert_eq!(point.x, placed[0].rect.x - 40);
        assert_eq!(point.y, placed[0].rect.y + 40);
    }

    #[test]
    fn a_drop_off_the_edge_is_pulled_back_on_screen() {
        // The first cell is already against the right margin, so dragging it further right
        // must not leave the icon half off the screen with no way to grab it again.
        let placed = placed(&["a"]);
        let grid = grid();
        let mut selection = Selection::default();
        let start = centre(&placed[0]);

        selection.press(&placed, start);
        selection.motion(Point::new(start.x + 400, start.y + 400));
        let dropped = selection.release(&grid, false).expect("should have moved");
        let Spot::Free(point) = dropped.spot else {
            panic!("free placement expected with snap off");
        };
        assert!(point.x + grid.metrics.cell_w <= grid.area.x + grid.area.w);
        assert!(point.y + grid.metrics.cell_h <= grid.area.y + grid.area.h);
    }

    #[test]
    fn dropping_with_snap_on_lands_in_a_cell() {
        let placed = placed(&["a"]);
        let grid = grid();
        let mut selection = Selection::default();
        let start = centre(&placed[0]);

        selection.press(&placed, start);
        // Somewhere over towards the middle of the screen.
        selection.motion(Point::new(start.x - 300, start.y + 200));
        let dropped = selection.release(&grid, true).expect("should have moved");
        let Spot::Slot(index) = dropped.spot else {
            panic!("a slot expected with snap on");
        };
        assert!((0..grid.capacity()).contains(&index));
    }

    #[test]
    fn a_drag_holds_the_hover() {
        // Dragging an icon over its neighbors must not make each of them light up in turn.
        let placed = placed(&["a", "b"]);
        let mut selection = Selection::default();
        let start = centre(&placed[0]);

        selection.hover(&placed, start);
        selection.press(&placed, start);
        selection.motion(Point::new(start.x + 40, start.y + 40));
        assert!(!selection.hover(&placed, centre(&placed[1])));
        assert_eq!(selection.hovered(), Some("a"));
    }

    #[test]
    fn the_dragged_icon_follows_the_pointer_from_where_it_was_grabbed() {
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        // Grab near the icon's bottom-right, not its center.
        let grab = Point::new(placed[0].rect.x + 70, placed[0].rect.y + 60);

        selection.press(&placed, grab);
        selection.motion(Point::new(grab.x + 100, grab.y + 100));
        let origin = selection.drag_origin().expect("should be dragging");
        // Still 70,60 in from the pointer -- the icon did not jump to center itself.
        assert_eq!(origin.x, placed[0].rect.x + 100);
        assert_eq!(origin.y, placed[0].rect.y + 100);
    }

    #[test]
    fn a_deleted_file_stops_being_hovered_or_selected() {
        let placed = placed(&["a", "b"]);
        let mut selection = Selection::default();
        selection.hover(&placed, centre(&placed[0]));
        selection.press(&placed, centre(&placed[0]));
        selection.release(&grid(), false);

        selection.retain_only(&["b".to_owned()]);
        assert_eq!(selection.hovered(), None);
        assert_eq!(selection.selected(), None);
    }

    #[test]
    fn the_topmost_icon_is_the_one_grabbed() {
        // Two overlapping freely-placed icons: the one drawn last is the one picked up.
        let mut placed = placed(&["under", "over"]);
        placed[1].rect = placed[0].rect;
        let mut selection = Selection::default();
        selection.press(&placed, centre(&placed[0]));
        assert_eq!(selection.selected(), Some("over"));
    }
}
