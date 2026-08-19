//! Interactive drag: resizing a tile, and moving one into another zone.
//!
//! Windows gives us `EVENT_SYSTEM_MOVESIZESTART` and `…MOVESIZEEND` but nothing
//! in between, so the application polls the dragged window on a short timer
//! while a drag is live. Everything in *this* module is pure geometry, so the
//! part that is easy to get wrong — which boundary an edge belongs to — is
//! testable without a mouse.
//!
//! ## Resizing
//!
//! A tiling window manager cannot honour a free-form resize: there is no room
//! for a window to grow into except a neighbour. So a drag is interpreted as
//! *moving a boundary*. [`resize_to_boundary`] works out which edge the user
//! pulled and which entry of [`Splits`] it corresponds to; the layout engine
//! then re-derives every zone from that one number, and the neighbour gives up
//! exactly the space the dragged window gained.
//!
//! ## Moving
//!
//! Dragging a window over another zone swaps the two. [`target_zone`] resolves
//! the cursor to a zone index so the preview overlay can show the result before
//! the button is released.

use crate::layout::{LayoutKind, Rect, SplitAxis};

/// Which side of a window the user pulled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// What a drag turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragKind {
    /// The window changed size: the user pulled an edge or corner.
    Resize,
    /// The window changed position only: the user is relocating it.
    Move,
}

/// Pixels an edge must move before it counts as deliberate.
///
/// Below this, a "resize" is usually just the shell nudging a window, or the
/// invisible-border compensation being a pixel out.
pub const EDGE_THRESHOLD: i32 = 6;

/// Classify a drag by comparing the window's current rect to where it started.
pub fn classify(start: Rect, now: Rect, threshold: i32) -> DragKind {
    let dw = (now.width() - start.width()).abs();
    let dh = (now.height() - start.height()).abs();
    if dw > threshold || dh > threshold {
        DragKind::Resize
    } else {
        DragKind::Move
    }
}

/// Which edges of `zone` the window has been pulled away from.
///
/// Compares against the *zone* rather than the drag's starting rect, because
/// the zone is where the tiler put the window and therefore where its edges
/// are supposed to be.
pub fn moved_edges(zone: Rect, now: Rect, threshold: i32) -> Vec<Edge> {
    let mut v = Vec::with_capacity(2);
    if (now.left - zone.left).abs() > threshold {
        v.push(Edge::Left);
    }
    if (now.right - zone.right).abs() > threshold {
        v.push(Edge::Right);
    }
    if (now.top - zone.top).abs() > threshold {
        v.push(Edge::Top);
    }
    if (now.bottom - zone.bottom).abs() > threshold {
        v.push(Edge::Bottom);
    }
    v
}

/// Grid geometry for `n` zones: (columns, rows).
fn grid_shape(n: usize) -> (usize, usize) {
    let cols = ((n as f64).sqrt().ceil() as usize).max(1);
    (cols, n.div_ceil(cols))
}

/// A boundary edit derived from a drag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundaryEdit {
    pub axis: SplitAxis,
    /// Index into the axis's boundary vector.
    pub index: usize,
    /// New position, as a fraction of the work area along that axis.
    pub fraction: f32,
    /// How many zones share that axis, so [`crate::layout::Splits::set`] can
    /// size the vector correctly.
    pub count: usize,
    /// Grid only: the row this column boundary belongs to.
    ///
    /// `Some(r)` routes the edit to
    /// [`crate::layout::Splits::set_grid_column`] so only that row moves.
    pub grid_row: Option<usize>,
}

/// Translate "the user pulled this edge of zone `index` to here" into the
/// boundary that should move.
///
/// Returns `None` when the edge has no boundary behind it — the outer edge of
/// the work area, or a layout with no adjustable splits.
// Every argument is a distinct fact about the drag; a struct would just move
// the same fields somewhere else and make the call sites longer.
#[allow(clippy::too_many_arguments)]
pub fn resize_to_boundary(
    kind: LayoutKind,
    index: usize,
    count: usize,
    master_count: usize,
    area: Rect,
    now: Rect,
    edge: Edge,
) -> Option<BoundaryEdit> {
    if count < 2 || area.width() <= 0 || area.height() <= 0 || index >= count {
        return None;
    }
    let fx = |x: i32| (x - area.left) as f32 / area.width() as f32;
    let fy = |y: i32| (y - area.top) as f32 / area.height() as f32;

    match kind {
        // No adjustable boundaries: every zone is the whole area, or the
        // split tree is derived rather than stored.
        //
        // Bsp is absent for a different reason: its boundaries live in the
        // tree, so a resize is routed through `tree::Tree::set_ratio` instead
        // of through `Splits`.
        LayoutKind::Monocle | LayoutKind::Dwindle | LayoutKind::Bsp => None,

        LayoutKind::Columns => match edge {
            Edge::Left if index > 0 => Some(BoundaryEdit {
                axis: SplitAxis::Main,
                index: index - 1,
                fraction: fx(now.left),
                count,
                grid_row: None,
            }),
            Edge::Right if index + 1 < count => Some(BoundaryEdit {
                axis: SplitAxis::Main,
                index,
                fraction: fx(now.right),
                count,
                grid_row: None,
            }),
            _ => None,
        },

        LayoutKind::Rows => match edge {
            Edge::Top if index > 0 => Some(BoundaryEdit {
                axis: SplitAxis::Main,
                index: index - 1,
                fraction: fy(now.top),
                count,
                grid_row: None,
            }),
            Edge::Bottom if index + 1 < count => Some(BoundaryEdit {
                axis: SplitAxis::Main,
                index,
                fraction: fy(now.bottom),
                count,
                grid_row: None,
            }),
            _ => None,
        },

        LayoutKind::Grid => {
            let (cols, rows) = grid_shape(count);
            let col = index % cols;
            let row = index / cols;
            // A final row may hold fewer cells than there are columns, and its
            // dividers belong to that row's own count -- not to `cols`, which
            // would size the boundary vector wrongly and make the row snap
            // back to equal splits.
            let cells_in_row = count.saturating_sub(row * cols).min(cols);
            match edge {
                // Column edits are scoped to this row; row edits are global,
                // because a row band spans the full width.
                Edge::Left if col > 0 => Some(BoundaryEdit {
                    axis: SplitAxis::Main,
                    index: col - 1,
                    fraction: fx(now.left),
                    count: cells_in_row,
                    grid_row: Some(row),
                }),
                Edge::Right if col + 1 < cells_in_row => Some(BoundaryEdit {
                    axis: SplitAxis::Main,
                    index: col,
                    fraction: fx(now.right),
                    count: cells_in_row,
                    grid_row: Some(row),
                }),
                Edge::Top if row > 0 => Some(BoundaryEdit {
                    axis: SplitAxis::Cross,
                    index: row - 1,
                    fraction: fy(now.top),
                    count: rows,
                    grid_row: None,
                }),
                Edge::Bottom if row + 1 < rows => Some(BoundaryEdit {
                    axis: SplitAxis::Cross,
                    index: row,
                    fraction: fy(now.bottom),
                    count: rows,
                    grid_row: None,
                }),
                _ => None,
            }
        }

        LayoutKind::MasterStack => {
            let masters = master_count.clamp(1, count);
            if masters >= count {
                // Degenerates to Rows.
                return resize_to_boundary(
                    LayoutKind::Rows,
                    index,
                    count,
                    masters,
                    area,
                    now,
                    edge,
                );
            }
            let stack_len = count - masters;
            if index < masters {
                // A master pane: only its right edge is the shared boundary.
                match edge {
                    Edge::Right => Some(BoundaryEdit {
                        axis: SplitAxis::Main,
                        index: 0,
                        fraction: fx(now.right),
                        // The master/stack split is one boundary between two
                        // panes, regardless of how many windows are in each.
                        count: 2,
                        grid_row: None,
                    }),
                    Edge::Bottom if index + 1 < masters => Some(BoundaryEdit {
                        axis: SplitAxis::Cross,
                        index,
                        fraction: fy(now.bottom),
                        count: masters,
                        grid_row: None,
                    }),
                    Edge::Top if index > 0 => Some(BoundaryEdit {
                        axis: SplitAxis::Cross,
                        index: index - 1,
                        fraction: fy(now.top),
                        count: masters,
                        grid_row: None,
                    }),
                    _ => None,
                }
            } else {
                let j = index - masters;
                match edge {
                    Edge::Left => Some(BoundaryEdit {
                        axis: SplitAxis::Main,
                        index: 0,
                        fraction: fx(now.left),
                        count: 2,
                        grid_row: None,
                    }),
                    Edge::Top if j > 0 => Some(BoundaryEdit {
                        axis: SplitAxis::Cross,
                        index: j - 1,
                        fraction: fy(now.top),
                        count: stack_len,
                        grid_row: None,
                    }),
                    Edge::Bottom if j + 1 < stack_len => Some(BoundaryEdit {
                        axis: SplitAxis::Cross,
                        index: j,
                        fraction: fy(now.bottom),
                        count: stack_len,
                        grid_row: None,
                    }),
                    _ => None,
                }
            }
        }
    }
}

