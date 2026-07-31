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
//!
//! ## Selecting more than one
//!
//! Pressing bare desktop and dragging pulls out a **rubber band**: every icon it touches is
//! selected, and the band is drawn as a plain black outline. Pressing an icon that is already
//! in the selection drags the *whole* selection, so a band followed by a drag moves the lot;
//! pressing one that is not replaces the selection with it, which is what makes a stray click
//! feel like starting over rather than adding to a pile.

use crate::layout::{Grid, Placed, Point, Rect, Spot};

/// How far the pointer must travel after a press before it counts as a drag.
const DRAG_THRESHOLD: f64 = 6.0;

/// How close together two presses on the same icon have to be to open it, in milliseconds.
const DOUBLE_CLICK_MS: u32 = 400;

/// An in-progress drag.
#[derive(Debug, Clone, PartialEq)]
struct Drag {
    /// The files being moved, by name -- the entry list can be rebuilt underneath a drag when
    /// the directory changes, and an index would then point at the wrong icon.
    ///
    /// More than one when the press landed on an already-selected icon: the whole selection
    /// travels together, every icon by the same delta.
    names: Vec<String>,
    /// The one actually under the pointer when the press landed.
    ///
    /// Kept so a *click* on an icon inside a group can narrow the selection down to it on
    /// release. Without that there is no way to get from "these four" back to "just this one"
    /// except by clicking bare desktop first.
    pressed: String,
    /// Where the press started. Everything moves by `current - press`, which keeps a group
    /// rigid and, for a single icon, is the same as tracking the grab offset.
    press: Point,
    /// The latest pointer position.
    current: Point,
    /// Whether the threshold has been passed.
    moved: bool,
}

/// A rubber band being dragged out over the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Band {
    /// Where the press landed. The band is drawn between this and `current`, either way round
    /// -- dragging up and left is as valid as down and right.
    origin: Point,
    current: Point,
}

impl Band {
    /// The band as a rectangle, whichever way it was dragged.
    fn rect(&self) -> Rect {
        let x = self.origin.x.min(self.current.x);
        let y = self.origin.y.min(self.current.y);
        Rect::new(
            x,
            y,
            (self.origin.x - self.current.x).abs(),
            (self.origin.y - self.current.y).abs(),
        )
    }
}

/// What is hovered, what is selected, and what is being dragged.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Selection {
    hovered: Option<String>,
    /// Every picked file. Usually one; more after a rubber band.
    selected: Vec<String>,
    drag: Option<Drag>,
    /// The rubber band, while one is being dragged out.
    band: Option<Band>,
    /// The last press on an icon: which, and when. A second press on the same icon soon
    /// enough is a double-click, which opens it.
    last_press: Option<(String, u32)>,
}

