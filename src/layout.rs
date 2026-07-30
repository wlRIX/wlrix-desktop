// SPDX-License-Identifier: GPL-3.0-or-later
//! Where the icons go.
//!
//! IRIX filled the desktop from the **top right**, downward, starting a new column to the
//! **left** when one ran out of room -- the opposite corner and the opposite axis from the
//! minimized-window tiles the compositor draws (`wlrix-compositor/src/minimized.rs`, which
//! fills top-left and rightward). The two grids therefore grow away from each other and only
//! meet when both are full.
//!
//! An icon is either **placed** at a free position the user dropped it at, or **slotted** in a
//! grid cell. Which one depends on the snap setting at the time it was dropped. Both are
//! remembered per file name (see [`crate::state`]), so an icon stays where it was left across
//! restarts, and a file leaving the desktop leaves a *gap* rather than pulling its neighbors
//! up. A file that leaves is forgotten, so one deleted and recreated gets a fresh cell -- the
//! first free one, which is usually the hole it just left.

use crate::entries::Entry;

/// A point in the surface's own pixels, top-left origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A rectangle in the surface's own pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x < self.x + self.w
            && point.y < self.y + self.h
    }
}

/// How big a cell is and how much room is left around the edges.
///
/// A cell holds the icon square with the label underneath, so it is taller than it is wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// The icon artwork, square.
    pub icon: i32,
    /// The whole cell, icon plus label.
    pub cell_w: i32,
    pub cell_h: i32,
    /// Between cells.
    pub gap: i32,
    /// Between the outermost cells and the screen edge.
    pub margin: i32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            icon: 64,
            cell_w: 96,
            // 64 of icon, 4 of gap, then two 16px lines of label with a little slack. Get
            // this wrong by a few pixels and the second line silently does not fit, which
            // shows up as every long filename being cut after one line.
            cell_h: 104,
            gap: 8,
            margin: 12,
        }
    }
}

/// The grid over one surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub area: Rect,
    pub metrics: Metrics,
}

impl Grid {
    pub fn new(area: Rect, metrics: Metrics) -> Self {
        Self { area, metrics }
    }

    /// How many cells fit in one column, top to bottom. Always at least one, so a very short
    /// screen still lays icons out in a single row rather than dividing by zero.
    pub fn rows(&self) -> i32 {
        let usable = self.area.h - 2 * self.metrics.margin + self.metrics.gap;
        let step = self.metrics.cell_h + self.metrics.gap;
        (usable / step).max(1)
    }

    /// How many columns fit, right to left. Always at least one.
    pub fn columns(&self) -> i32 {
        let usable = self.area.w - 2 * self.metrics.margin + self.metrics.gap;
        let step = self.metrics.cell_w + self.metrics.gap;
        (usable / step).max(1)
    }

    /// Every cell that fits.
    pub fn capacity(&self) -> i32 {
        self.rows() * self.columns()
    }

    /// Where cell `index` sits.
    ///
    /// Indices advance **down** a column first and then **leftward**, so index 0 is the
    /// top-right corner and index `rows()` starts the second column in from the right.
    pub fn slot_rect(&self, index: i32) -> Rect {
        let rows = self.rows();
        let column = index.div_euclid(rows);
        let row = index.rem_euclid(rows);
        let Metrics {
            cell_w,
            cell_h,
            gap,
            margin,
            ..
        } = self.metrics;

        // Measured in from the right edge: the first column's right side is one margin in.
        let right = self.area.x + self.area.w - margin;
        let x = right - (column + 1) * cell_w - column * gap;
        let y = self.area.y + margin + row * (cell_h + gap);
        Rect::new(x, y, cell_w, cell_h)
    }