/// Every boundary edit a drag implies — at most one per axis.
///
/// A corner drag moves a horizontal *and* a vertical edge, on two independent
/// boundaries. Returning only the edge that moved furthest meant a corner
/// resized along one axis and ignored the other, which reads as the drag
/// simply not working in one direction.
///
/// Within an axis the furthest-moved edge still wins: an edge cannot be pulled
/// in two directions at once.
#[allow(clippy::too_many_arguments)]
pub fn best_edits(
    kind: LayoutKind,
    index: usize,
    count: usize,
    master_count: usize,
    area: Rect,
    zone: Rect,
    now: Rect,
    threshold: i32,
) -> Vec<BoundaryEdit> {
    let mut per_axis: Vec<(SplitAxis, i32, BoundaryEdit)> = Vec::with_capacity(2);
    for edge in moved_edges(zone, now, threshold) {
        let delta = match edge {
            Edge::Left => (now.left - zone.left).abs(),
            Edge::Right => (now.right - zone.right).abs(),
            Edge::Top => (now.top - zone.top).abs(),
            Edge::Bottom => (now.bottom - zone.bottom).abs(),
        };
        let Some(edit) = resize_to_boundary(kind, index, count, master_count, area, now, edge)
        else {
            continue;
        };
        match per_axis.iter_mut().find(|(a, _, _)| *a == edit.axis) {
            Some(slot) if delta > slot.1 => *slot = (edit.axis, delta, edit),
            Some(_) => {}
            None => per_axis.push((edit.axis, delta, edit)),
        }
    }
    per_axis.into_iter().map(|(_, _, e)| e).collect()
}

/// The single best boundary edit for a drag.
///
/// Corner drags move two edges at once; the one that moved furthest is the one
/// the user is steering. Prefer [`best_edits`], which handles both axes.
#[allow(clippy::too_many_arguments)]
pub fn best_edit(
    kind: LayoutKind,
    index: usize,
    count: usize,
    master_count: usize,
    area: Rect,
    zone: Rect,
    now: Rect,
    threshold: i32,
) -> Option<BoundaryEdit> {
    let mut best: Option<(i32, BoundaryEdit)> = None;
    for edge in moved_edges(zone, now, threshold) {
        let delta = match edge {
            Edge::Left => (now.left - zone.left).abs(),
            Edge::Right => (now.right - zone.right).abs(),
            Edge::Top => (now.top - zone.top).abs(),
            Edge::Bottom => (now.bottom - zone.bottom).abs(),
        };
        if let Some(edit) = resize_to_boundary(kind, index, count, master_count, area, now, edge) {
            if best.is_none_or(|(d, _)| delta > d) {
                best = Some((delta, edit));
            }
        }
    }
    best.map(|(_, e)| e)
}

/// The zone the cursor is over, for drag-to-swap.
pub fn target_zone(zones: &[Rect], x: i32, y: i32) -> Option<usize> {
    crate::layout::zone_at(zones, x, y)
}

/// What dropping a window at a given point over another window would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropAction {
    /// Exchange the two windows' slots.
    Swap,
    /// Place the dragged window immediately before the target.
    InsertBefore,
    /// Place the dragged window immediately after the target.
    InsertAfter,
}

impl DropAction {
    /// Short label for the drop overlay.
    ///
    /// Says what actually happens. An earlier version promised "New column
    /// right", which was not true: the dragged window is already part of the
    /// layout, so moving it in the order re-orders the existing columns rather
    /// than adding one. Creating a genuine new column -- splitting one
    /// window's cell while its neighbours keep their span -- needs the BSP
    /// tree, and a caption must not promise it before it exists.
    pub fn label(self, side: Side) -> &'static str {
        match (self, side) {
            (DropAction::Swap, _) => "Swap",
            (DropAction::InsertBefore, Side::Horizontal) => "Place left",
            (DropAction::InsertAfter, Side::Horizontal) => "Place right",
            (DropAction::InsertBefore, Side::Vertical) => "Place above",
            (DropAction::InsertAfter, Side::Vertical) => "Place below",
            // Centre always swaps; an insert there is not constructible.
            (_, Side::Centre) => "Swap",
        }
    }
}

/// Which axis a drop targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Horizontal,
    Vertical,
    /// The centre region, which swaps rather than inserting.
    Centre,
}

/// A resolved drop: what would happen, where, and the region to highlight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drop {
    /// Index of the window being dropped onto.
    pub target: usize,
    pub action: DropAction,
    pub side: Side,
    /// The part of the target zone to highlight — the band for an insert, the
    /// whole zone for a swap.
    pub highlight: Rect,
}

impl Drop {
    pub fn label(&self) -> &'static str {
        match self.side {
            Side::Centre => "Swap",
            other => self.action.label(other),
        }
    }
}

/// Fraction of a zone's width or height each edge band occupies.
///
/// 20% at each end leaves a 60% centre, so aiming at an edge is deliberate
/// rather than something you hit by accident while crossing a window.
pub const EDGE_BAND: f32 = 0.20;