/// What a press amounted to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pressed {
    /// Whether anything visible changed, so the desktop should redraw.
    pub changed: bool,
    /// Set when the press completed a double-click: open this icon.
    pub activate: Option<String>,
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

    /// Every picked file.
    pub fn selected(&self) -> &[String] {
        &self.selected
    }

    /// Whether `name` is picked.
    pub fn is_selected(&self, name: &str) -> bool {
        self.selected.iter().any(|picked| picked == name)
    }

    /// Whether `name` is being dragged, once the threshold has been passed.
    ///
    /// False while a press is merely armed, so a click does not make the icon jump.
    pub fn is_dragging(&self, name: &str) -> bool {
        self.drag
            .as_ref()
            .filter(|drag| drag.moved)
            .is_some_and(|drag| drag.names.iter().any(|dragged| dragged == name))
    }

    /// How far the dragged icons have traveled from where they were picked up.
    ///
    /// A delta rather than an absolute position, so a group stays rigid: every icon in the
    /// drag moves by the same amount.
    pub fn drag_delta(&self) -> Option<Point> {
        let drag = self.drag.as_ref().filter(|drag| drag.moved)?;
        Some(Point::new(
            drag.current.x - drag.press.x,
            drag.current.y - drag.press.y,
        ))
    }

    /// The rubber band to draw, if one is being dragged out.
    pub fn band(&self) -> Option<Rect> {
        self.band.map(|band| band.rect())
    }

    /// Forget a file that is no longer on the desktop.
    ///
    /// Called after a rescan: a hovered or selected file that has been deleted must not leave
    /// a highlight pointing at nothing.
    pub fn retain_only(&mut self, present: &[String]) {
        if self
            .hovered
            .as_ref()
            .is_some_and(|name| !present.contains(name))
        {
            self.hovered = None;
        }
        self.selected.retain(|name| present.contains(name));
        if let Some(drag) = self.drag.as_mut() {
            drag.names.retain(|name| present.contains(name));
            if drag.names.is_empty() {
                self.drag = None;
            }
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
    /// bare desktop -- arms a possible drag, and reports a double-click.
    ///
    /// `time` is the pointer event's own millisecond stamp, which is what decides whether this
    /// press is the second half of a double-click.
    pub fn press(&mut self, placed: &[Placed], point: Point, time: u32) -> Pressed {
        let Some(item) = icon_at(placed, point) else {
            // Bare desktop: deselect, and start pulling a rubber band out from here. A press
            // that never moves is just a click, and the band stays empty.
            let changed = !self.selected.is_empty();
            self.selected.clear();
            self.drag = None;
            self.last_press = None;
            self.band = Some(Band {
                origin: point,
                current: point,
            });
            return Pressed {
                changed,
                activate: None,
            };
        };
        let name = item.entry.name.clone();

        // The second press on the same icon, soon enough, opens it. `wrapping_sub` because the
        // pointer's clock is a free-running `u32` of milliseconds and does wrap.
        let doubled = self
            .last_press
            .as_ref()
            .is_some_and(|(last, at)| *last == name && time.wrapping_sub(*at) <= DOUBLE_CLICK_MS);

        // Cleared on a double-click, so a third press starts a new pair rather than opening
        // the icon again.
        self.last_press = (!doubled).then(|| (name.clone(), time));

        // Pressing something already selected keeps the whole selection and drags all of it;
        // pressing anything else replaces the selection. That is what lets a rubber band be
        // followed by a drag, while a stray click still starts over.
        let changed = if self.is_selected(&name) {
            false
        } else {
            self.selected = vec![name.clone()];
            true
        };
        self.drag = Some(Drag {
            names: self.selected.clone(),
            pressed: name.clone(),
            press: point,
            current: point,
            moved: false,
        });
        Pressed {
            changed,
            activate: doubled.then_some(name),
        }
    }

    /// Pointer motion while a button is held.
    ///
    /// Either grows the rubber band -- reselecting whatever it now covers -- or moves the
    /// icons being dragged. Returns whether anything changed on screen.
    pub fn motion(&mut self, placed: &[Placed], point: Point) -> bool {
        if let Some(band) = self.band.as_mut() {
            if band.current == point {
                return false;
            }
            band.current = point;
            let rect = band.rect();
            // Recomputed from scratch every time rather than accumulated, so shrinking the
            // band deselects again -- dragging past an icon and back must not keep it.
            let covered: Vec<String> = placed
                .iter()
                .filter(|item| rect.intersects(item.rect))
                .map(|item| item.entry.name.clone())
                .collect();
            self.selected = covered;
            return true;
        }

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
            // Moving an icon is not half of a double-click: drag it somewhere and click it
            // once, and it should select, not open.
            self.last_press = None;
            return true;
        }
        false
    }

    /// A release. If the press turned into a drag, says where each icon landed.
    ///
    /// A release that never passed the threshold was a click: the selection made on press
    /// stands, and nothing moves. Ending a rubber band likewise moves nothing -- it only put
    /// the band away, leaving whatever it caught selected.
    pub fn release(&mut self, placed: &[Placed], grid: &Grid, snap: bool) -> Vec<Dropped> {
        if self.band.take().is_some() {
            return Vec::new();
        }
        let Some(drag) = self.drag.take() else {
            return Vec::new();
        };
        if !drag.moved {
            // A click, not a drag. If it landed inside a group, narrow the selection to just
            // that icon -- the group was kept on *press* only in case this turned into a
            // drag, and it did not.
            self.selected = vec![drag.pressed];
            return Vec::new();
        }
        let delta = Point::new(drag.current.x - drag.press.x, drag.current.y - drag.press.y);

        drag.names
            .iter()
            .filter_map(|name| {
                let item = placed.iter().find(|item| &item.entry.name == name)?;
                let origin = Point::new(item.rect.x + delta.x, item.rect.y + delta.y);
                let spot = if snap {
                    // Snap by the cell's center, not its corner: dropping an icon so it mostly
                    // covers a cell should choose that cell. Each icon in a group snaps to its
                    // own nearest cell, and `layout::arrange` rehomes any that collide.
                    let centre = Point::new(
                        origin.x + grid.metrics.cell_w / 2,
                        origin.y + grid.metrics.cell_h / 2,
                    );
                    Spot::Slot(grid.nearest_slot(centre))
                } else {
                    Spot::Free(grid.clamp(origin))
                };
                Some(Dropped {
                    name: name.clone(),
                    spot,
                })
            })
            .collect()
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
                    launcher: None,
                },
                spot: Spot::Slot(index as i32),
                rect: grid.slot_rect(index as i32),
            })
            .collect()
    }

    /// A press with a timestamp far from any other, so it is never half of a double-click.
    ///
    /// Tests that care about the double-click clock pass their own times instead.
    fn press(selection: &mut Selection, placed: &[Placed], point: Point) -> Pressed {
        // Each call is a full threshold-and-then-some later than the last.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let time = NEXT.fetch_add(DOUBLE_CLICK_MS * 10, std::sync::atomic::Ordering::Relaxed);
        selection.press(placed, point, time)
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

        press(&mut selection, &placed, centre(&placed[0]));
        assert!(selection.is_selected("a"));
        selection.release(&placed, &grid(), false);

        press(&mut selection, &placed, centre(&placed[1]));
        assert!(selection.is_selected("b"));
        selection.release(&placed, &grid(), false);

        press(&mut selection, &placed, Point::new(5, 400));
        assert!(selection.selected().is_empty());
    }

    #[test]
    fn a_click_does_not_move_the_icon() {
        // The whole reason for the threshold: a press and release at nearly the same place
        // is a selection, not a one-pixel nudge.
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        let start = centre(&placed[0]);

        press(&mut selection, &placed, start);
        selection.motion(&placed, Point::new(start.x + 2, start.y + 1));
        assert!(!selection.is_dragging("a"), "2px is not a drag");
        assert!(selection.release(&placed, &grid(), false).is_empty());
    }

    #[test]
    fn moving_past_the_threshold_becomes_a_drag() {
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        let start = centre(&placed[0]);

        press(&mut selection, &placed, start);
        // Left and down, away from the corner: dragging the top-right icon further right
        // would hit the clamp, which is its own test below.
        selection.motion(&placed, Point::new(start.x - 40, start.y + 40));
        assert!(selection.is_dragging("a"));

        let dropped = selection.release(&placed, &grid(), false);
        let dropped = dropped.first().expect("should have moved");
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

        press(&mut selection, &placed, start);
        selection.motion(&placed, Point::new(start.x + 400, start.y + 400));
        let dropped = selection.release(&placed, &grid, false);
        let dropped = dropped.first().expect("should have moved");
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

        press(&mut selection, &placed, start);
        // Somewhere over towards the middle of the screen.
        selection.motion(&placed, Point::new(start.x - 300, start.y + 200));
        let dropped = selection.release(&placed, &grid, true);
        let dropped = dropped.first().expect("should have moved");
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
        press(&mut selection, &placed, start);
        selection.motion(&placed, Point::new(start.x + 40, start.y + 40));
        assert!(!selection.hover(&placed, centre(&placed[1])));
        assert_eq!(selection.hovered(), Some("a"));
    }

    #[test]
    fn the_dragged_icon_follows_the_pointer_from_where_it_was_grabbed() {
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        // Grab near the icon's bottom-right, not its center.
        let grab = Point::new(placed[0].rect.x + 70, placed[0].rect.y + 60);

        press(&mut selection, &placed, grab);
        selection.motion(&placed, Point::new(grab.x + 100, grab.y + 100));
        let delta = selection.drag_delta().expect("should be dragging");
        // The icon travels exactly as far as the pointer did, rather than jumping to center
        // itself under it: the grab offset is preserved because the delta is what moves.
        assert_eq!(delta, Point::new(100, 100));
    }

    #[test]
    fn a_deleted_file_stops_being_hovered_or_selected() {
        let placed = placed(&["a", "b"]);
        let mut selection = Selection::default();
        selection.hover(&placed, centre(&placed[0]));
        press(&mut selection, &placed, centre(&placed[0]));
        selection.release(&placed, &grid(), false);

        selection.retain_only(&["b".to_owned()]);
        assert_eq!(selection.hovered(), None);
        assert!(selection.selected().is_empty());
    }

    #[test]
    fn the_topmost_icon_is_the_one_grabbed() {
        // Two overlapping freely-placed icons: the one drawn last is the one picked up.
        let mut placed = placed(&["under", "over"]);
        placed[1].rect = placed[0].rect;
        let mut selection = Selection::default();
        press(&mut selection, &placed, centre(&placed[0]));
        assert!(selection.is_selected("over"));
    }

    // --- rubber band ----------------------------------------------------------------

    /// A point on bare desktop, well away from the icon column on the right.
    fn empty() -> Point {
        Point::new(40, 40)
    }

    /// The bottom-right corner of `index`'s cell, one pixel inside it.
    fn corner(item: &Placed) -> Point {
        Point::new(item.rect.x + item.rect.w - 1, item.rect.y + item.rect.h - 1)
    }

    #[test]
    fn dragging_from_bare_desktop_pulls_out_a_band() {
        let placed = placed(&["a"]);
        let mut selection = Selection::default();

        assert_eq!(selection.band(), None, "no band before a press");
        press(&mut selection, &placed, empty());
        // A press alone is a zero-sized band, which paints nothing.
        assert_eq!(selection.band(), Some(Rect::new(40, 40, 0, 0)));

        selection.motion(&placed, Point::new(140, 240));
        assert_eq!(selection.band(), Some(Rect::new(40, 40, 100, 200)));

        selection.release(&placed, &grid(), false);
        assert_eq!(selection.band(), None, "the band goes away on release");
    }

    #[test]
    fn a_band_dragged_up_and_left_is_the_same_rectangle() {
        // Bands are dragged in every direction; the rect must normalize rather than come out
        // with a negative size and catch nothing.
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        press(&mut selection, &placed, Point::new(300, 300));
        selection.motion(&placed, Point::new(200, 100));
        assert_eq!(selection.band(), Some(Rect::new(200, 100, 100, 200)));
    }

    #[test]
    fn a_band_selects_everything_it_touches() {
        let placed = placed(&["a", "b", "c"]);
        let mut selection = Selection::default();

        // From bare desktop across the first two cells.
        press(&mut selection, &placed, empty());
        selection.motion(&placed, corner(&placed[1]));

        assert!(selection.is_selected("a"));
        assert!(selection.is_selected("b"));
        assert!(!selection.is_selected("c"), "the band stopped short of c");
    }

    #[test]
    fn shrinking_the_band_deselects_again() {
        // Recomputed from scratch each motion, so dragging past an icon and back must not
        // leave it selected.
        let placed = placed(&["a", "b"]);
        let mut selection = Selection::default();

        press(&mut selection, &placed, empty());
        selection.motion(&placed, corner(&placed[1]));
        assert!(selection.is_selected("b"));

        selection.motion(&placed, corner(&placed[0]));
        assert!(selection.is_selected("a"));
        assert!(!selection.is_selected("b"), "b should have been let go");
    }

    #[test]
    fn a_click_on_bare_desktop_still_just_deselects() {
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        press(&mut selection, &placed, centre(&placed[0]));
        assert!(selection.is_selected("a"));

        // Press and release without moving: no band, nothing selected, nothing moved.
        press(&mut selection, &placed, empty());
        assert!(selection.release(&placed, &grid(), false).is_empty());
        assert!(selection.selected().is_empty());
    }

    // --- moving a selection together ------------------------------------------------

    #[test]
    fn pressing_a_selected_icon_drags_the_whole_selection() {
        let placed = placed(&["a", "b", "c"]);
        let mut selection = Selection::default();

        // Band the first two, then grab one of them.
        press(&mut selection, &placed, empty());
        selection.motion(&placed, corner(&placed[1]));
        selection.release(&placed, &grid(), false);

        press(&mut selection, &placed, centre(&placed[0]));
        selection.motion(
            &placed,
            Point::new(centre(&placed[0]).x - 200, centre(&placed[0]).y),
        );

        assert!(selection.is_dragging("a"));
        assert!(
            selection.is_dragging("b"),
            "the whole selection should move"
        );
        assert!(!selection.is_dragging("c"));
    }

    #[test]
    fn a_group_moves_rigidly_and_all_of_it_is_dropped() {
        let placed = placed(&["a", "b", "c"]);
        let grid = grid();
        let mut selection = Selection::default();

        press(&mut selection, &placed, empty());
        selection.motion(&placed, corner(&placed[1]));
        selection.release(&placed, &grid, false);

        let start = centre(&placed[0]);
        press(&mut selection, &placed, start);
        selection.motion(&placed, Point::new(start.x - 200, start.y + 100));

        let mut dropped = selection.release(&placed, &grid, false);
        dropped.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(dropped.len(), 2, "both selected icons should have moved");

        // Every icon moved by the same delta, so the gaps between them are unchanged.
        for (drop, item) in dropped.iter().zip(placed.iter()) {
            let Spot::Free(point) = drop.spot else {
                panic!("free placement expected with snap off");
            };
            assert_eq!(
                point.x,
                item.rect.x - 200,
                "{} moved differently",
                drop.name
            );
            assert_eq!(
                point.y,
                item.rect.y + 100,
                "{} moved differently",
                drop.name
            );
        }
    }

    #[test]
    fn clicking_one_icon_inside_a_group_narrows_to_it() {
        // The group is kept on *press* in case the click turns into a drag. When it does not,
        // the release collapses to the one clicked -- otherwise there is no way back to a
        // single selection except by clicking bare desktop first.
        let placed = placed(&["a", "b", "c"]);
        let grid = grid();
        let mut selection = Selection::default();

        press(&mut selection, &placed, empty());
        selection.motion(&placed, corner(&placed[1]));
        selection.release(&placed, &grid, false);
        assert_eq!(selection.selected().len(), 2);

        // Click one of the two, without moving.
        press(&mut selection, &placed, centre(&placed[1]));
        assert_eq!(selection.selected().len(), 2, "still a group while pressed");
        selection.release(&placed, &grid, false);
        assert_eq!(selection.selected(), ["b"], "the click should narrow it");
    }

    #[test]
    fn dragging_a_group_keeps_the_group() {
        // The mirror of the test above: a press that *does* become a drag must not collapse.
        let placed = placed(&["a", "b"]);
        let grid = grid();
        let mut selection = Selection::default();

        press(&mut selection, &placed, empty());
        selection.motion(&placed, corner(&placed[1]));
        selection.release(&placed, &grid, false);

        let start = centre(&placed[0]);
        press(&mut selection, &placed, start);
        selection.motion(&placed, Point::new(start.x - 100, start.y));
        selection.release(&placed, &grid, false);
        assert_eq!(
            selection.selected().len(),
            2,
            "the group should survive a move"
        );
    }

    #[test]
    fn pressing_an_unselected_icon_starts_over() {
        // Otherwise a click next to a band-selected group would quietly add to it, and there
        // would be no way to get back to a single selection.
        let placed = placed(&["a", "b", "c"]);
        let mut selection = Selection::default();

        press(&mut selection, &placed, empty());
        selection.motion(&placed, corner(&placed[1]));
        selection.release(&placed, &grid(), false);
        assert_eq!(selection.selected().len(), 2);

        press(&mut selection, &placed, centre(&placed[2]));
        assert_eq!(selection.selected(), ["c"]);
    }

    #[test]
    fn a_deleted_file_leaves_a_group_drag_alone() {
        // A rescan mid-drag must drop the vanished icon without cancelling the move.
        let placed = placed(&["a", "b"]);
        let mut selection = Selection::default();

        press(&mut selection, &placed, empty());
        selection.motion(&placed, corner(&placed[1]));
        selection.release(&placed, &grid(), false);

        let start = centre(&placed[0]);
        press(&mut selection, &placed, start);
        selection.motion(&placed, Point::new(start.x - 100, start.y));

        selection.retain_only(&["a".to_owned()]);
        assert!(selection.is_dragging("a"), "a should still be moving");
        assert!(!selection.is_dragging("b"));
    }

    // --- double-click ---------------------------------------------------------------

    #[test]
    fn two_quick_presses_on_one_icon_open_it() {
        let placed = placed(&["a", "b"]);
        let mut selection = Selection::default();
        let at = centre(&placed[0]);

        let first = selection.press(&placed, at, 1_000);
        assert_eq!(first.activate, None, "one press only selects");
        let second = selection.press(&placed, at, 1_100);
        assert_eq!(second.activate.as_deref(), Some("a"));
    }

    #[test]
    fn two_slow_presses_do_not() {
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        let at = centre(&placed[0]);

        selection.press(&placed, at, 1_000);
        let second = selection.press(&placed, at, 1_000 + DOUBLE_CLICK_MS + 1);
        assert_eq!(second.activate, None);
    }

    #[test]
    fn two_presses_on_different_icons_do_not() {
        let placed = placed(&["a", "b"]);
        let mut selection = Selection::default();

        selection.press(&placed, centre(&placed[0]), 1_000);
        let second = selection.press(&placed, centre(&placed[1]), 1_050);
        assert_eq!(
            second.activate, None,
            "different icons are not a double-click"
        );
    }

    #[test]
    fn a_third_press_does_not_open_it_again() {
        // Otherwise a rapid triple-click launches twice.
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        let at = centre(&placed[0]);

        selection.press(&placed, at, 1_000);
        assert!(selection.press(&placed, at, 1_080).activate.is_some());
        assert_eq!(selection.press(&placed, at, 1_160).activate, None);
        // ...but a fresh pair after that does open it again.
        assert!(selection.press(&placed, at, 1_240).activate.is_some());
    }

    #[test]
    fn a_click_on_bare_desktop_between_the_two_breaks_it_up() {
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        let at = centre(&placed[0]);

        selection.press(&placed, at, 1_000);
        selection.press(&placed, Point::new(5, 400), 1_050);
        let third = selection.press(&placed, at, 1_100);
        assert_eq!(third.activate, None);
    }

    #[test]
    fn dragging_an_icon_does_not_arm_a_double_click() {
        // Drag an icon somewhere and click it once: that should select, not open.
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        let at = centre(&placed[0]);

        selection.press(&placed, at, 1_000);
        selection.motion(&placed, Point::new(at.x - 60, at.y + 60));
        selection.release(&placed, &grid(), false);

        let next = selection.press(&placed, at, 1_100);
        assert_eq!(next.activate, None);
    }

    #[test]
    fn the_pointer_clock_wrapping_is_not_a_double_click_storm() {
        // `time` is a free-running u32 of milliseconds and does wrap. Two presses either side
        // of the wrap are milliseconds apart in real time, so they *are* a double-click --
        // what must not happen is the subtraction going enormous and every press counting.
        let placed = placed(&["a"]);
        let mut selection = Selection::default();
        let at = centre(&placed[0]);

        selection.press(&placed, at, u32::MAX - 50);
        let across = selection.press(&placed, at, 49);
        assert_eq!(
            across.activate.as_deref(),
            Some("a"),
            "100ms apart across the wrap"
        );

        // And a genuinely distant pair still does not open.
        let mut selection = Selection::default();
        selection.press(&placed, at, u32::MAX - 50);
        let far = selection.press(&placed, at, 5_000);
        assert_eq!(far.activate, None);
    }
}
