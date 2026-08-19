//! Tiling layout engine.
//!
//! Pure geometry: takes a work area, a window count and a [`LayoutParams`],
//! and returns one zone rectangle per window. No Win32 types cross this
//! boundary, which keeps the whole engine unit-testable off-device.
//!
//! ## Rounding contract
//!
//! Zone edges are computed as *shared boundaries* (`start + i * span / count`)
//! rather than by accumulating per-zone widths. Adjacent zones therefore always
//! agree on the pixel where they meet, so a tiled monitor has no 1px seams and
//! no 1px overlaps regardless of how the work area divides. This is verified by
//! [`tests::tiling_is_exact_and_gapless`] across a wide sweep of sizes.

use serde::{Deserialize, Serialize};

/// An inclusive-left, exclusive-right rectangle in virtual-screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
    pub const fn width(&self) -> i32 {
        self.right - self.left
    }
    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub const fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }
    pub const fn center_x(&self) -> i32 {
        self.left + self.width() / 2
    }
    pub const fn center_y(&self) -> i32 {
        self.top + self.height() / 2
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
    /// Shrink on all sides by `d` (used for gaps).
    ///
    /// Deflation is capped so a non-empty rectangle always stays non-empty:
    /// applying an 8px gap to a 4px-tall zone must leave 1px, not 0. Zero-sized
    /// zones would reach `SetWindowPos` as invisible windows.
    pub fn deflate(&self, d: i32) -> Rect {
        let max_x = (self.width() - 1).max(0) / 2;
        let max_y = (self.height() - 1).max(0) / 2;
        let dx = d.clamp(0, max_x);
        let dy = d.clamp(0, max_y);
        Rect::new(
            self.left + dx,
            self.top + dy,
            self.right - dx,
            self.bottom - dy,
        )
    }
    pub fn area(&self) -> i64 {
        (self.width().max(0) as i64) * (self.height().max(0) as i64)
    }
    /// Area of the intersection with `other`.
    pub fn intersection_area(&self, other: &Rect) -> i64 {
        let l = self.left.max(other.left);
        let t = self.top.max(other.top);
        let r = self.right.min(other.right);
        let b = self.bottom.min(other.bottom);
        if r <= l || b <= t {
            0
        } else {
            ((r - l) as i64) * ((b - t) as i64)
        }
    }
}

/// Which tiling algorithm to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutKind {
    /// Near-square grid; the last row absorbs the remainder.
    #[default]
    Grid,
    /// One master pane plus a vertical stack (dwm-style).
    MasterStack,
    /// Equal-width vertical columns.
    Columns,
    /// Equal-height horizontal rows.
    Rows,
    /// Recursive alternating half-splits; each new window halves the remainder.
    Dwindle,
    /// Every window fills the whole work area (tabbed/stacked feel).
    Monocle,
    /// Free-form binary partition: cells are split where the user drops.
    ///
    /// Unlike the others, zones are not derived from a window count — they
    /// come from a tree the user shapes by dropping windows on edges. This is
    /// the only layout that can split one cell while its neighbours keep
    /// their span. See [`crate::tree`].
    Bsp,
}