/// Resolve a cursor position over a set of zones into a drop action.
///
/// The bands are cross-checked: the right band is only the right band when the
/// cursor is also within the middle 60% vertically. Without that, the corners
/// of a zone would belong to two bands at once and the action would flicker
/// between them as the pointer moved a pixel.
pub fn drop_action(zones: &[Rect], x: i32, y: i32) -> Option<Drop> {
    let target = target_zone(zones, x, y)?;
    let z = *zones.get(target)?;
    if z.width() <= 0 || z.height() <= 0 {
        return None;
    }

    let u = (x - z.left) as f32 / z.width() as f32;
    let v = (y - z.top) as f32 / z.height() as f32;
    let lo = EDGE_BAND;
    let hi = 1.0 - EDGE_BAND;
    let bw = (z.width() as f32 * EDGE_BAND).round() as i32;
    let bh = (z.height() as f32 * EDGE_BAND).round() as i32;

    // Horizontal bands take priority only when the cursor is vertically
    // centred, and vice versa; the corners fall through to a swap.
    let centred_v = (lo..=hi).contains(&v);
    let centred_h = (lo..=hi).contains(&u);

    if u < lo && centred_v {
        return Some(Drop {
            target,
            action: DropAction::InsertBefore,
            side: Side::Horizontal,
            highlight: Rect::new(z.left, z.top, z.left + bw, z.bottom),
        });
    }
    if u > hi && centred_v {
        return Some(Drop {
            target,
            action: DropAction::InsertAfter,
            side: Side::Horizontal,
            highlight: Rect::new(z.right - bw, z.top, z.right, z.bottom),
        });
    }
    if v < lo && centred_h {
        return Some(Drop {
            target,
            action: DropAction::InsertBefore,
            side: Side::Vertical,
            highlight: Rect::new(z.left, z.top, z.right, z.top + bh),
        });
    }
    if v > hi && centred_h {
        return Some(Drop {
            target,
            action: DropAction::InsertAfter,
            side: Side::Vertical,
            highlight: Rect::new(z.left, z.bottom - bh, z.right, z.bottom),
        });
    }
    Some(Drop {
        target,
        action: DropAction::Swap,
        side: Side::Centre,
        highlight: z,
    })
}

/// Hold `now` to the dragged window's minimum track size.
///
/// A window with a minimum does not shrink past it. Ask anyway and it clamps
/// itself, ending up wider than the cell it was given and overlapping its
/// neighbour -- which is what made Signal and Steam look erratic. Clamping the
/// rectangle before any fraction is derived from it means the layout is never
/// asked for a cell the occupant cannot fill.
///
/// Which edge gives way is decided by which one moved. Growing the opposite
/// edge instead would shove a boundary the user is not touching, so a window
/// that has hit its floor simply stops following the pointer.
pub fn clamp_to_minimum(zone: Rect, now: Rect, min_w: i32, min_h: i32) -> Rect {
    let (left, right) = if now.width() >= min_w {
        (now.left, now.right)
    } else if now.left != zone.left {
        // Dragging the left edge inwards: stop it short of the right edge.
        (now.right - min_w, now.right)
    } else {
        (now.left, now.left + min_w)
    };
    let (top, bottom) = if now.height() >= min_h {
        (now.top, now.bottom)
    } else if now.top != zone.top {
        (now.bottom - min_h, now.bottom)
    } else {
        (now.top, now.top + min_h)
    };
    Rect::new(left, top, right, bottom)
}

/// What a `WM_NCHITTEST` result says the user grabbed.
///
/// Inferring move-versus-resize from how the rectangle changed is a guess, and
/// it is wrong exactly when it matters: dragging a left or top edge moves the
/// window's origin as well as its size, so a frame in which the size barely
/// changed reads as a move and the drop overlay appears mid-resize.
///
/// The window itself already knows — it answered a hit test before Windows
/// began the drag. Asking it once at the start is both cheaper than guessing
/// every frame and correct, and it matches how the gesture is described:
/// grabbing the title bar moves, grabbing an edge resizes.
pub fn grab_kind(hit: u32) -> Option<DragKind> {
    // Values from WinUser.h. Named locally rather than imported so this stays
    // pure and testable without a desktop.
    const HTCAPTION: u32 = 2;
    const HTGROWBOX: u32 = 4;
    const HTLEFT: u32 = 10;
    const HTBOTTOMRIGHT: u32 = 17;

    match hit {
        HTCAPTION => Some(DragKind::Move),
        // HTLEFT..=HTBOTTOMRIGHT is the contiguous block of the eight resize
        // borders and corners.
        HTGROWBOX => Some(DragKind::Resize),
        h if (HTLEFT..=HTBOTTOMRIGHT).contains(&h) => Some(DragKind::Resize),
        // Client area, menu, buttons, nowhere: not a drag we can attribute.
        // The caller falls back to comparing rectangles.
        _ => None,
    }
}

/// A rectangle `t` of the way from `from` to `to`, edge by edge.
///
/// Used to walk a rejected drag back towards where it started until it stops
/// squeezing a neighbour past its minimum. Refusing the whole movement -- which
/// is what happened before -- turns "you cannot go past here" into "this does
/// not work": the boundary simply stopped following the pointer with no
/// indication that a limit had been reached rather than a bug.
pub fn lerp_rect(from: Rect, to: Rect, t: f32) -> Rect {
    let mix = |a: i32, b: i32| a + ((b - a) as f32 * t).round() as i32;
    Rect::new(
        mix(from.left, to.left),
        mix(from.top, to.top),
        mix(from.right, to.right),
        mix(from.bottom, to.bottom),
    )
}

/// How far along a rejected drag to try, in order, before giving up.
///
/// Coarse on purpose. Each attempt costs a layout computation and a minimum-
/// size lookup per window, this runs inside a 16ms drag tick, and the
/// difference between stopping at 70% and 68% of the way is not visible. Five
/// probes put the boundary within a tenth of its limit, which looks like
/// stopping against a wall.
pub const CLAMP_STEPS: [f32; 5] = [1.0, 0.75, 0.5, 0.3, 0.15];

#[cfg(test)]
mod tests_lerp {
    use super::*;

    const A: Rect = Rect {
        left: 0,
        top: 0,
        right: 100,
        bottom: 100,
    };
    const B: Rect = Rect {
        left: 40,
        top: 20,
        right: 200,
        bottom: 300,
    };

    #[test]
    fn the_ends_are_exact() {
        assert_eq!(lerp_rect(A, B, 0.0), A);
        assert_eq!(lerp_rect(A, B, 1.0), B);
    }

    #[test]
    fn the_midpoint_is_halfway_on_every_edge() {
        let m = lerp_rect(A, B, 0.5);
        assert_eq!((m.left, m.top, m.right, m.bottom), (20, 10, 150, 200));
    }

    #[test]
    fn each_step_moves_strictly_further_than_the_last() {
        // The search relies on this ordering to stop at the first acceptable
        // step and know nothing larger would have fitted.
        let mut previous = 0.0f32;
        for t in CLAMP_STEPS.iter().rev() {
            assert!(*t > previous, "steps must ascend when reversed");
            previous = *t;
        }
        assert_eq!(CLAMP_STEPS[0], 1.0, "the first try must be what was asked");
    }