    /// Which cell contains `point`, if any. The inverse of [`Grid::slot_rect`].
    ///
    /// Points in the gaps between cells belong to no cell, so this is not simply arithmetic
    /// on the position -- the candidate is computed and then checked against its own rect.
    pub fn slot_at(&self, point: Point) -> Option<i32> {
        let Metrics {
            cell_w,
            cell_h,
            gap,
            margin,
            ..
        } = self.metrics;

        let right = self.area.x + self.area.w - margin;
        // How far left of the right margin, in whole columns.
        let from_right = right - point.x;
        if from_right < 0 {
            return None;
        }
        let column = from_right / (cell_w + gap);
        let from_top = point.y - (self.area.y + margin);
        if from_top < 0 {
            return None;
        }
        let row = from_top / (cell_h + gap);
        if column >= self.columns() || row >= self.rows() {
            return None;
        }

        let index = column * self.rows() + row;
        self.slot_rect(index).contains(point).then_some(index)
    }

    /// The cell nearest `point`, for snapping a dropped icon.
    ///
    /// Unlike [`Grid::slot_at`] this always answers: a drop in a gap, or past the last cell,
    /// still has to land somewhere.
    pub fn nearest_slot(&self, point: Point) -> i32 {
        let Metrics {
            cell_w,
            cell_h,
            gap,
            margin,
            ..
        } = self.metrics;

        let right = self.area.x + self.area.w - margin;
        // Round to the closest column/row rather than truncating, so an icon dropped just
        // past a cell's midpoint snaps forward instead of always backward.
        let column = div_round(right - point.x, cell_w + gap).clamp(0, self.columns() - 1);
        let row =
            div_round(point.y - (self.area.y + margin), cell_h + gap).clamp(0, self.rows() - 1);
        column * self.rows() + row
    }

    /// Keep a freely-placed icon on screen.
    pub fn clamp(&self, point: Point) -> Point {
        let max_x = (self.area.x + self.area.w - self.metrics.cell_w).max(self.area.x);
        let max_y = (self.area.y + self.area.h - self.metrics.cell_h).max(self.area.y);
        Point::new(
            point.x.clamp(self.area.x, max_x),
            point.y.clamp(self.area.y, max_y),
        )
    }
}

/// Round a division to the nearest integer, for negatives too.
fn div_round(value: i32, by: i32) -> i32 {
    if by == 0 {
        return 0;
    }
    let half = by / 2;
    if value >= 0 {
        (value + half) / by
    } else {
        (value - half) / by
    }
}

/// Where one icon is, however it got there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spot {
    /// In a grid cell.
    Slot(i32),
    /// Wherever it was dropped, as the cell's top-left corner.
    Free(Point),
}

/// One laid-out icon: the entry and the cell it occupies.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub entry: Entry,
    pub spot: Spot,
    pub rect: Rect,
}