impl LayoutKind {
    pub const ALL: [LayoutKind; 7] = [
        LayoutKind::Grid,
        LayoutKind::MasterStack,
        LayoutKind::Columns,
        LayoutKind::Rows,
        LayoutKind::Dwindle,
        LayoutKind::Monocle,
        LayoutKind::Bsp,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            LayoutKind::Grid => "Grid",
            LayoutKind::MasterStack => "Master + Stack",
            LayoutKind::Columns => "Columns",
            LayoutKind::Rows => "Rows",
            LayoutKind::Dwindle => "Dwindle",
            LayoutKind::Monocle => "Monocle",
            LayoutKind::Bsp => "Split (drag to divide)",
        }
    }

    pub fn next(&self) -> LayoutKind {
        let i = Self::ALL.iter().position(|k| k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn prev(&self) -> LayoutKind {
        let i = Self::ALL.iter().position(|k| k == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Tunables applied on top of a [`LayoutKind`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LayoutParams {
    /// Gap between the work-area edge and the outermost windows, in pixels.
    pub outer_gap: i32,
    /// Gap between adjacent windows, in pixels. Split evenly between neighbours.
    pub inner_gap: i32,
    /// Fraction of the primary axis given to the master pane (MasterStack /
    /// Dwindle). Clamped to a sane range at apply time.
    pub master_fraction: f32,
    /// How many windows share the master area (MasterStack only).
    pub master_count: u32,
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self {
            outer_gap: 8,
            inner_gap: 8,
            master_fraction: 0.55,
            master_count: 1,
        }
    }
}

impl LayoutParams {
    /// Clamp user-supplied values into ranges the engine can honour.
    ///
    /// Config is user-editable TOML, so these values are untrusted input:
    /// a negative gap or a NaN fraction must not be able to produce inverted
    /// or non-finite rectangles that would later be handed to `SetWindowPos`.
    pub fn sanitized(&self) -> LayoutParams {
        LayoutParams {
            outer_gap: self.outer_gap.clamp(0, 400),
            inner_gap: self.inner_gap.clamp(0, 400),
            master_fraction: if self.master_fraction.is_finite() {
                self.master_fraction.clamp(0.15, 0.85)
            } else {
                0.55
            },
            master_count: self.master_count.clamp(1, 16),
        }
    }
}

/// User-adjusted split positions, produced by dragging a tile edge.
///
/// Stored as fractions of the work area rather than pixels so they survive a
/// resolution change, and as *boundaries* (n-1 values for n zones) rather than
/// per-zone widths so adjacent zones cannot disagree about where they meet.
///
/// An empty vector, or one of the wrong length, means "equal splits" — that is
/// the normal state, and the reason a fresh layout needs no stored data at all.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Splits {
    /// Boundaries along the layout's main axis.
    ///
    /// Columns: the vertical lines between columns. Rows: the horizontal lines.
    /// Grid: the column boundaries. Master + Stack: `main[0]` is the master
    /// pane's right edge, and it overrides `master_fraction`.
    #[serde(default)]
    pub main: Vec<f32>,
    /// Boundaries along the cross axis: grid rows, or stack-item heights in
    /// Master + Stack.
    #[serde(default)]
    pub cross: Vec<f32>,
    /// Grid only: column boundaries **per row**.
    ///
    /// `main` cannot express this. A grid's columns were shared by every row,
    /// so dragging the divider between two cells on the bottom row moved the
    /// same divider on every row above it, and the lower rows could not be
    /// resized independently at all. Index is the row; an absent or
    /// wrong-length entry falls back to `main`, then to equal splits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grid_rows: Vec<Vec<f32>>,
}

impl Splits {
    pub fn is_empty(&self) -> bool {
        self.main.is_empty() && self.cross.is_empty() && self.grid_rows.is_empty()
    }

    pub fn clear(&mut self) {
        self.main.clear();
        self.cross.clear();
        self.grid_rows.clear();
    }

    /// Column boundaries for one grid row, falling back to the shared set.
    pub fn grid_row(&self, row: usize) -> &[f32] {
        match self.grid_rows.get(row) {
            Some(v) if !v.is_empty() => v,
            _ => &self.main,
        }
    }

    /// Move one column boundary within a single grid row.
    ///
    /// Rows are materialised lazily from whatever they look like now, so
    /// dragging one row's divider leaves every other row exactly where it was.
    pub fn set_grid_column(&mut self, row: usize, index: usize, fraction: f32, cols: usize) {
        if cols < 2 || !fraction.is_finite() {
            return;
        }
        let want = cols - 1;
        if self.grid_rows.len() <= row {
            self.grid_rows.resize(row + 1, Vec::new());
        }
        // Seed from the shared boundaries if they are usable, so the row does
        // not visibly jump to equal splits on the first drag.
        let seed: Vec<f32> = if self.main.len() == want {
            self.main.clone()
        } else {
            (1..cols).map(|i| i as f32 / cols as f32).collect()
        };
        let v = &mut self.grid_rows[row];
        if v.len() != want {
            *v = seed;
        }
        if index < v.len() {
            v[index] = fraction;
        }
    }

    /// Set one boundary on an axis, resizing the vector to `count - 1` first.
    ///
    /// Out-of-range indices are ignored rather than panicking: the caller
    /// derives the index from a live window's geometry, which can race with a
    /// window closing.
    pub fn set(&mut self, axis: SplitAxis, index: usize, fraction: f32, count: usize) {
        if count < 2 || !fraction.is_finite() {
            return;
        }
        let want = count - 1;
        let v = match axis {
            SplitAxis::Main => &mut self.main,
            SplitAxis::Cross => &mut self.cross,
        };
        if v.len() != want {
            // Materialise the current equal split before editing one edge, so
            // the others stay where they visually were.
            *v = (1..count).map(|i| i as f32 / count as f32).collect();
        }
        if index < v.len() {
            v[index] = fraction;
        }
    }
}

/// Which family of boundaries a [`Splits`] edit refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    Main,
    Cross,
}

/// Resolve `n - 1` boundary fractions, falling back to equal splits.
///
/// The result is always strictly increasing and leaves every zone at least
/// `MIN_ZONE_FRACTION` of the axis, so a drag cannot collapse a neighbour to
/// nothing or invert the ordering.
fn boundaries(n: usize, custom: &[f32]) -> Vec<f32> {
    let want = n.saturating_sub(1);
    if want == 0 {
        return Vec::new();
    }
    let usable = custom.len() == want && custom.iter().all(|f| f.is_finite());
    let mut v: Vec<f32> = if usable {
        custom.to_vec()
    } else {
        (1..n).map(|i| i as f32 / n as f32).collect()
    };

    // Every zone keeps at least this share of the axis.
    let min_gap = MIN_ZONE_FRACTION.min(1.0 / (n as f32 * 2.0));
    let mut lo = min_gap;
    for (i, b) in v.iter_mut().enumerate() {
        let remaining_after = (want - i) as f32;
        let hi = 1.0 - min_gap * remaining_after;
        *b = if hi > lo { b.clamp(lo, hi) } else { lo };
        lo = *b + min_gap;
    }
    v
}

/// Smallest share of an axis any zone may be dragged down to.
pub const MIN_ZONE_FRACTION: f32 = 0.05;

/// Compute one zone per window.
///
/// Returns exactly `count` rectangles (empty vec when `count == 0`). Every
/// rectangle is non-empty as long as the work area can accommodate the
/// requested split; degenerate splits collapse gracefully rather than
/// producing inverted rects.
pub fn compute(area: Rect, count: usize, kind: LayoutKind, params: &LayoutParams) -> Vec<Rect> {
    compute_with(area, count, kind, params, &Splits::default())
}

/// As [`compute`], honouring user-dragged split positions.
pub fn compute_with(
    area: Rect,
    count: usize,
    kind: LayoutKind,
    params: &LayoutParams,
    splits: &Splits,
) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let p = params.sanitized();
    let outer = area.deflate(p.outer_gap);
    if outer.is_empty() {
        // Work area is smaller than the requested padding: fall back to the raw
        // area so windows stay usable rather than collapsing to zero size.
        return vec![area; count];
    }

    let raw = match kind {
        LayoutKind::Monocle => vec![outer; count],
        LayoutKind::Grid => grid(outer, count, splits),
        LayoutKind::Columns => split_axis_at(outer, count, Axis::Horizontal, &splits.main),
        LayoutKind::Rows => split_axis_at(outer, count, Axis::Vertical, &splits.main),
        LayoutKind::MasterStack => {
            master_stack(outer, count, p.master_fraction, p.master_count, splits)
        }
        // Bsp zones come from the tree, not from a count. This branch is only
        // reached by callers that have no tree to hand (neighbour lookup, for
        // instance); Dwindle is the closest fixed approximation.
        LayoutKind::Dwindle | LayoutKind::Bsp => dwindle(outer, count, p.master_fraction),
    };

    // Apply the inner gap by deflating each zone by half of it, so the visual
    // distance between two neighbours equals `inner_gap`. Monocle is exempt:
    // its zones intentionally coincide.
    if p.inner_gap > 0 && kind != LayoutKind::Monocle && count > 1 {
        raw.into_iter()
            .map(|r| r.deflate(p.inner_gap / 2))
            .collect()
    } else {
        raw
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Horizontal,
    Vertical,
}

/// Split `area` into `n` equal parts along `axis` using shared boundaries.
fn split_axis(area: Rect, n: usize, axis: Axis) -> Vec<Rect> {
    split_axis_at(area, n, axis, &[])
}

/// Split `area` into `n` parts along `axis` at the given boundary fractions.
///
/// Boundary positions are computed once and shared by the zones on either
/// side, so the tiling stays exact whatever the fractions are.
fn split_axis_at(area: Rect, n: usize, axis: Axis, custom: &[f32]) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let span = match axis {
        Axis::Horizontal => area.width(),
        Axis::Vertical => area.height(),
    };
    let start = match axis {
        Axis::Horizontal => area.left,
        Axis::Vertical => area.top,
    };

    // Edge positions: start, each boundary, end. Using exact integer division
    // for the equal case keeps the historic pixel-perfect behaviour.
    let mut edges: Vec<i32> = Vec::with_capacity(n + 1);
    edges.push(start);
    if custom.len() == n.saturating_sub(1) && !custom.is_empty() {
        for f in boundaries(n, custom) {
            edges.push(start + (span as f32 * f).round() as i32);
        }
    } else {
        let n_i = n as i32;
        for i in 1..n_i {
            edges.push(start + (i * span) / n_i);
        }
    }
    edges.push(start + span);

    (0..n)
        .map(|i| match axis {
            Axis::Horizontal => Rect::new(edges[i], area.top, edges[i + 1], area.bottom),
            Axis::Vertical => Rect::new(area.left, edges[i], area.right, edges[i + 1]),
        })
        .collect()
}

/// Near-square grid. Columns are chosen as `ceil(sqrt(n))`; the final row
/// spreads its (possibly fewer) windows across the full width so there is
/// never a hole in the tiling.
fn grid(area: Rect, n: usize, splits: &Splits) -> Vec<Rect> {
    let cols = ((n as f64).sqrt().ceil() as usize).max(1);
    let rows = n.div_ceil(cols);

    // Row bands first, then cells within each band.
    let bands = split_axis_at(area, rows, Axis::Vertical, &splits.cross);

    let mut out = Vec::with_capacity(n);
    let mut placed = 0usize;
    for (row, band) in bands.iter().enumerate() {
        let remaining = n - placed;
        let in_this_row = remaining.min(cols);
        // Column boundaries belong to their row, so resizing the bottom row
        // does not drag every row above it.
        //
        // This applies to a short final row too. Excluding it -- on the theory
        // that its cells do not line up with the full rows -- made the last
        // row unresizable whenever it was short, which is most of the time: 5
        // windows is 3 + 2. `split_axis_at` ignores a vector of the wrong
        // length, so a row that changes width simply reverts to equal splits.
        let custom: &[f32] = splits.grid_row(row);
        out.extend(split_axis_at(*band, in_this_row, Axis::Horizontal, custom));
        placed += in_this_row;
        if placed >= n {
            break;
        }
    }
    debug_assert_eq!(out.len(), n);
    out
}

/// `master_count` windows share the left pane; the rest stack on the right.
fn master_stack(
    area: Rect,
    n: usize,
    fraction: f32,
    master_count: u32,
    splits: &Splits,
) -> Vec<Rect> {
    let masters = (master_count as usize).min(n);
    if masters == n {
        // Everything is master: degenerate to a plain vertical split.
        return split_axis_at(area, n, Axis::Vertical, &splits.cross);
    }
    // A dragged master edge (main[0]) overrides the configured fraction: the
    // user's most recent direct manipulation wins over the stored default.
    let f = match splits.main.first() {
        Some(v) if v.is_finite() => v.clamp(MIN_ZONE_FRACTION, 1.0 - MIN_ZONE_FRACTION),
        _ => fraction,
    };
    let split_x = area.left + (area.width() as f32 * f).round() as i32;
    // Guarantee both panes remain usable even at extreme fractions.
    let split_x = split_x.clamp(area.left + 1, area.right - 1);

    let master_area = Rect::new(area.left, area.top, split_x, area.bottom);
    let stack_area = Rect::new(split_x, area.top, area.right, area.bottom);

    let mut out = split_axis(master_area, masters, Axis::Vertical);
    out.extend(split_axis_at(
        stack_area,
        n - masters,
        Axis::Vertical,
        &splits.cross,
    ));
    out
}

/// Recursive alternating split: window *i* takes `fraction` of what remains,
/// alternating horizontal/vertical. The final window absorbs the remainder.
///
/// Naive alternation runs out of pixels: each horizontal step keeps only
/// `1 - fraction` of the width, so at the default 0.55 the remainder is under
/// one pixel after ~9 horizontal splits and the split point inverts. Two
/// guards prevent that:
///
/// 1. The split axis falls back to whichever side still has room, instead of
///    blindly alternating.
/// 2. The split point is bounded so the remainder retains at least one pixel
///    per window still to be placed.
///
/// Both guards preserve the shared-boundary property, so dwindle still tiles
/// the area exactly.
fn dwindle(area: Rect, n: usize, fraction: f32) -> Vec<Rect> {
    let mut out = Vec::with_capacity(n);
    let mut remaining = area;
    for i in 0..n {
        // Windows that still need space *after* this one.
        let after = (n - i - 1) as i32;
        if after == 0 {
            out.push(remaining);
            break;
        }

        let w = remaining.width();
        let h = remaining.height();
        // To keep every later zone non-degenerate the split axis needs at least
        // one pixel for this window plus one for each of the rest.
        let need = after + 1;
        let h_ok = w >= 2 && w >= need;
        let v_ok = h >= 2 && h >= need;

        let prefer_h = i % 2 == 0;
        let use_h = match (h_ok, v_ok) {
            (true, true) => prefer_h, // both viable: keep alternating
            (true, false) => true,    // only width has room
            (false, true) => false,   // only height has room
            // Neither axis has a pixel to spare for every window still queued.
            // Only reachable when the zone has fewer pixels than windows; hand
            // the survivors the same rect rather than emitting inverted ones.
            // This is the single case where dwindle stops tiling exactly.
            (false, false) => {
                out.extend(std::iter::repeat_n(remaining, after as usize + 1));
                break;
            }
        };

        if use_h {
            let lo = remaining.left + 1;
            let hi = (remaining.right - after).max(lo);
            let x = (remaining.left + (w as f32 * fraction).round() as i32).clamp(lo, hi);
            out.push(Rect::new(
                remaining.left,
                remaining.top,
                x,
                remaining.bottom,
            ));
            remaining = Rect::new(x, remaining.top, remaining.right, remaining.bottom);
        } else {
            let lo = remaining.top + 1;
            let hi = (remaining.bottom - after).max(lo);
            let y = (remaining.top + (h as f32 * fraction).round() as i32).clamp(lo, hi);
            out.push(Rect::new(remaining.left, remaining.top, remaining.right, y));
            remaining = Rect::new(remaining.left, y, remaining.right, remaining.bottom);
        }
    }
    debug_assert_eq!(out.len(), n);
    out
}

/// Index of the zone whose centre is nearest to the given point, or the zone
/// containing it if any does. Used to map a dragged window onto a zone.
pub fn zone_at(zones: &[Rect], x: i32, y: i32) -> Option<usize> {
    if zones.is_empty() {
        return None;
    }
    if let Some(i) = zones.iter().position(|z| z.contains(x, y)) {
        return Some(i);
    }
    let mut best = 0usize;
    let mut best_d = i64::MAX;
    for (i, z) in zones.iter().enumerate() {
        let dx = (z.center_x() - x) as i64;
        let dy = (z.center_y() - y) as i64;
        let d = dx * dx + dy * dy;
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    Some(best)
}

/// Index of the zone with the largest overlap with `r`. Used to work out which
/// zone a window currently occupies so its slot can be remembered.
pub fn best_overlap(zones: &[Rect], r: &Rect) -> Option<usize> {
    let mut best: Option<(usize, i64)> = None;
    for (i, z) in zones.iter().enumerate() {
        let a = z.intersection_area(r);
        if a > 0 && best.is_none_or(|(_, ba)| a > ba) {
            best = Some((i, a));
        }
    }
    best.map(|(i, _)| i)
}

/// Direction for focus/swap navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Nearest zone from `from` in `dir`, scored by axial distance with a penalty
/// for cross-axis drift. Returns `None` when nothing lies that way.
pub fn neighbour(zones: &[Rect], from: usize, dir: Direction) -> Option<usize> {
    let origin = zones.get(from)?;
    let (ox, oy) = (origin.center_x(), origin.center_y());
    let mut best: Option<(usize, i64)> = None;

    for (i, z) in zones.iter().enumerate() {
        if i == from {
            continue;
        }
        let (cx, cy) = (z.center_x(), z.center_y());
        let (primary, cross) = match dir {
            Direction::Left => (ox - cx, (cy - oy).abs()),
            Direction::Right => (cx - ox, (cy - oy).abs()),
            Direction::Up => (oy - cy, (cx - ox).abs()),
            Direction::Down => (cy - oy, (cx - ox).abs()),
        };
        if primary <= 0 {
            continue; // not in the requested direction
        }
        // Weight cross-axis drift heavily so we prefer the truly adjacent zone.
        let score = primary as i64 + (cross as i64) * 3;
        if best.is_none_or(|(_, bs)| score < bs) {
            best = Some((i, score));
        }
    }
    best.map(|(i, _)| i)
}

/// Grow `zone` to at least `min`, anchored at its top-left, kept inside `bounds`.
///
/// A window with a hard minimum does not shrink into a cell smaller than that
/// -- it clamps, and then sits wherever it likes. Asking for a size it cannot
/// take means it never matches its zone, which is how FreeCAD, WhatsApp and
/// Steam ended up being written off as unmanageable and ejected from the grid.
///
/// So ask for something it can actually do. The cell keeps its position and
/// grows just enough; the window overlaps its neighbour, which is honest and
/// predictable, rather than being dropped out of the layout altogether. When
/// even `bounds` cannot hold the minimum, the zone is pinned to `bounds` --
/// there is nothing better available, and overflowing the monitor would put
/// the title bar off-screen.
pub fn fit_to_minimum(zone: Rect, min: (i32, i32), bounds: Rect) -> Rect {
    let (min_w, min_h) = min;
    let w = zone.width().max(min_w).min(bounds.width().max(1));
    let h = zone.height().max(min_h).min(bounds.height().max(1));
    if w == zone.width() && h == zone.height() {
        return zone;
    }
    // Slide back inside the work area rather than growing off the right or
    // bottom edge: a window whose controls are off-screen is unusable.
    let left = zone.left.min(bounds.right - w).max(bounds.left);
    let top = zone.top.min(bounds.bottom - h).max(bounds.top);
    Rect::new(left, top, left + w, top + h)
}

/// Total pixels by which a layout falls short of its occupants' minimum sizes.
///
/// Zero means every window fits. A larger number is a worse layout, and that
/// ordering is the whole point: a drag is judged by whether it makes the
/// squeeze worse, not by whether a squeeze exists.
///
/// Rejecting any layout that violates a minimum would be simpler and wrong.
/// Fifteen windows on one monitor will not all clear their minimums whatever
/// is done with them, and a rule phrased as "never violate" would freeze every
/// boundary on the screen — including the drags that were relieving the
/// pressure.
pub fn squeeze_deficit(zones: &[Rect], mins: &[(i32, i32)]) -> i64 {
    zones
        .iter()
        .zip(mins.iter())
        .map(|(z, (mw, mh))| {
            let dw = (mw - z.width()).max(0) as i64;
            let dh = (mh - z.height()).max(0) as i64;
            dw + dh
        })
        .sum()
}

/// What a window's response to a placement reveals about its minimum size.
///
/// Returns the width and height it insisted on, where it insisted at all.
///
/// A window that ends up bigger than it was asked for has either clamped the
/// size or ignored the request entirely. Compare only the sizes and the two are
/// indistinguishable, and treating the second as the first is how an earlier
/// attempt at this froze GIMP at whatever width it happened to have.
///
/// The tell is the anchor. A window that honours a move and refuses a size
/// keeps one edge exactly where it was put and pushes the opposite edge out:
/// Chrome asked to be 1024px wide at x=1976 came back 1371px wide at the same
/// x. A window that ignored the request matches on neither edge. So an axis
/// only teaches its minimum when one of its two edges landed where we asked.
pub fn learned_minimum(want: Rect, got: Rect, tolerance: i32) -> (Option<i32>, Option<i32>) {
    let near = |a: i32, b: i32| (a - b).abs() <= tolerance;

    let width = (got.width() > want.width() + tolerance
        && (near(got.left, want.left) || near(got.right, want.right)))
    .then_some(got.width());

    let height = (got.height() > want.height() + tolerance
        && (near(got.top, want.top) || near(got.bottom, want.bottom)))
    .then_some(got.height());

    (width, height)
}

#[cfg(test)]
mod tests {
    use super::learned_minimum;

    /// The tolerance the application actually passes: the same figure that
    /// decides whether a window is refusing at all. A few pixels of rounding
    /// must not be read as a declared minimum any more than it is read as a
    /// refusal.
    const TOL: i32 = 24;

    #[test]
    fn a_window_that_fitted_teaches_nothing() {
        let want = Rect::new(100, 100, 900, 700);
        assert_eq!(learned_minimum(want, want, TOL), (None, None));
    }

    #[test]
    fn holding_its_left_edge_and_overflowing_right_states_a_width() {
        // Straight from the log: asked for 1024 wide at x=1976, came back 1371
        // wide at the same x. Same left edge, so Chrome honoured the move and
        // declined the size.
        let want = Rect::new(1976, 1058, 1976 + 1024, 2117);
        let got = Rect::new(1976, 1058, 1976 + 1371, 2117);
        assert_eq!(learned_minimum(want, got, TOL).0, Some(1371));
    }

    #[test]
    fn holding_its_right_edge_and_overflowing_left_states_a_width() {
        let want = Rect::new(2555, 2, 2555 + 1157, 2117);
        let got = Rect::new(2391, 2, 2391 + 1321, 2117);
        assert_eq!(learned_minimum(want, got, TOL).0, Some(1321));
    }

    #[test]
    fn a_window_that_ignored_the_move_teaches_nothing() {
        // Bigger than asked, but neither edge landed where it was put: a window
        // that did not move at all, or one mid-drag. Reading a minimum out of
        // this is what froze GIMP.
        let want = Rect::new(2555, 2, 2555 + 1157, 2117);
        let got = Rect::new(1990, 40, 1990 + 1855, 2100);
        assert_eq!(learned_minimum(want, got, TOL), (None, None));
    }

    #[test]
    fn a_window_smaller_than_asked_teaches_nothing() {
        let want = Rect::new(100, 100, 1100, 900);
        let got = Rect::new(100, 100, 700, 500);
        assert_eq!(learned_minimum(want, got, TOL), (None, None));
    }

    #[test]
    fn the_axes_are_judged_independently() {
        let want = Rect::new(0, 0, 500, 400);
        let got = Rect::new(0, 0, 900, 400);
        assert_eq!(learned_minimum(want, got, TOL), (Some(900), None));
    }

    #[test]
    fn a_difference_within_tolerance_is_not_a_refusal() {
        // Five pixels out is the case that was wrongly condemning windows; it
        // must not be read as a stated minimum either.
        let want = Rect::new(1976, 1058, 1976 + 1024, 2117);
        let got = Rect::new(1976, 1058, 1976 + 1029, 2117);
        assert_eq!(learned_minimum(want, got, TOL), (None, None));
    }

    use super::squeeze_deficit;

    #[test]
    fn a_grid_with_dragged_splits_never_overlaps() {
        // The user-visible fault: after an uneven vertical drag, two windows in
        // different rows and columns overlapped. Grid keeps column boundaries
        // per row, so a row boundary moving must not leave the rows disagreeing
        // about where their columns are.
        let area = Rect::new(0, 0, 2000, 1200);
        for n in 2..=9usize {
            let mut sp = Splits::default();
            // An uneven row boundary, plus columns dragged differently in each
            // row -- exactly what dragging around a grid produces.
            sp.set(SplitAxis::Cross, 0, 0.23, 2);
            let rows = 3;
            for r in 0..rows {
                let cols = n.div_ceil(rows).max(1);
                for c in 0..cols.saturating_sub(1) {
                    let f = 0.17 + 0.11 * (r as f32) + 0.09 * (c as f32);
                    sp.set_grid_column(r, c, f.min(0.93), cols);
                }
            }
            let zones = compute_with(area, n, LayoutKind::Grid, &NO_GAP, &sp);
            assert_eq!(zones.len(), n);
            for (i, a) in zones.iter().enumerate() {
                for b in &zones[i + 1..] {
                    let ox = (a.right.min(b.right) - a.left.max(b.left)).max(0);
                    let oy = (a.bottom.min(b.bottom) - a.top.max(b.top)).max(0);
                    assert_eq!(ox * oy, 0, "{n} windows: {a:?} overlaps {b:?}");
                }
            }
        }
    }

    #[test]
    fn a_layout_that_fits_has_no_deficit() {
        let zones = [Rect::new(0, 0, 800, 600), Rect::new(800, 0, 1600, 600)];
        assert_eq!(squeeze_deficit(&zones, &[(400, 300), (400, 300)]), 0);
        assert_eq!(squeeze_deficit(&zones, &[(800, 600), (800, 600)]), 0);
    }

    #[test]
    fn the_deficit_counts_both_axes_of_every_window() {
        let zones = [Rect::new(0, 0, 300, 200), Rect::new(300, 0, 600, 200)];
        // 100 short across and 50 short down, twice over.
        assert_eq!(squeeze_deficit(&zones, &[(400, 250), (400, 250)]), 300);
    }

    #[test]
    fn a_worse_squeeze_scores_higher() {
        // This ordering is what the drag handler relies on.
        let mins = [(600, 400), (600, 400)];
        let roomy = [Rect::new(0, 0, 700, 500), Rect::new(700, 0, 1400, 500)];
        let tight = [Rect::new(0, 0, 500, 500), Rect::new(500, 0, 1400, 500)];
        let worse = [Rect::new(0, 0, 300, 500), Rect::new(300, 0, 1400, 500)];
        assert!(squeeze_deficit(&roomy, &mins) < squeeze_deficit(&tight, &mins));
        assert!(squeeze_deficit(&tight, &mins) < squeeze_deficit(&worse, &mins));
    }

    #[test]
    fn relieving_a_squeeze_lowers_the_score_even_when_one_remains() {
        // The case a "never violate" rule would freeze: too many windows to
        // ever satisfy, but the drag still lifts one of them out of deficit.
        let mins = [(900, 100), (400, 100), (100, 100)];
        let before = [
            Rect::new(0, 0, 200, 500),
            Rect::new(200, 0, 700, 500),
            Rect::new(700, 0, 1600, 500),
        ];
        let after = [
            Rect::new(0, 0, 900, 500),
            Rect::new(900, 0, 1300, 500),
            Rect::new(1300, 0, 1600, 500),
        ];
        assert_eq!(squeeze_deficit(&before, &mins), 700);
        assert_eq!(squeeze_deficit(&after, &mins), 0);
    }

    #[test]
    fn shifting_pixels_between_two_starved_windows_is_neither_better_nor_worse() {
        // Along one axis the total is fixed, so while both sides are below
        // their minimum the deficit is conserved however the boundary moves.
        // The drag handler rejects only a *worse* layout, so these moves stay
        // allowed -- which is what lets a user shuffle space around inside an
        // over-full monitor instead of being frozen.
        let mins = [(900, 100), (900, 100)];
        let a = [Rect::new(0, 0, 400, 500), Rect::new(400, 0, 1000, 500)];
        let b = [Rect::new(0, 0, 600, 500), Rect::new(600, 0, 1000, 500)];
        assert_eq!(squeeze_deficit(&a, &mins), squeeze_deficit(&b, &mins));
    }

    #[test]
    fn windows_without_a_minimum_never_contribute() {
        let zones = [Rect::new(0, 0, 1, 1), Rect::new(0, 0, 1, 1)];
        assert_eq!(squeeze_deficit(&zones, &[(0, 0), (0, 0)]), 0);
    }

    #[test]
    fn a_short_or_empty_list_is_not_a_panic() {
        assert_eq!(squeeze_deficit(&[], &[]), 0);
        // zip stops at the shorter side rather than indexing out of bounds.
        assert_eq!(squeeze_deficit(&[Rect::new(0, 0, 10, 10)], &[]), 0);
    }

    use super::fit_to_minimum;

    const SCREEN: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

    #[test]
    fn a_zone_that_already_holds_the_minimum_is_untouched() {
        let z = Rect::new(100, 100, 900, 700);
        assert_eq!(fit_to_minimum(z, (400, 300), SCREEN), z);
        assert_eq!(fit_to_minimum(z, (0, 0), SCREEN), z);
    }

    #[test]
    fn a_narrow_zone_grows_rightwards_and_keeps_its_origin() {
        let z = Rect::new(100, 100, 300, 700);
        let f = fit_to_minimum(z, (800, 0), SCREEN);
        assert_eq!((f.left, f.top), (100, 100), "the origin must not drift");
        assert_eq!(f.width(), 800);
        assert_eq!(f.height(), z.height(), "the other axis is not disturbed");
    }

    #[test]
    fn growth_slides_back_rather_than_running_off_the_screen() {
        let z = Rect::new(1700, 100, 1920, 700);
        let f = fit_to_minimum(z, (800, 0), SCREEN);
        assert_eq!(f.right, 1920);
        assert_eq!(f.width(), 800);
        assert!(f.left >= SCREEN.left);
    }

    #[test]
    fn a_minimum_larger_than_the_screen_is_pinned_to_the_screen() {
        let z = Rect::new(100, 100, 300, 300);
        let f = fit_to_minimum(z, (4000, 4000), SCREEN);
        assert_eq!(f.width(), SCREEN.width());
        assert_eq!(f.height(), SCREEN.height());
        assert_eq!((f.left, f.top), (0, 0), "must stay on the monitor");
    }

    #[test]
    fn fitting_is_idempotent() {
        let z = Rect::new(1700, 900, 1920, 1000);
        let once = fit_to_minimum(z, (600, 500), SCREEN);
        assert_eq!(fit_to_minimum(once, (600, 500), SCREEN), once);
    }

    use super::*;

    const AREA: Rect = Rect::new(0, 0, 1920, 1080);
    const NO_GAP: LayoutParams = LayoutParams {
        outer_gap: 0,
        inner_gap: 0,
        master_fraction: 0.55,
        master_count: 1,
    };

    fn total_area(rs: &[Rect]) -> i64 {
        rs.iter().map(|r| r.area()).sum()
    }

    fn any_overlap(rs: &[Rect]) -> bool {
        for i in 0..rs.len() {
            for j in (i + 1)..rs.len() {
                if rs[i].intersection_area(&rs[j]) > 0 {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn zero_windows_yields_no_zones() {
        for kind in LayoutKind::ALL {
            assert!(compute(AREA, 0, kind, &NO_GAP).is_empty());
        }
    }

    #[test]
    fn always_returns_requested_count() {
        for kind in LayoutKind::ALL {
            for n in 1..=24 {
                assert_eq!(compute(AREA, n, kind, &NO_GAP).len(), n, "{kind:?} n={n}");
            }
        }
    }

    #[test]
    fn no_zone_is_empty_or_inverted() {
        for kind in LayoutKind::ALL {
            for n in 1..=24 {
                for z in compute(AREA, n, kind, &LayoutParams::default()) {
                    assert!(z.right > z.left, "{kind:?} n={n} inverted x: {z:?}");
                    assert!(z.bottom > z.top, "{kind:?} n={n} inverted y: {z:?}");
                }
            }
        }
    }

    /// The core correctness property: with no gaps, the zones exactly partition
    /// the work area — total area matches, and nothing overlaps.
    #[test]
    fn tiling_is_exact_and_gapless() {
        let sizes = [
            Rect::new(0, 0, 1920, 1080),
            Rect::new(0, 0, 2560, 1440),
            Rect::new(0, 0, 3840, 2160),
            Rect::new(0, 0, 1366, 768),
            Rect::new(-1920, 0, 0, 1080), // negative-origin (left-of-primary monitor)
            Rect::new(0, 0, 1001, 733),   // deliberately prime-ish
        ];
        for area in sizes {
            for kind in [
                LayoutKind::Grid,
                LayoutKind::Columns,
                LayoutKind::Rows,
                LayoutKind::MasterStack,
                LayoutKind::Dwindle,
            ] {
                for n in 1..=17 {
                    let zones = compute(area, n, kind, &NO_GAP);
                    assert!(!any_overlap(&zones), "{kind:?} n={n} {area:?} overlaps");
                    assert_eq!(
                        total_area(&zones),
                        area.area(),
                        "{kind:?} n={n} {area:?} does not tile exactly"
                    );
                }
            }
        }
    }

    #[test]
    fn zones_stay_inside_work_area() {
        for kind in LayoutKind::ALL {
            for n in 1..=16 {
                for z in compute(AREA, n, kind, &LayoutParams::default()) {
                    assert!(
                        z.left >= AREA.left && z.right <= AREA.right,
                        "{kind:?} {z:?}"
                    );
                    assert!(
                        z.top >= AREA.top && z.bottom <= AREA.bottom,
                        "{kind:?} {z:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn bsp_has_a_fallback_shape_for_callers_without_a_tree() {
        for n in 1..=10 {
            let z = compute(AREA, n, LayoutKind::Bsp, &NO_GAP);
            assert_eq!(z.len(), n);
            assert_eq!(total_area(&z), AREA.area());
            assert!(!any_overlap(&z));
        }
    }

    #[test]
    fn monocle_gives_everyone_the_whole_area() {
        let zones = compute(AREA, 5, LayoutKind::Monocle, &NO_GAP);
        assert!(zones.iter().all(|z| *z == AREA));
    }

    #[test]
    fn single_window_fills_area_in_every_layout() {
        for kind in LayoutKind::ALL {
            assert_eq!(compute(AREA, 1, kind, &NO_GAP), vec![AREA], "{kind:?}");
        }
    }

    #[test]
    fn gaps_shrink_zones_but_keep_them_valid() {
        let p = LayoutParams {
            outer_gap: 20,
            inner_gap: 10,
            ..Default::default()
        };
        let zones = compute(AREA, 4, LayoutKind::Grid, &p);
        assert_eq!(zones.len(), 4);
        assert!(!any_overlap(&zones));
        assert!(total_area(&zones) < AREA.area());
        for z in &zones {
            assert!(z.left >= AREA.left + 20 - 5);
            assert!(!z.is_empty());
        }
    }

    /// Gaps larger than the monitor must degrade gracefully, not invert.
    #[test]
    fn absurd_gaps_do_not_produce_inverted_rects() {
        let p = LayoutParams {
            outer_gap: 400,
            inner_gap: 400,
            master_fraction: 0.5,
            master_count: 1,
        };
        let tiny = Rect::new(0, 0, 300, 200);
        for kind in LayoutKind::ALL {
            for z in compute(tiny, 4, kind, &p) {
                assert!(z.right >= z.left, "{kind:?} {z:?}");
                assert!(z.bottom >= z.top, "{kind:?} {z:?}");
            }
        }
    }

    #[test]
    fn params_are_clamped() {
        let p = LayoutParams {
            outer_gap: -50,
            inner_gap: 100_000,
            master_fraction: f32::NAN,
            master_count: 0,
        }
        .sanitized();
        assert_eq!(p.outer_gap, 0);
        assert_eq!(p.inner_gap, 400);
        assert_eq!(p.master_fraction, 0.55);
        assert_eq!(p.master_count, 1);

        let p2 = LayoutParams {
            master_fraction: f32::INFINITY,
            ..Default::default()
        }
        .sanitized();
        assert!(p2.master_fraction.is_finite());
    }

    #[test]
    fn master_stack_honours_fraction() {
        let p = LayoutParams {
            master_fraction: 0.6,
            ..NO_GAP
        };
        let zones = compute(AREA, 3, LayoutKind::MasterStack, &p);
        assert_eq!(zones[0].width(), 1152); // 1920 * 0.6
        assert_eq!(zones[0].height(), 1080);
        assert_eq!(zones[1].width(), 768);
    }

    #[test]
    fn master_stack_all_master_degenerates_to_rows() {
        let p = LayoutParams {
            master_count: 8,
            ..NO_GAP
        };
        let zones = compute(AREA, 3, LayoutKind::MasterStack, &p);
        assert_eq!(zones, compute(AREA, 3, LayoutKind::Rows, &NO_GAP));
    }

    #[test]
    fn grid_shape_is_near_square() {
        let z4 = compute(AREA, 4, LayoutKind::Grid, &NO_GAP);
        assert_eq!(z4[0], Rect::new(0, 0, 960, 540));
        assert_eq!(z4[3], Rect::new(960, 540, 1920, 1080));
    }

    #[test]
    fn grid_last_row_stretches() {
        // 3 windows -> 2 cols, 2 rows; the single window in row 2 spans full width.
        let z = compute(AREA, 3, LayoutKind::Grid, &NO_GAP);
        assert_eq!(z[2], Rect::new(0, 540, 1920, 1080));
    }

    #[test]
    fn layout_cycling_wraps_both_ways() {
        let mut k = LayoutKind::Grid;
        for _ in 0..LayoutKind::ALL.len() {
            k = k.next();
        }
        assert_eq!(k, LayoutKind::Grid);
        assert_eq!(LayoutKind::Grid.prev(), LayoutKind::Bsp);
        assert_eq!(LayoutKind::Bsp.next(), LayoutKind::Grid);
        assert_eq!(LayoutKind::Monocle.next(), LayoutKind::Bsp);
    }

    #[test]
    fn neighbour_navigation_in_a_2x2_grid() {
        let z = compute(AREA, 4, LayoutKind::Grid, &NO_GAP);
        // 0 1
        // 2 3
        assert_eq!(neighbour(&z, 0, Direction::Right), Some(1));
        assert_eq!(neighbour(&z, 0, Direction::Down), Some(2));
        assert_eq!(neighbour(&z, 3, Direction::Left), Some(2));
        assert_eq!(neighbour(&z, 3, Direction::Up), Some(1));
        assert_eq!(neighbour(&z, 0, Direction::Left), None);
        assert_eq!(neighbour(&z, 0, Direction::Up), None);
    }

    #[test]
    fn neighbour_out_of_range_is_none() {
        let z = compute(AREA, 4, LayoutKind::Grid, &NO_GAP);
        assert_eq!(neighbour(&z, 99, Direction::Left), None);
        assert_eq!(neighbour(&[], 0, Direction::Left), None);
    }

    #[test]
    fn zone_at_finds_container_then_nearest() {
        let z = compute(AREA, 4, LayoutKind::Grid, &NO_GAP);
        assert_eq!(zone_at(&z, 10, 10), Some(0));
        assert_eq!(zone_at(&z, 1900, 1000), Some(3));
        // Outside the area entirely -> nearest centre.
        assert_eq!(zone_at(&z, -500, -500), Some(0));
        assert_eq!(zone_at(&[], 0, 0), None);
    }

    #[test]
    fn best_overlap_picks_dominant_zone() {
        let z = compute(AREA, 4, LayoutKind::Grid, &NO_GAP);
        // Mostly inside zone 0, slightly into zone 1.
        let w = Rect::new(0, 0, 1000, 540);
        assert_eq!(best_overlap(&z, &w), Some(0));
        // No overlap at all.
        assert_eq!(best_overlap(&z, &Rect::new(5000, 5000, 5100, 5100)), None);
    }

    #[test]
    fn deflate_never_inverts() {
        let r = Rect::new(0, 0, 10, 4);
        let d = r.deflate(100);
        assert!(d.right >= d.left && d.bottom >= d.top);
        assert_eq!(r.deflate(-5), r, "negative deflate is a no-op");
    }

    // --- user-dragged splits ------------------------------------------

    #[test]
    fn no_splits_behaves_exactly_like_equal_division() {
        for kind in LayoutKind::ALL {
            for n in 1..=12 {
                assert_eq!(
                    compute_with(AREA, n, kind, &NO_GAP, &Splits::default()),
                    compute(AREA, n, kind, &NO_GAP),
                    "{kind:?} n={n}"
                );
            }
        }
    }

    #[test]
    fn a_dragged_column_boundary_moves_only_its_two_neighbours() {
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 0, 0.25, 3); // first of two boundaries
        let z = compute_with(AREA, 3, LayoutKind::Columns, &NO_GAP, &sp);
        assert_eq!(z[0].width(), 480, "first column should be 25%");
        // The untouched boundary stays where equal division put it.
        assert_eq!(z[1].right, 1280);
        assert_eq!(z[2].right, AREA.right);
    }

    #[test]
    fn dragged_splits_still_tile_the_area_exactly() {
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 0, 0.2, 4);
        sp.set(SplitAxis::Main, 2, 0.9, 4);
        for kind in [LayoutKind::Columns, LayoutKind::Rows] {
            let z = compute_with(AREA, 4, kind, &NO_GAP, &sp);
            assert!(!any_overlap(&z), "{kind:?} overlaps");
            assert_eq!(
                total_area(&z),
                AREA.area(),
                "{kind:?} does not tile exactly"
            );
        }
    }

    #[test]
    fn a_boundary_cannot_collapse_its_neighbour() {
        // Dragging an edge to the far wall must leave the other zone usable.
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 0, 0.0, 2);
        let z = compute_with(AREA, 2, LayoutKind::Columns, &NO_GAP, &sp);
        assert!(z[0].width() > 0, "first zone collapsed: {z:?}");
        assert!(z[1].width() > 0);

        sp.set(SplitAxis::Main, 0, 1.0, 2);
        let z = compute_with(AREA, 2, LayoutKind::Columns, &NO_GAP, &sp);
        assert!(z[0].width() > 0);
        assert!(z[1].width() > 0, "second zone collapsed: {z:?}");
    }

    #[test]
    fn boundaries_are_forced_into_increasing_order() {
        // A caller writing nonsense must not produce inverted zones.
        let sp = Splits {
            main: vec![0.8, 0.2, 0.5],
            cross: vec![],
            grid_rows: vec![],
        };
        let z = compute_with(AREA, 4, LayoutKind::Columns, &NO_GAP, &sp);
        for w in z.windows(2) {
            assert!(w[0].right <= w[1].left, "zones out of order: {z:?}");
        }
        for r in &z {
            assert!(r.width() > 0, "collapsed zone in {z:?}");
        }
        assert_eq!(total_area(&z), AREA.area());
    }

    #[test]
    fn non_finite_boundaries_fall_back_to_equal_splits() {
        let sp = Splits {
            main: vec![f32::NAN, 0.5],
            cross: vec![],
            grid_rows: vec![],
        };
        assert_eq!(
            compute_with(AREA, 3, LayoutKind::Columns, &NO_GAP, &sp),
            compute(AREA, 3, LayoutKind::Columns, &NO_GAP)
        );
    }

    #[test]
    fn a_stale_boundary_count_is_ignored() {
        // Two boundaries stored, but four zones now: fall back rather than
        // mis-assign them.
        let sp = Splits {
            main: vec![0.3, 0.6],
            cross: vec![],
            grid_rows: vec![],
        };
        assert_eq!(
            compute_with(AREA, 4, LayoutKind::Columns, &NO_GAP, &sp),
            compute(AREA, 4, LayoutKind::Columns, &NO_GAP)
        );
    }

    #[test]
    fn setting_one_boundary_materialises_the_rest_where_they_were() {
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 1, 0.7, 4);
        assert_eq!(sp.main.len(), 3);
        // Untouched boundaries keep their equal-split positions.
        assert!((sp.main[0] - 0.25).abs() < 1e-6, "{:?}", sp.main);
        assert!((sp.main[2] - 0.75).abs() < 1e-6, "{:?}", sp.main);
        assert!((sp.main[1] - 0.7).abs() < 1e-6);
    }

    #[test]
    fn setting_a_boundary_is_ignored_when_it_cannot_apply() {
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 0, 0.5, 1); // only one zone: no boundaries
        assert!(sp.is_empty());
        sp.set(SplitAxis::Main, 9, 0.5, 3); // index past the end
        assert_eq!(sp.main.len(), 2);
        sp.set(SplitAxis::Main, 0, f32::NAN, 3);
        assert!(sp.main.iter().all(|f| f.is_finite()));
    }

    #[test]
    fn a_dragged_master_edge_overrides_the_configured_fraction() {
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 0, 0.3, 2);
        let p = LayoutParams {
            master_fraction: 0.7,
            ..NO_GAP
        };
        let z = compute_with(AREA, 3, LayoutKind::MasterStack, &p, &sp);
        assert_eq!(z[0].width(), 576, "master should follow the drag, not 0.7");
    }

    #[test]
    fn each_grid_row_resizes_its_own_columns() {
        // The reported bug: dragging the divider on one row moved every row.
        // 6 windows => 3 columns, 2 rows.
        let mut sp = Splits::default();
        sp.set_grid_column(1, 0, 0.15, 3); // bottom row, first divider
        let z = compute_with(AREA, 6, LayoutKind::Grid, &NO_GAP, &sp);

        // Bottom row (indices 3,4,5) moved.
        assert_eq!(z[3].width(), 288, "bottom row should follow the drag");
        // Top row (indices 0,1,2) did not.
        assert_eq!(z[0].width(), 640, "top row must be untouched");

        // Still exact.
        assert_eq!(total_area(&z), AREA.area());
        assert!(!any_overlap(&z));
    }

    #[test]
    fn a_short_final_row_is_resizable_too() {
        // 5 windows => 3 columns, rows of 3 and 2. The bottom row is short;
        // it used to be pinned to equal splits and snap back on every drag.
        let mut sp = Splits::default();
        sp.set_grid_column(1, 0, 0.25, 2); // bottom row has 2 cells, 1 divider
        let z = compute_with(AREA, 5, LayoutKind::Grid, &NO_GAP, &sp);
        assert_eq!(z[3].width(), 480, "short bottom row should follow the drag");
        assert_eq!(z[0].width(), 640, "the full top row must be untouched");
        assert_eq!(total_area(&z), AREA.area());
        assert!(!any_overlap(&z));
    }

    #[test]
    fn a_row_whose_width_changed_reverts_to_equal_splits() {
        // Boundaries stored for a 2-cell row, then a window opens and the row
        // has 3. The stale vector must be ignored, not misapplied.
        let mut sp = Splits::default();
        sp.set_grid_column(1, 0, 0.25, 2);
        let z = compute_with(AREA, 6, LayoutKind::Grid, &NO_GAP, &sp);
        assert_eq!(z[3].width(), 640, "stale row boundaries should be ignored");
        assert_eq!(total_area(&z), AREA.area());
    }

    #[test]
    fn a_grid_row_falls_back_to_the_shared_boundaries() {
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 0, 0.2, 3);
        let z = compute_with(AREA, 6, LayoutKind::Grid, &NO_GAP, &sp);
        // Both rows use `main` until one of them is dragged individually.
        assert_eq!(z[0].width(), z[3].width());
        assert_eq!(z[0].width(), 384);
    }

    #[test]
    fn dragging_one_row_seeds_from_the_shared_boundaries() {
        // Row 1 is dragged after a shared boundary was already set; row 1's
        // untouched dividers should keep the shared positions, not jump to
        // equal splits.
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 0, 0.2, 3);
        sp.set(SplitAxis::Main, 1, 0.5, 3);
        sp.set_grid_column(1, 0, 0.3, 3);
        let z = compute_with(AREA, 6, LayoutKind::Grid, &NO_GAP, &sp);
        assert_eq!(z[3].width(), 576, "dragged divider moved");
        // The second divider of row 1 stayed at the shared 0.5.
        assert_eq!(z[4].right, 960);
    }

    #[test]
    fn per_row_grid_columns_round_trip_and_clear() {
        let mut sp = Splits::default();
        sp.set_grid_column(0, 0, 0.4, 2);
        assert!(!sp.is_empty());
        let text = serde_json::to_string(&sp).unwrap();
        assert_eq!(serde_json::from_str::<Splits>(&text).unwrap(), sp);
        sp.clear();
        assert!(sp.is_empty());
    }

    #[test]
    fn setting_a_grid_column_is_ignored_when_it_cannot_apply() {
        let mut sp = Splits::default();
        sp.set_grid_column(0, 0, 0.5, 1); // one column: no dividers
        assert!(sp.grid_rows.iter().all(|r| r.is_empty()));
        sp.set_grid_column(0, 9, 0.5, 3); // index past the end
        assert_eq!(sp.grid_rows[0].len(), 2);
        sp.set_grid_column(0, 0, f32::NAN, 3);
        assert!(sp.grid_rows[0].iter().all(|f| f.is_finite()));
    }

    #[test]
    fn grid_row_and_column_boundaries_are_independent() {
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 0, 0.3, 2); // 2 columns for n=4
        sp.set(SplitAxis::Cross, 0, 0.8, 2); // 2 rows
        let z = compute_with(AREA, 4, LayoutKind::Grid, &NO_GAP, &sp);
        assert_eq!(z[0].width(), 576, "column boundary at 30%");
        assert_eq!(z[0].height(), 864, "row boundary at 80%");
        assert_eq!(total_area(&z), AREA.area());
        assert!(!any_overlap(&z));
    }

    #[test]
    fn dragged_grid_zones_stay_non_degenerate_with_gaps() {
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 0, 0.01, 3);
        sp.set(SplitAxis::Cross, 0, 0.99, 3);
        for n in 1..=16 {
            for z in compute_with(AREA, n, LayoutKind::Grid, &LayoutParams::default(), &sp) {
                assert!(z.width() > 0 && z.height() > 0, "n={n} {z:?}");
            }
        }
    }

    #[test]
    fn splits_round_trip_through_json() {
        let sp = Splits {
            main: vec![0.25, 0.5],
            cross: vec![0.4],
            grid_rows: vec![],
        };
        let text = serde_json::to_string(&sp).unwrap();
        assert_eq!(serde_json::from_str::<Splits>(&text).unwrap(), sp);
        // Missing fields default to empty.
        assert!(serde_json::from_str::<Splits>("{}").unwrap().is_empty());
    }

    #[test]
    fn clearing_splits_restores_equal_division() {
        let mut sp = Splits::default();
        sp.set(SplitAxis::Main, 0, 0.1, 3);
        assert!(!sp.is_empty());
        sp.clear();
        assert!(sp.is_empty());
        assert_eq!(
            compute_with(AREA, 3, LayoutKind::Columns, &NO_GAP, &sp),
            compute(AREA, 3, LayoutKind::Columns, &NO_GAP)
        );
    }

    #[test]
    fn negative_origin_monitors_tile_correctly() {
        // Second monitor to the left of primary => negative coordinates.
        let area = Rect::new(-2560, -200, 0, 1240);
        let zones = compute(area, 4, LayoutKind::Grid, &NO_GAP);
        assert_eq!(total_area(&zones), area.area());
        assert!(!any_overlap(&zones));
        assert!(zones
            .iter()
            .all(|z| z.left >= area.left && z.right <= area.right));
    }
}