    #[test]
    fn a_rect_lerped_to_itself_never_moves() {
        for t in CLAMP_STEPS {
            assert_eq!(lerp_rect(A, A, t), A);
        }
    }
}

/// Which edges of a window a `WM_NCHITTEST` result says are being dragged.
///
/// Returned as (left, top, right, bottom).
pub fn grabbed_edges(hit: u32) -> (bool, bool, bool, bool) {
    // WinUser.h. Named locally so this stays pure and testable.
    const HTLEFT: u32 = 10;
    const HTRIGHT: u32 = 11;
    const HTTOP: u32 = 12;
    const HTTOPLEFT: u32 = 13;
    const HTTOPRIGHT: u32 = 14;
    const HTBOTTOM: u32 = 15;
    const HTBOTTOMLEFT: u32 = 16;
    const HTBOTTOMRIGHT: u32 = 17;

    (
        matches!(hit, HTLEFT | HTTOPLEFT | HTBOTTOMLEFT),
        matches!(hit, HTTOP | HTTOPLEFT | HTTOPRIGHT),
        matches!(hit, HTRIGHT | HTTOPRIGHT | HTBOTTOMRIGHT),
        matches!(hit, HTBOTTOM | HTBOTTOMLEFT | HTBOTTOMRIGHT),
    )
}