/// Lay every entry out, honoring the positions already remembered for them.
///
/// An entry with a remembered spot keeps it -- that is the whole point of remembering -- and a
/// slot is only given up if it no longer exists on this screen, or another entry got there
/// first. Everything else takes the first free cell, in the order [`crate::entries::read`]
/// returned them.
///
/// **The caller is expected to remember what this hands back**, not just what the user dragged.
/// An icon whose position is only implied by its place in the list is not really placed at all:
/// move the one above it and everything below slides up to close the gap, because they were
/// never anywhere in particular. `crate::ui::Desktop::relayout` pins the result for exactly
/// this reason.
pub fn arrange(
    grid: &Grid,
    entries: &[Entry],
    remembered: &dyn Fn(&str) -> Option<Spot>,
) -> Vec<Placed> {
    let capacity = grid.capacity();
    let mut taken: Vec<i32> = Vec::new();
    let mut placed: Vec<Option<Spot>> = Vec::with_capacity(entries.len());

    // First pass: honor what is remembered, so a new file can never displace an icon the
    // user positioned deliberately.
    for entry in entries {
        let spot = match remembered(&entry.name) {
            // A slot past the end of the grid is one the screen used to have and no longer
            // does; honoring it would draw the icon off the edge, so it is rehomed below.
            Some(Spot::Slot(index)) if index < capacity && !taken.contains(&index) => {
                taken.push(index);
                Some(Spot::Slot(index))
            }
            // A remembered free position is always honored: free icons do not collide,
            // they just overlap, which is what the user asked for by placing them freely.
            Some(Spot::Free(point)) => Some(Spot::Free(grid.clamp(point))),
            // Either nothing remembered, or a slot someone else already holds.
            _ => None,
        };
        placed.push(spot);
    }

    // Second pass: fill the gaps with the lowest free cells.
    let mut next = 0;
    for spot in placed.iter_mut() {
        if spot.is_some() {
            continue;
        }
        while taken.contains(&next) {
            next += 1;
        }
        taken.push(next);
        *spot = Some(Spot::Slot(next));
    }

    entries
        .iter()
        .zip(placed)
        .map(|(entry, spot)| {
            let spot = spot.unwrap_or(Spot::Slot(0));
            let rect = match spot {
                Spot::Slot(index) => grid.slot_rect(index),
                Spot::Free(point) => {
                    Rect::new(point.x, point.y, grid.metrics.cell_w, grid.metrics.cell_h)
                }
            };
            Placed {
                entry: entry.clone(),
                spot,
                rect,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries::Kind;

    /// A 1280x800 screen with the default metrics: 8 rows, 12 columns.
    fn grid() -> Grid {
        Grid::new(Rect::new(0, 0, 1280, 800), Metrics::default())
    }

    fn entry(name: &str) -> Entry {
        Entry {
            path: format!("/desktop/{name}").into(),
            name: name.to_owned(),
            kind: Kind::Plain,
        }
    }

    #[test]
    fn the_first_cell_is_the_top_right_corner() {
        let grid = grid();
        let first = grid.slot_rect(0);
        let metrics = grid.metrics;
        assert_eq!(first.y, metrics.margin, "should start at the top");
        assert_eq!(
            first.x + first.w,
            grid.area.w - metrics.margin,
            "should sit against the right margin"
        );
    }

    #[test]
    fn cells_fill_downward_before_stepping_left() {
        let grid = grid();
        let first = grid.slot_rect(0);
        let second = grid.slot_rect(1);
        // Down first: same column, lower.
        assert_eq!(second.x, first.x);
        assert!(
            second.y > first.y,
            "the second cell should be below the first"
        );

        // Then leftward, once the column is full.
        let next_column = grid.slot_rect(grid.rows());
        assert_eq!(
            next_column.y, first.y,
            "a new column starts back at the top"
        );
        assert!(
            next_column.x < first.x,
            "a new column should be to the *left*, not the right"
        );
    }

    #[test]
    fn columns_step_by_a_whole_cell_plus_the_gap() {
        let grid = grid();
        let first = grid.slot_rect(0);
        let second_column = grid.slot_rect(grid.rows());
        assert_eq!(
            first.x - second_column.x,
            grid.metrics.cell_w + grid.metrics.gap
        );
    }

    #[test]
    fn every_cell_stays_inside_the_area() {
        let grid = grid();
        for index in 0..grid.capacity() {
            let rect = grid.slot_rect(index);
            assert!(rect.x >= grid.area.x, "cell {index} runs off the left");
            assert!(rect.y >= grid.area.y, "cell {index} runs off the top");
            assert!(
                rect.x + rect.w <= grid.area.x + grid.area.w,
                "cell {index} runs off the right"
            );
            assert!(
                rect.y + rect.h <= grid.area.y + grid.area.h,
                "cell {index} runs off the bottom"
            );
        }
    }

    #[test]
    fn slot_at_is_the_inverse_of_slot_rect() {
        let grid = grid();
        for index in 0..grid.capacity() {
            let rect = grid.slot_rect(index);
            let middle = Point::new(rect.x + rect.w / 2, rect.y + rect.h / 2);
            assert_eq!(grid.slot_at(middle), Some(index), "cell {index}");
        }
    }

    #[test]
    fn gaps_and_margins_belong_to_no_cell() {
        let grid = grid();
        // Hard against the right edge, outside the margin.
        assert_eq!(grid.slot_at(Point::new(grid.area.w - 1, 400)), None);
        // The top margin.
        assert_eq!(grid.slot_at(Point::new(grid.area.w - 40, 0)), None);
        // Between the first two rows.
        let first = grid.slot_rect(0);
        let between = Point::new(first.x + 1, first.y + first.h + grid.metrics.gap / 2);
        assert_eq!(grid.slot_at(between), None);
    }

    #[test]
    fn a_short_screen_still_has_one_row() {
        // Shorter than a single cell: the grid must not divide by zero or vanish.
        let grid = Grid::new(Rect::new(0, 0, 1280, 20), Metrics::default());
        assert_eq!(grid.rows(), 1);
        assert!(grid.capacity() >= 1);
    }

    #[test]
    fn nearest_slot_always_answers() {
        let grid = grid();
        // Far off every edge, including negative.
        for point in [
            Point::new(-500, -500),
            Point::new(5000, 5000),
            Point::new(0, 400),
        ] {
            let index = grid.nearest_slot(point);
            assert!(
                (0..grid.capacity()).contains(&index),
                "{point:?} snapped to {index}, outside 0..{}",
                grid.capacity()
            );
        }
    }

    #[test]
    fn new_entries_take_the_first_free_cells_in_order() {
        let grid = grid();
        let entries = [entry("apple"), entry("mango"), entry("zebra")];
        let placed = arrange(&grid, &entries, &|_| None);
        assert_eq!(placed[0].spot, Spot::Slot(0));
        assert_eq!(placed[1].spot, Spot::Slot(1));
        assert_eq!(placed[2].spot, Spot::Slot(2));
    }

    #[test]
    fn a_remembered_slot_is_kept_and_others_flow_around_it() {
        let grid = grid();
        let entries = [entry("apple"), entry("mango"), entry("zebra")];
        // `mango` was left in cell 0; the others must not take it.
        let placed = arrange(&grid, &entries, &|name| {
            (name == "mango").then_some(Spot::Slot(0))
        });
        assert_eq!(placed[1].spot, Spot::Slot(0), "mango should keep its cell");
        assert_eq!(placed[0].spot, Spot::Slot(1));
        assert_eq!(placed[2].spot, Spot::Slot(2));
    }

    #[test]
    fn a_slot_the_screen_no_longer_has_is_rehomed() {
        // Plugging into a smaller monitor: a remembered cell past the end would otherwise be
        // drawn off the edge, where it could never be clicked back.
        let small = Grid::new(Rect::new(0, 0, 400, 300), Metrics::default());
        let entries = [entry("a")];
        let placed = arrange(&small, &entries, &|_| Some(Spot::Slot(999)));
        let Spot::Slot(index) = placed[0].spot else {
            panic!("should still be slotted");
        };
        assert!(index < small.capacity(), "slot {index} is off the grid");
        let rect = placed[0].rect;
        assert!(rect.x >= small.area.x, "icon runs off the left edge");
        assert!(rect.y + rect.h <= small.area.y + small.area.h);
    }

    #[test]
    fn a_file_that_comes_back_returns_to_its_cell() {
        // Deleting and recreating a file must not shuffle the desktop.
        let grid = grid();
        let remembered = |name: &str| (name == "zebra").then_some(Spot::Slot(7));
        let before = arrange(&grid, &[entry("apple"), entry("zebra")], &remembered);
        let after = arrange(&grid, &[entry("apple"), entry("zebra")], &remembered);
        assert_eq!(before[1].spot, Spot::Slot(7));
        assert_eq!(before, after);
    }

    #[test]
    fn two_entries_cannot_hold_the_same_cell() {
        let grid = grid();
        let entries = [entry("apple"), entry("mango")];
        // Both remember cell 3 -- which happens when the screen shrinks and two slots
        // collapse onto one. The first keeps it, the second is rehomed.
        let placed = arrange(&grid, &entries, &|_| Some(Spot::Slot(3)));
        assert_eq!(placed[0].spot, Spot::Slot(3));
        assert_ne!(placed[1].spot, Spot::Slot(3));
    }

    #[test]
    fn a_free_position_is_clamped_onto_the_screen() {
        let grid = grid();
        let entries = [entry("apple")];
        // Left where the screen used to be bigger.
        let placed = arrange(&grid, &entries, &|_| {
            Some(Spot::Free(Point::new(5000, 5000)))
        });
        let Spot::Free(point) = placed[0].spot else {
            panic!("should still be freely placed");
        };
        assert!(point.x + grid.metrics.cell_w <= grid.area.x + grid.area.w);
        assert!(point.y + grid.metrics.cell_h <= grid.area.y + grid.area.h);
    }
}