/// Where the dragged window's edges are now, according to the pointer.
///
/// The window rectangle is the obvious source for this and it is the wrong one.
/// Chromium, Electron and GTK windows do not resize themselves while the user
/// drags a border -- the rectangle is byte-identical on every poll for the
/// whole gesture -- so anything derived from it is derived from a constant, and
/// the boundary never moves. Meanwhile a stale placement leaves such a window
/// sitting hundreds of pixels off its cell, and measuring against the cell
/// reads that offset as an enormous drag on every poll.
///
/// The pointer is the one thing that is certainly moving. `start` anchors the
/// edges the user is not dragging; the grabbed edges follow the cursor.
pub fn edges_from_pointer(start: Rect, grabbed: (bool, bool, bool, bool), x: i32, y: i32) -> Rect {
    let (left, top, right, bottom) = grabbed;
    // Keep at least a pixel of extent: a dragged edge crossing its opposite
    // number would otherwise invert the rectangle.
    Rect::new(
        if left {
            x.min(start.right - 1)
        } else {
            start.left
        },
        if top {
            y.min(start.bottom - 1)
        } else {
            start.top
        },
        if right {
            x.max(start.left + 1)
        } else {
            start.right
        },
        if bottom {
            y.max(start.top + 1)
        } else {
            start.bottom
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{edges_from_pointer, grabbed_edges};

    const START: Rect = Rect {
        left: 100,
        top: 100,
        right: 900,
        bottom: 700,
    };

    /// Run the whole resize pipeline the way the application does, without a
    /// desktop: pointer -> grabbed edges -> boundary edits -> splits -> zones.
    ///
    /// Every resize fault reported today lived in the wiring between these
    /// steps rather than inside any one of them, and every one of them has its
    /// own passing unit tests. This is the test that would have caught them.
    fn drag_once(
        area: Rect,
        count: usize,
        index: usize,
        hit: u32,
        pointer: (i32, i32),
        splits: &mut crate::layout::Splits,
    ) -> Vec<Rect> {
        let params = crate::layout::LayoutParams {
            outer_gap: 0,
            inner_gap: 0,
            master_fraction: 0.55,
            master_count: 1,
        };
        let before = crate::layout::compute_with(area, count, LayoutKind::Grid, &params, splits);
        let zone = before[index];

        let now = edges_from_pointer(zone, grabbed_edges(hit), pointer.0, pointer.1);
        for e in best_edits(
            LayoutKind::Grid,
            index,
            count,
            1,
            area,
            zone,
            now,
            EDGE_THRESHOLD,
        ) {
            match e.grid_row {
                Some(row) => splits.set_grid_column(row, e.index, e.fraction, e.count),
                None => splits.set(e.axis, e.index, e.fraction, e.count),
            }
        }
        crate::layout::compute_with(area, count, LayoutKind::Grid, &params, splits)
    }

    /// How many zones changed size between two layouts.
    fn changed(a: &[Rect], b: &[Rect]) -> usize {
        a.iter()
            .zip(b.iter())
            .filter(|(x, y)| x.width() != y.width() || x.height() != y.height())
            .count()
    }

    #[test]
    fn dragging_one_edge_resizes_only_the_two_cells_that_share_it() {
        // The reported fault: drag a single grid edge and everything moves.
        const AREA: Rect = Rect {
            left: 0,
            top: 0,
            right: 2400,
            bottom: 1200,
        };
        const HTRIGHT: u32 = 11;

        for count in [4usize, 6, 8] {
            let mut splits = crate::layout::Splits::default();
            let params = crate::layout::LayoutParams {
                outer_gap: 0,
                inner_gap: 0,
                master_fraction: 0.55,
                master_count: 1,
            };
            let before =
                crate::layout::compute_with(AREA, count, LayoutKind::Grid, &params, &splits);

            // Grab the right edge of the first cell and pull it 120px right.
            let zone = before[0];
            let after = drag_once(
                AREA,
                count,
                0,
                HTRIGHT,
                (zone.right + 120, zone.top + zone.height() / 2),
                &mut splits,
            );

            let moved = changed(&before, &after);
            assert!(moved > 0, "{count} windows: the drag did nothing at all");
            // A vertical boundary is shared by one column pair, but a grid
            // stacks rows -- so the cells above and below in those two columns
            // move too. What must not happen is every cell moving.
            assert!(
                moved < count,
                "{count} windows: dragging one edge resized all {moved} cells"
            );
        }
    }

    #[test]
    fn a_drag_that_does_not_move_the_pointer_changes_nothing() {
        const AREA: Rect = Rect {
            left: 0,
            top: 0,
            right: 2400,
            bottom: 1200,
        };
        const HTRIGHT: u32 = 11;
        let mut splits = crate::layout::Splits::default();
        let params = crate::layout::LayoutParams {
            outer_gap: 0,
            inner_gap: 0,
            master_fraction: 0.55,
            master_count: 1,
        };
        let before = crate::layout::compute_with(AREA, 6, LayoutKind::Grid, &params, &splits);
        let zone = before[0];
        let after = drag_once(
            AREA,
            6,
            0,
            HTRIGHT,
            (zone.right, zone.top + 10),
            &mut splits,
        );
        assert_eq!(before, after, "a stationary pointer moved the layout");
    }

    #[test]
    fn each_hit_names_the_edges_it_touches() {
        assert_eq!(grabbed_edges(10), (true, false, false, false)); // HTLEFT
        assert_eq!(grabbed_edges(11), (false, false, true, false)); // HTRIGHT
        assert_eq!(grabbed_edges(12), (false, true, false, false)); // HTTOP
        assert_eq!(grabbed_edges(15), (false, false, false, true)); // HTBOTTOM
        assert_eq!(grabbed_edges(13), (true, true, false, false)); // HTTOPLEFT
        assert_eq!(grabbed_edges(17), (false, false, true, true)); // HTBOTTOMRIGHT
    }

    #[test]
    fn a_caption_or_client_hit_grabs_no_edge() {
        for hit in [0u32, 1, 2, 3, 20] {
            assert_eq!(
                grabbed_edges(hit),
                (false, false, false, false),
                "hit {hit}"
            );
        }
    }

    #[test]
    fn only_the_grabbed_edge_follows_the_pointer() {
        let r = edges_from_pointer(START, grabbed_edges(10), 300, 555);
        assert_eq!(r.left, 300, "the dragged edge tracks the cursor");
        assert_eq!(
            (r.top, r.right, r.bottom),
            (100, 900, 700),
            "the rest is anchored"
        );
    }

    #[test]
    fn a_corner_drag_moves_both_of_its_edges() {
        let r = edges_from_pointer(START, grabbed_edges(17), 1200, 950);
        assert_eq!((r.right, r.bottom), (1200, 950));
        assert_eq!((r.left, r.top), (100, 100));
    }

    #[test]
    fn the_rectangle_cannot_be_inverted_by_dragging_past_the_far_edge() {
        // Drag the left edge way past the right one.
        let r = edges_from_pointer(START, grabbed_edges(10), 5000, 400);
        assert!(r.left < r.right, "got {r:?}");
        let r = edges_from_pointer(START, grabbed_edges(15), 400, -5000);
        assert!(r.top < r.bottom, "got {r:?}");
    }

    #[test]
    fn a_pointer_that_has_not_moved_reproduces_the_start_rectangle() {
        // The identity case must be exact, or every poll would register a
        // spurious one-pixel drag.
        let r = edges_from_pointer(START, grabbed_edges(11), START.right, 400);
        assert_eq!(r, START);
    }

    #[test]
    fn an_ungrabbed_window_is_never_altered() {
        for hit in [0u32, 2, 20] {
            assert_eq!(edges_from_pointer(START, grabbed_edges(hit), 12, 34), START);
        }
    }

    use super::grab_kind;

    #[test]
    fn grabbing_the_title_bar_is_a_move() {
        assert_eq!(grab_kind(2), Some(DragKind::Move));
    }

    #[test]
    fn grabbing_any_border_or_corner_is_a_resize() {
        // HTLEFT..=HTBOTTOMRIGHT, plus the size grip.
        for hit in [10u32, 11, 12, 13, 14, 15, 16, 17, 4] {
            assert_eq!(grab_kind(hit), Some(DragKind::Resize), "hit test {hit}");
        }
    }

    #[test]
    fn anything_else_is_left_to_the_caller_to_infer() {
        // HTNOWHERE, HTCLIENT, HTSYSMENU, HTMINBUTTON, HTCLOSE, and a value
        // no documented constant uses.
        for hit in [0u32, 1, 3, 8, 20, 999] {
            assert_eq!(grab_kind(hit), None, "hit test {hit}");
        }
    }

    #[test]
    fn the_two_kinds_never_overlap() {
        let moves: Vec<u32> = (0..40)
            .filter(|h| grab_kind(*h) == Some(DragKind::Move))
            .collect();
        let resizes: Vec<u32> = (0..40)
            .filter(|h| grab_kind(*h) == Some(DragKind::Resize))
            .collect();
        for m in &moves {
            assert!(!resizes.contains(m));
        }
    }

    use super::clamp_to_minimum;

    const CELL: Rect = Rect {
        left: 100,
        top: 100,
        right: 900,
        bottom: 700,
    };

    #[test]
    fn a_rect_above_its_minimum_is_left_alone() {
        let now = Rect::new(200, 150, 800, 650);
        assert_eq!(clamp_to_minimum(CELL, now, 300, 200), now);
    }

    #[test]
    fn dragging_the_left_edge_past_the_minimum_pins_the_left_edge() {
        // The right edge is stationary, so it is the left that must stop.
        let now = Rect::new(850, 100, 900, 700);
        let c = clamp_to_minimum(CELL, now, 400, 200);
        assert_eq!(c.right, 900, "the untouched edge must not move");
        assert_eq!(c.width(), 400);
    }

    #[test]
    fn dragging_the_right_edge_past_the_minimum_pins_the_right_edge() {
        let now = Rect::new(100, 100, 150, 700);
        let c = clamp_to_minimum(CELL, now, 400, 200);
        assert_eq!(c.left, 100, "the untouched edge must not move");
        assert_eq!(c.width(), 400);
    }

    #[test]
    fn both_axes_clamp_independently_on_a_corner_drag() {
        let now = Rect::new(880, 690, 900, 700);
        let c = clamp_to_minimum(CELL, now, 400, 300);
        assert_eq!((c.right, c.bottom), (900, 700));
        assert_eq!((c.width(), c.height()), (400, 300));
    }

    #[test]
    fn a_window_without_a_minimum_is_never_clamped() {
        // min_size returns zeroes when the window does not answer.
        for now in [
            Rect::new(100, 100, 101, 101),
            Rect::new(0, 0, 0, 0),
            Rect::new(500, 500, 900, 700),
        ] {
            assert_eq!(clamp_to_minimum(CELL, now, 0, 0), now);
        }
    }

    #[test]
    fn clamping_is_idempotent() {
        let now = Rect::new(870, 100, 900, 400);
        let once = clamp_to_minimum(CELL, now, 400, 300);
        assert_eq!(clamp_to_minimum(CELL, once, 400, 300), once);
    }

    use super::*;
    use crate::layout::{compute, LayoutParams, Splits};

    const AREA: Rect = Rect::new(0, 0, 1920, 1080);
    const NO_GAP: LayoutParams = LayoutParams {
        outer_gap: 0,
        inner_gap: 0,
        master_fraction: 0.55,
        master_count: 1,
    };

    fn zones(kind: LayoutKind, n: usize) -> Vec<Rect> {
        compute(AREA, n, kind, &NO_GAP)
    }

    // --- classification -------------------------------------------------

    #[test]
    fn a_pure_translation_is_a_move() {
        let a = Rect::new(0, 0, 800, 600);
        let b = Rect::new(500, 300, 1300, 900);
        assert_eq!(classify(a, b, EDGE_THRESHOLD), DragKind::Move);
    }

    #[test]
    fn a_size_change_is_a_resize() {
        let a = Rect::new(0, 0, 800, 600);
        assert_eq!(
            classify(a, Rect::new(0, 0, 1000, 600), EDGE_THRESHOLD),
            DragKind::Resize
        );
        assert_eq!(
            classify(a, Rect::new(0, 0, 800, 900), EDGE_THRESHOLD),
            DragKind::Resize
        );
    }

    #[test]
    fn a_sub_threshold_wobble_is_not_a_resize() {
        let a = Rect::new(0, 0, 800, 600);
        assert_eq!(
            classify(a, Rect::new(0, 0, 803, 602), EDGE_THRESHOLD),
            DragKind::Move
        );
    }

    #[test]
    fn moved_edges_reports_only_what_actually_moved() {
        let zone = Rect::new(0, 0, 960, 1080);
        assert_eq!(moved_edges(zone, zone, EDGE_THRESHOLD), vec![]);
        assert_eq!(
            moved_edges(zone, Rect::new(0, 0, 1200, 1080), EDGE_THRESHOLD),
            vec![Edge::Right]
        );
        let corner = moved_edges(zone, Rect::new(100, 100, 960, 1080), EDGE_THRESHOLD);
        assert_eq!(corner, vec![Edge::Left, Edge::Top]);
    }

    // --- columns ----------------------------------------------------------

    #[test]
    fn dragging_a_column_right_edge_moves_that_boundary() {
        let z = zones(LayoutKind::Columns, 3);
        // Pull zone 0's right edge from 640 to 500.
        let now = Rect::new(0, 0, 500, 1080);
        let e = resize_to_boundary(LayoutKind::Columns, 0, 3, 1, AREA, now, Edge::Right).unwrap();
        assert_eq!(e.axis, SplitAxis::Main);
        assert_eq!(e.index, 0);
        assert_eq!(e.count, 3);
        assert!((e.fraction - 500.0 / 1920.0).abs() < 1e-6);
        let _ = z;
    }

    #[test]
    fn dragging_a_column_left_edge_moves_the_previous_boundary() {
        let now = Rect::new(500, 0, 1280, 1080);
        let e = resize_to_boundary(LayoutKind::Columns, 1, 3, 1, AREA, now, Edge::Left).unwrap();
        assert_eq!(e.index, 0, "zone 1's left edge is boundary 0");
    }

    #[test]
    fn the_outer_edges_of_the_work_area_have_no_boundary() {
        let n = 3;
        assert!(resize_to_boundary(LayoutKind::Columns, 0, n, 1, AREA, AREA, Edge::Left).is_none());
        assert!(
            resize_to_boundary(LayoutKind::Columns, 2, n, 1, AREA, AREA, Edge::Right).is_none()
        );
        assert!(resize_to_boundary(LayoutKind::Rows, 0, n, 1, AREA, AREA, Edge::Top).is_none());
        assert!(resize_to_boundary(LayoutKind::Rows, 2, n, 1, AREA, AREA, Edge::Bottom).is_none());
    }

    #[test]
    fn a_column_drag_is_perpendicular_only() {
        // Dragging the top of a column does nothing: columns are full height.
        assert!(resize_to_boundary(LayoutKind::Columns, 1, 3, 1, AREA, AREA, Edge::Top).is_none());
    }

    // --- the round trip that matters -------------------------------------

    #[test]
    fn applying_a_drag_actually_moves_the_edge_where_it_was_dropped() {
        // The property the whole feature rests on: drop an edge at x, and the
        // recomputed layout puts that edge at x.
        for (kind, edge, index) in [
            (LayoutKind::Columns, Edge::Right, 0usize),
            (LayoutKind::Columns, Edge::Left, 1),
            (LayoutKind::Rows, Edge::Bottom, 0),
            (LayoutKind::MasterStack, Edge::Right, 0),
        ] {
            let n = 3;
            let before = compute(AREA, n, kind, &NO_GAP);
            let zone = before[index];
            // Drag the chosen edge 200px inward.
            let now = match edge {
                Edge::Right => Rect::new(zone.left, zone.top, zone.right - 200, zone.bottom),
                Edge::Left => Rect::new(zone.left - 200, zone.top, zone.right, zone.bottom),
                Edge::Bottom => Rect::new(zone.left, zone.top, zone.right, zone.bottom - 200),
                Edge::Top => Rect::new(zone.left, zone.top - 200, zone.right, zone.bottom),
            };
            let edit = best_edit(kind, index, n, 1, AREA, zone, now, EDGE_THRESHOLD)
                .unwrap_or_else(|| panic!("{kind:?} {edge:?} produced no edit"));

            let mut sp = Splits::default();
            sp.set(edit.axis, edit.index, edit.fraction, edit.count);
            let after = crate::layout::compute_with(AREA, n, kind, &NO_GAP, &sp);

            let got = match edge {
                Edge::Right => after[index].right,
                Edge::Left => after[index].left,
                Edge::Bottom => after[index].bottom,
                Edge::Top => after[index].top,
            };
            let want = match edge {
                Edge::Right => now.right,
                Edge::Left => now.left,
                Edge::Bottom => now.bottom,
                Edge::Top => now.top,
            };
            assert!(
                (got - want).abs() <= 1,
                "{kind:?} {edge:?}: dropped at {want}, landed at {got}"
            );
        }
    }

    #[test]
    fn a_resize_takes_space_from_the_neighbour_not_from_nowhere() {
        let n = 3;
        let before = compute(AREA, n, LayoutKind::Columns, &NO_GAP);
        let zone = before[0];
        let now = Rect::new(0, 0, zone.right + 200, 1080);
        let edit = best_edit(
            LayoutKind::Columns,
            0,
            n,
            1,
            AREA,
            zone,
            now,
            EDGE_THRESHOLD,
        )
        .unwrap();
        let mut sp = Splits::default();
        sp.set(edit.axis, edit.index, edit.fraction, edit.count);
        let after = crate::layout::compute_with(AREA, n, LayoutKind::Columns, &NO_GAP, &sp);

        assert!(
            after[0].width() > before[0].width(),
            "dragged zone should grow"
        );
        assert!(
            after[1].width() < before[1].width(),
            "neighbour should give up the space"
        );
        assert_eq!(after[2], before[2], "the far zone should not move");
        let total: i64 = after.iter().map(|r| r.area()).sum();
        assert_eq!(total, AREA.area(), "the area must still tile exactly");
    }

    // --- corners ----------------------------------------------------------

    #[test]
    fn a_corner_drag_follows_the_edge_that_moved_furthest() {
        let z = zones(LayoutKind::Grid, 4);
        let zone = z[0]; // top-left cell
                         // Right edge moves 300, bottom edge moves 50.
        let now = Rect::new(zone.left, zone.top, zone.right + 300, zone.bottom + 50);
        let edit = best_edit(LayoutKind::Grid, 0, 4, 1, AREA, zone, now, EDGE_THRESHOLD).unwrap();
        assert_eq!(edit.axis, SplitAxis::Main, "the horizontal drag dominated");
    }

    // --- grid -------------------------------------------------------------

    #[test]
    fn a_grid_column_edit_names_its_row() {
        // 6 zones => 3 columns, 2 rows. Zone 4 is the middle of the bottom row.
        let e = resize_to_boundary(LayoutKind::Grid, 4, 6, 1, AREA, AREA, Edge::Left).unwrap();
        assert_eq!(e.grid_row, Some(1), "column edits must be scoped to a row");
        assert_eq!(e.axis, SplitAxis::Main);
        // A row edit spans the full width, so it is not row-scoped.
        let e = resize_to_boundary(LayoutKind::Grid, 4, 6, 1, AREA, AREA, Edge::Top).unwrap();
        assert_eq!(e.grid_row, None);
        assert_eq!(e.axis, SplitAxis::Cross);
    }

    #[test]
    fn only_grid_produces_row_scoped_edits() {
        for kind in [
            LayoutKind::Columns,
            LayoutKind::Rows,
            LayoutKind::MasterStack,
        ] {
            for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
                if let Some(e) = resize_to_boundary(kind, 1, 4, 1, AREA, AREA, edge) {
                    assert_eq!(e.grid_row, None, "{kind:?} {edge:?}");
                }
            }
        }
    }

    #[test]
    fn dragging_one_grid_row_leaves_the_others_alone() {
        // End-to-end: the reported bug, as a test.
        let n = 6;
        let before = compute(AREA, n, LayoutKind::Grid, &NO_GAP);
        let zone = before[3]; // bottom-left cell
        let now = Rect::new(zone.left, zone.top, zone.right - 200, zone.bottom);
        let edit = best_edit(LayoutKind::Grid, 3, n, 1, AREA, zone, now, EDGE_THRESHOLD).unwrap();

        let mut sp = Splits::default();
        match edit.grid_row {
            Some(r) => sp.set_grid_column(r, edit.index, edit.fraction, edit.count),
            None => sp.set(edit.axis, edit.index, edit.fraction, edit.count),
        }
        let after = crate::layout::compute_with(AREA, n, LayoutKind::Grid, &NO_GAP, &sp);

        assert_eq!(
            after[3].right, now.right,
            "the dragged edge should land where dropped"
        );
        assert_eq!(after[0], before[0], "the top row must not move");
        assert_eq!(after[1], before[1]);
        assert_eq!(after[2], before[2]);
    }

    #[test]
    fn a_short_grid_row_sizes_its_boundaries_to_its_own_cell_count() {
        // 5 zones => 3 columns; the bottom row holds only 2. Zone 3 is its
        // left cell, so its right edge is that row's single divider.
        let e = resize_to_boundary(LayoutKind::Grid, 3, 5, 1, AREA, AREA, Edge::Right).unwrap();
        assert_eq!(e.grid_row, Some(1));
        assert_eq!(e.count, 2, "the short row has 2 cells, not 3");
        assert_eq!(e.index, 0);
        // Zone 4 is the last cell of that row: its right edge is the wall.
        assert!(resize_to_boundary(LayoutKind::Grid, 4, 5, 1, AREA, AREA, Edge::Right).is_none());
    }

    #[test]
    fn dragging_a_short_bottom_row_lands_where_dropped() {
        // End-to-end for the reported case: 5 windows, drag the bottom row.
        let n = 5;
        let before = compute(AREA, n, LayoutKind::Grid, &NO_GAP);
        let zone = before[3];
        let now = Rect::new(zone.left, zone.top, zone.right - 300, zone.bottom);
        let edit = best_edit(LayoutKind::Grid, 3, n, 1, AREA, zone, now, EDGE_THRESHOLD).unwrap();

        let mut sp = Splits::default();
        match edit.grid_row {
            Some(r) => sp.set_grid_column(r, edit.index, edit.fraction, edit.count),
            None => sp.set(edit.axis, edit.index, edit.fraction, edit.count),
        }
        let after = crate::layout::compute_with(AREA, n, LayoutKind::Grid, &NO_GAP, &sp);
        assert_eq!(
            after[3].right, now.right,
            "the edge must stay where it was dropped"
        );
        assert_eq!(after[0], before[0], "the top row must not move");
    }

    #[test]
    fn grid_maps_edges_to_the_right_row_and_column() {
        // 4 zones => 2x2. Zone 3 is bottom-right.
        let e = resize_to_boundary(LayoutKind::Grid, 3, 4, 1, AREA, AREA, Edge::Left).unwrap();
        assert_eq!((e.axis, e.index, e.count), (SplitAxis::Main, 0, 2));
        let e = resize_to_boundary(LayoutKind::Grid, 3, 4, 1, AREA, AREA, Edge::Top).unwrap();
        assert_eq!((e.axis, e.index, e.count), (SplitAxis::Cross, 0, 2));
        // Its outer edges have no boundary.
        assert!(resize_to_boundary(LayoutKind::Grid, 3, 4, 1, AREA, AREA, Edge::Right).is_none());
        assert!(resize_to_boundary(LayoutKind::Grid, 3, 4, 1, AREA, AREA, Edge::Bottom).is_none());
    }

    // --- master + stack ----------------------------------------------------

    #[test]
    fn the_master_edge_is_one_boundary_however_many_windows_there_are() {
        for n in 2..8 {
            let e = resize_to_boundary(LayoutKind::MasterStack, 0, n, 1, AREA, AREA, Edge::Right)
                .unwrap();
            assert_eq!(e.count, 2, "master/stack is a single split");
            assert_eq!(e.index, 0);
            assert_eq!(e.axis, SplitAxis::Main);
        }
    }

    #[test]
    fn a_stack_windows_left_edge_also_drives_the_master_split() {
        let e =
            resize_to_boundary(LayoutKind::MasterStack, 1, 4, 1, AREA, AREA, Edge::Left).unwrap();
        assert_eq!((e.axis, e.index, e.count), (SplitAxis::Main, 0, 2));
    }

    #[test]
    fn stack_items_resize_against_each_other_vertically() {
        // 4 windows, 1 master => stack of 3. Zone 2 is the middle stack item.
        let e =
            resize_to_boundary(LayoutKind::MasterStack, 2, 4, 1, AREA, AREA, Edge::Bottom).unwrap();
        assert_eq!((e.axis, e.index, e.count), (SplitAxis::Cross, 1, 3));
        let e =
            resize_to_boundary(LayoutKind::MasterStack, 2, 4, 1, AREA, AREA, Edge::Top).unwrap();
        assert_eq!((e.axis, e.index, e.count), (SplitAxis::Cross, 0, 3));
    }

    #[test]
    fn all_master_degenerates_to_rows() {
        let e =
            resize_to_boundary(LayoutKind::MasterStack, 0, 3, 5, AREA, AREA, Edge::Bottom).unwrap();
        assert_eq!((e.axis, e.index, e.count), (SplitAxis::Main, 0, 3));
    }

    // --- layouts without adjustable splits ---------------------------------

    #[test]
    fn monocle_and_dwindle_are_not_drag_resizable() {
        for kind in [LayoutKind::Monocle, LayoutKind::Dwindle, LayoutKind::Bsp] {
            for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
                assert!(
                    resize_to_boundary(kind, 1, 4, 1, AREA, AREA, edge).is_none(),
                    "{kind:?} {edge:?}"
                );
            }
        }
    }

    #[test]
    fn degenerate_input_is_rejected_rather_than_panicking() {
        assert!(
            resize_to_boundary(LayoutKind::Columns, 0, 1, 1, AREA, AREA, Edge::Right).is_none()
        );
        assert!(
            resize_to_boundary(LayoutKind::Columns, 9, 3, 1, AREA, AREA, Edge::Right).is_none()
        );
        let empty = Rect::new(0, 0, 0, 0);
        assert!(
            resize_to_boundary(LayoutKind::Columns, 0, 3, 1, empty, empty, Edge::Right).is_none()
        );
    }

    // --- drag to swap -------------------------------------------------------

    // --- corner resize ----------------------------------------------------

    #[test]
    fn a_corner_drag_resizes_both_axes() {
        // The reported bug: pulling a corner only moved one axis.
        let z = zones(LayoutKind::Grid, 4);
        let zone = z[0]; // top-left of a 2x2
        let now = Rect::new(zone.left, zone.top, zone.right + 300, zone.bottom + 200);
        let edits = best_edits(LayoutKind::Grid, 0, 4, 1, AREA, zone, now, EDGE_THRESHOLD);
        assert_eq!(
            edits.len(),
            2,
            "a corner touches one boundary per axis: {edits:?}"
        );
        assert!(edits.iter().any(|e| e.axis == SplitAxis::Main));
        assert!(edits.iter().any(|e| e.axis == SplitAxis::Cross));
    }

    #[test]
    fn a_corner_drag_lands_both_edges_where_dropped() {
        let n = 4;
        let before = compute(AREA, n, LayoutKind::Grid, &NO_GAP);
        let zone = before[0];
        let now = Rect::new(zone.left, zone.top, zone.right - 260, zone.bottom - 140);
        let edits = best_edits(LayoutKind::Grid, 0, n, 1, AREA, zone, now, EDGE_THRESHOLD);

        let mut sp = Splits::default();
        for e in &edits {
            match e.grid_row {
                Some(r) => sp.set_grid_column(r, e.index, e.fraction, e.count),
                None => sp.set(e.axis, e.index, e.fraction, e.count),
            }
        }
        let after = crate::layout::compute_with(AREA, n, LayoutKind::Grid, &NO_GAP, &sp);
        assert_eq!(after[0].right, now.right, "horizontal edge");
        assert_eq!(after[0].bottom, now.bottom, "vertical edge");
    }

    #[test]
    fn a_single_axis_drag_still_produces_one_edit() {
        let z = zones(LayoutKind::Columns, 3);
        let zone = z[0];
        let now = Rect::new(zone.left, zone.top, zone.right + 100, zone.bottom);
        let edits = best_edits(
            LayoutKind::Columns,
            0,
            3,
            1,
            AREA,
            zone,
            now,
            EDGE_THRESHOLD,
        );
        assert_eq!(edits.len(), 1);
    }

    #[test]
    fn two_edges_on_the_same_axis_collapse_to_the_furthest() {
        // Both left and right moved: still one horizontal boundary can win.
        let z = zones(LayoutKind::Columns, 3);
        let zone = z[1];
        let now = Rect::new(zone.left - 40, zone.top, zone.right + 200, zone.bottom);
        let edits = best_edits(
            LayoutKind::Columns,
            1,
            3,
            1,
            AREA,
            zone,
            now,
            EDGE_THRESHOLD,
        );
        assert_eq!(edits.len(), 1, "one edit per axis");
        assert_eq!(edits[0].index, 1, "the right edge moved furthest");
    }

    // --- drop actions -----------------------------------------------------

    #[test]
    fn the_centre_of_a_window_swaps() {
        let z = zones(LayoutKind::Columns, 2);
        let t = z[1];
        let d = drop_action(&z, t.center_x(), t.center_y()).unwrap();
        assert_eq!(d.target, 1);
        assert_eq!(d.action, DropAction::Swap);
        assert_eq!(d.side, Side::Centre);
        assert_eq!(d.highlight, t, "a swap highlights the whole zone");
        assert_eq!(d.label(), "Swap");
    }

    #[test]
    fn the_right_band_makes_a_new_column_to_the_right() {
        let z = zones(LayoutKind::Columns, 2);
        let t = z[0];
        // 90% across, vertically centred.
        let x = t.left + (t.width() * 90) / 100;
        let d = drop_action(&z, x, t.center_y()).unwrap();
        assert_eq!(d.action, DropAction::InsertAfter);
        assert_eq!(d.side, Side::Horizontal);
        assert_eq!(d.label(), "Place right");
        // The highlight is the band, not the whole zone.
        assert!(d.highlight.width() < t.width() / 2);
        assert_eq!(d.highlight.right, t.right);
        assert_eq!(d.highlight.height(), t.height());
    }

    #[test]
    fn the_bottom_band_makes_a_new_row_below() {
        let z = zones(LayoutKind::Columns, 2);
        let t = z[0];
        let y = t.top + (t.height() * 90) / 100;
        let d = drop_action(&z, t.center_x(), y).unwrap();
        assert_eq!(d.action, DropAction::InsertAfter);
        assert_eq!(d.side, Side::Vertical);
        assert_eq!(d.label(), "Place below");
        assert_eq!(d.highlight.bottom, t.bottom);
        assert!(d.highlight.height() < t.height() / 2);
    }

    #[test]
    fn the_left_and_top_bands_insert_before() {
        let z = zones(LayoutKind::Columns, 2);
        let t = z[1];
        let left = drop_action(&z, t.left + 2, t.center_y()).unwrap();
        assert_eq!(left.action, DropAction::InsertBefore);
        assert_eq!(left.label(), "Place left");
        let top = drop_action(&z, t.center_x(), t.top + 2).unwrap();
        assert_eq!(top.action, DropAction::InsertBefore);
        assert_eq!(top.label(), "Place above");
    }

    #[test]
    fn the_corners_of_a_zone_swap_rather_than_flickering() {
        // A corner is in both an edge band and a cross band; without the
        // cross-check the action would alternate as the pointer moved a pixel.
        let z = zones(LayoutKind::Columns, 2);
        let t = z[0];
        for (x, y) in [
            (t.left + 2, t.top + 2),
            (t.right - 2, t.top + 2),
            (t.left + 2, t.bottom - 2),
            (t.right - 2, t.bottom - 2),
        ] {
            let d = drop_action(&z, x, y).unwrap();
            assert_eq!(d.action, DropAction::Swap, "corner ({x},{y}) should swap");
        }
    }

    #[test]
    fn the_bands_are_twenty_percent_leaving_a_sixty_percent_centre() {
        let z = vec![Rect::new(0, 0, 1000, 1000)];
        // 19% across is inside the left band; 21% is not.
        assert_eq!(
            drop_action(&z, 190, 500).unwrap().action,
            DropAction::InsertBefore
        );
        assert_eq!(drop_action(&z, 210, 500).unwrap().action, DropAction::Swap);
        assert_eq!(
            drop_action(&z, 810, 500).unwrap().action,
            DropAction::InsertAfter
        );
        assert_eq!(drop_action(&z, 790, 500).unwrap().action, DropAction::Swap);
    }

    #[test]
    fn a_drop_on_no_zones_resolves_to_nothing() {
        assert_eq!(drop_action(&[], 0, 0), None);
    }

    #[test]
    fn a_degenerate_zone_is_refused() {
        let z = vec![Rect::new(10, 10, 10, 10)];
        assert_eq!(drop_action(&z, 10, 10), None);
    }

    #[test]
    fn the_cursor_resolves_to_the_zone_under_it() {
        let z = zones(LayoutKind::Grid, 4);
        assert_eq!(target_zone(&z, 10, 10), Some(0));
        assert_eq!(target_zone(&z, 1900, 10), Some(1));
        assert_eq!(target_zone(&z, 10, 1000), Some(2));
        assert_eq!(target_zone(&z, 1900, 1000), Some(3));
    }

    #[test]
    fn a_cursor_outside_every_zone_still_picks_the_nearest() {
        let z = zones(LayoutKind::Grid, 4);
        assert!(target_zone(&z, -500, -500).is_some());
        assert_eq!(target_zone(&[], 0, 0), None);
    }
}
