//! Binary space partition tree — the layout model that can actually split.
//!
//! The parametric layouts (Grid, Columns, …) derive every zone from a window
//! *count*, so they cannot express "split this one cell in half and leave its
//! neighbours alone". Dropping a window on another window's edge re-ordered
//! the existing cells instead of creating one, because there was nowhere to
//! put a boundary that belongs to a subtree.
//!
//! A tree can. Each node is either a leaf holding one window, or a split with
//! an orientation, a ratio and two children. Zones fall out of a recursive
//! walk, every boundary belongs to exactly one split, and inserting means
//! replacing a leaf with a split containing the old occupant and the new one —
//! which is precisely "50% stays, 50% goes to the new window", with everything
//! outside that subtree untouched.
//!
//! Pure geometry and pure data: no Win32 here, so the structural operations
//! are exhaustively testable.

use serde::{Deserialize, Serialize};

use crate::layout::Rect;

/// Which way a split divides its area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    /// Children side by side; the boundary is vertical. Produces columns.
    Horizontal,
    /// Children stacked; the boundary is horizontal. Produces rows.
    Vertical,
}

impl Orientation {
    pub fn flipped(self) -> Orientation {
        match self {
            Orientation::Horizontal => Orientation::Vertical,
            Orientation::Vertical => Orientation::Horizontal,
        }
    }
}

/// Smallest share of a split either child may be squeezed to.
pub const MIN_RATIO: f32 = 0.05;

/// A node in the partition tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Node {
    /// One window.
    Leaf(isize),
    /// Two children divided by a boundary at `ratio` along `orientation`.
    Split {
        orientation: Orientation,
        /// Share of the area given to `first`, 0..1.
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    fn leaf(h: isize) -> Node {
        Node::Leaf(h)
    }

    fn split(orientation: Orientation, ratio: f32, first: Node, second: Node) -> Node {
        Node::Split {
            orientation,
            ratio: ratio.clamp(0.05, 0.95),
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Every window in this subtree, left-to-right / top-to-bottom.
    pub fn leaves(&self, out: &mut Vec<isize>) {
        match self {
            Node::Leaf(h) => out.push(*h),
            Node::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            }
        }
    }

    fn contains(&self, hwnd: isize) -> bool {
        match self {
            Node::Leaf(h) => *h == hwnd,
            Node::Split { first, second, .. } => first.contains(hwnd) || second.contains(hwnd),
        }
    }

    fn count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { first, second, .. } => first.count() + second.count(),
        }
    }
}

/// A window and the rectangle it should occupy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub hwnd: isize,
    pub rect: Rect,
}

/// The partition tree for one monitor.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tree {
    pub root: Option<Node>,
}

impl Tree {
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn len(&self) -> usize {
        self.root.as_ref().map(|n| n.count()).unwrap_or(0)
    }

    pub fn contains(&self, hwnd: isize) -> bool {
        self.root.as_ref().is_some_and(|n| n.contains(hwnd))
    }

    pub fn windows(&self) -> Vec<isize> {
        let mut v = Vec::new();
        if let Some(r) = &self.root {
            r.leaves(&mut v);
        }
        v
    }

    /// Build a tree from an ordered window list, splitting the largest leaf
    /// each time.
    ///
    /// Used to seed the tree from whatever layout was in use, so switching to
    /// it does not scatter the user's windows. Splitting the largest leaf
    /// keeps the result close to a grid rather than degenerating into a
    /// staircase.
    pub fn from_windows(order: &[isize], area: Rect) -> Tree {
        Tree {
            root: balanced(order, area),
        }
    }

    /// Add a window without being told where: split whichever leaf currently
    /// has the most room, across its longer axis.
    pub fn insert_auto(&mut self, hwnd: isize, area: Rect) {
        if self.contains(hwnd) {
            return;
        }
        let Some(root) = self.root.take() else {
            self.root = Some(Node::leaf(hwnd));
            return;
        };
        // Find the biggest leaf by area, then split it.
        let placements = layout_node(&root, area);
        let target = placements
            .iter()
            .max_by_key(|p| p.rect.area())
            .map(|p| (p.hwnd, p.rect));

        self.root = Some(root);
        match target {
            Some((victim, rect)) => {
                let orientation = if rect.width() >= rect.height() {
                    Orientation::Horizontal
                } else {
                    Orientation::Vertical
                };
                self.split_at(victim, hwnd, orientation, false, 0.5);
            }
            None => self.root = Some(Node::leaf(hwnd)),
        }
    }

    /// Split `target`'s cell in two, giving half to `new`.
    ///
    /// This is the operation the parametric layouts could not express. The
    /// leaf holding `target` becomes a split containing `target` and `new`;
    /// nothing outside that subtree moves, so the windows above and below keep
    /// their full span.
    ///
    /// `before` places `new` in the first slot — left of, or above, `target`.
    pub fn split_at(
        &mut self,
        target: isize,
        new: isize,
        orientation: Orientation,
        before: bool,
        ratio: f32,
    ) -> bool {
        if target == new {
            return false;
        }
        // A window being moved must leave its old position first, or it would
        // appear twice.
        self.remove(new);
        let Some(root) = self.root.take() else {
            self.root = Some(Node::leaf(new));
            return false;
        };
        let (node, done) = split_in(root, target, new, orientation, before, ratio);
        self.root = Some(node);
        done
    }

    /// Remove a window; its sibling takes the whole of the parent's area.
    ///
    /// This is how a split is destroyed: closing or moving out the last window
    /// on one side collapses the boundary rather than leaving a hole.
    pub fn remove(&mut self, hwnd: isize) -> bool {
        let Some(root) = self.root.take() else {
            return false;
        };
        match remove_in(root, hwnd) {
            (Some(node), removed) => {
                self.root = Some(node);
                removed
            }
            (None, removed) => {
                self.root = None;
                removed
            }
        }
    }

    /// Exchange two windows' positions, leaving the structure alone.
    pub fn swap(&mut self, a: isize, b: isize) {
        if a == b {
            return;
        }
        if let Some(root) = self.root.as_mut() {
            swap_in(root, a, b);
        }
    }

    /// Drop every window not in `live`, and add any that are missing.
    ///
    /// Returns true if anything changed.
    pub fn reconcile(&mut self, live: &[isize], area: Rect) -> bool {
        let mut changed = false;
        for h in self.windows() {
            if !live.contains(&h) {
                self.remove(h);
                changed = true;
            }
        }
        for h in live {
            if !self.contains(*h) {
                self.insert_auto(*h, area);
                changed = true;
            }
        }
        changed
    }

    /// Rectangles for every window.
    pub fn layout(&self, area: Rect) -> Vec<Placement> {
        match &self.root {
            Some(r) => layout_node(r, area),
            None => Vec::new(),
        }
    }

    /// Move the boundary of the split that separates `hwnd` from its sibling
    /// along `orientation`, to `fraction` of that split's own area.
    ///
    /// Returns false when no enclosing split runs that way — dragging the
    /// outer edge of the work area, for instance.
    /// `want_second` says which side of the boundary the window is on: true
    /// when dragging a left or top edge, false for a right or bottom one.
    ///
    /// Without it the innermost split of the right orientation was taken
    /// whatever side the window sat on, so a window that is the first child of
    /// its split could only be resized from the right, and one that is the
    /// second child only from the left. Every window had exactly one working
    /// edge, decided by where it happened to land in the tree.
    pub fn set_ratio(
        &mut self,
        hwnd: isize,
        orientation: Orientation,
        area: Rect,
        edge_pos: i32,
        want_second: bool,
    ) -> bool {
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        // Where every boundary sits before the change, so the others can be
        // put back afterwards. Without this a ratio-based tree rescales the
        // whole subtree and one drag appears to move everything.
        let mut before = Vec::new();
        boundaries(root, area, &mut before);
        let chosen = index_of_chosen(root, hwnd, orientation, area, want_second, 0);

        if !set_ratio_in(root, hwnd, orientation, area, edge_pos, want_second) {
            return false;
        }
        if let Some(skip) = chosen {
            let mut next = 0usize;
            restore_boundaries(root, area, &before, &mut next, skip);
        }
        true
    }
}

// --- recursion helpers ------------------------------------------------------

fn split_pos(area: Rect, orientation: Orientation, ratio: f32) -> i32 {
    let r = if ratio.is_finite() {
        ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO)
    } else {
        0.5
    };
    match orientation {
        Orientation::Horizontal => {
            let x = area.left + (area.width() as f32 * r).round() as i32;
            x.clamp(area.left + 1, (area.right - 1).max(area.left + 1))
        }
        Orientation::Vertical => {
            let y = area.top + (area.height() as f32 * r).round() as i32;
            y.clamp(area.top + 1, (area.bottom - 1).max(area.top + 1))
        }
    }
}

fn halves(area: Rect, orientation: Orientation, ratio: f32) -> (Rect, Rect) {
    let p = split_pos(area, orientation, ratio);
    match orientation {
        Orientation::Horizontal => (
            Rect::new(area.left, area.top, p, area.bottom),
            Rect::new(p, area.top, area.right, area.bottom),
        ),
        Orientation::Vertical => (
            Rect::new(area.left, area.top, area.right, p),
            Rect::new(area.left, p, area.right, area.bottom),
        ),
    }
}

fn layout_node(node: &Node, area: Rect) -> Vec<Placement> {
    let mut out = Vec::new();
    walk(node, area, &mut out);
    out
}

fn walk(node: &Node, area: Rect, out: &mut Vec<Placement>) {
    match node {
        Node::Leaf(h) => out.push(Placement {
            hwnd: *h,
            rect: area,
        }),
        Node::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let (a, b) = halves(area, *orientation, *ratio);
            walk(first, a, out);
            walk(second, b, out);
        }
    }
}

fn split_in(
    node: Node,
    target: isize,
    new: isize,
    orientation: Orientation,
    before: bool,
    ratio: f32,
) -> (Node, bool) {
    match node {
        Node::Leaf(h) if h == target => {
            let (a, b) = if before {
                (Node::leaf(new), Node::leaf(target))
            } else {
                (Node::leaf(target), Node::leaf(new))
            };
            (Node::split(orientation, ratio, a, b), true)
        }
        Node::Leaf(h) => (Node::Leaf(h), false),
        Node::Split {
            orientation: o,
            ratio,
            first,
            second,
        } => {
            let (f, done) = split_in(*first, target, new, orientation, before, ratio);
            if done {
                return (
                    Node::Split {
                        orientation: o,
                        ratio,
                        first: Box::new(f),
                        second,
                    },
                    true,
                );
            }
            let (s, done) = split_in(*second, target, new, orientation, before, ratio);
            (
                Node::Split {
                    orientation: o,
                    ratio,
                    first: Box::new(f),
                    second: Box::new(s),
                },
                done,
            )
        }
    }
}

fn remove_in(node: Node, hwnd: isize) -> (Option<Node>, bool) {
    match node {
        Node::Leaf(h) if h == hwnd => (None, true),
        Node::Leaf(h) => (Some(Node::Leaf(h)), false),
        Node::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let (f, removed_first) = remove_in(*first, hwnd);
            if removed_first {
                // The sibling absorbs the whole area: the boundary is gone.
                return (
                    f.map_or_else(
                        || Some(*second.clone()),
                        |n| {
                            Some(Node::Split {
                                orientation,
                                ratio,
                                first: Box::new(n),
                                second: second.clone(),
                            })
                        },
                    ),
                    true,
                );
            }
            let (s, removed_second) = remove_in(*second, hwnd);
            let f = f.expect("first child survives when nothing was removed from it");
            if removed_second {
                return (
                    Some(s.map_or(f.clone(), |n| Node::Split {
                        orientation,
                        ratio,
                        first: Box::new(f),
                        second: Box::new(n),
                    })),
                    true,
                );
            }
            let s = s.expect("second child survives when nothing was removed from it");
            (
                Some(Node::Split {
                    orientation,
                    ratio,
                    first: Box::new(f),
                    second: Box::new(s),
                }),
                false,
            )
        }
    }
}

fn swap_in(node: &mut Node, a: isize, b: isize) {
    match node {
        Node::Leaf(h) => {
            if *h == a {
                *h = b;
            } else if *h == b {
                *h = a;
            }
        }
        Node::Split { first, second, .. } => {
            swap_in(first, a, b);
            swap_in(second, a, b);
        }
    }
}

/// Find the nearest enclosing split of the right orientation and move it.
/// How close a window's edge must be to a boundary to count as touching it.
///
/// Integer division when halving an area leaves a pixel of slack at some
/// depths, so an exact comparison would reject boundaries the window really
/// does border.
const ADJACENT_TOLERANCE: i32 = 2;

fn set_ratio_in(
    node: &mut Node,
    hwnd: isize,
    orientation: Orientation,
    area: Rect,
    edge_pos: i32,
    want_second: bool,
) -> bool {
    let Node::Split {
        orientation: o,
        ratio,
        first,
        second,
    } = node
    else {
        return false;
    };
    let (a, b) = halves(area, *o, *ratio);

    // The split the dragged edge belongs to is the innermost one of the right
    // orientation that has the window on the *correct side*.
    //
    // The side matters and used to be ignored. Dragging a window's left edge
    // moves the boundary it shares with whatever is to its left -- the split
    // where this window is the second child. Dragging the right edge moves the
    // one where it is the first. Take the nearest split of the right
    // orientation without checking, and half the windows can only be resized
    // in one direction, which is exactly how it behaved.
    //
    // Deeper splits are still tried first, so the innermost qualifying boundary
    // wins: that is the one under the cursor.
    let in_first = first.contains(hwnd);
    let child_area = if in_first { a } else { b };
    let child: &mut Node = if in_first { first } else { second };
    if set_ratio_in(child, hwnd, orientation, child_area, edge_pos, want_second) {
        return true;
    }

    let on_wanted_side = if want_second {
        second.contains(hwnd)
    } else {
        first.contains(hwnd)
    };

    // The boundary must also be one this window's own cell borders.
    //
    // Being on the correct side of a split is not enough: that split may be an
    // ancestor separating two whole groups, and moving it drags the outer edge
    // of everything in the group rather than the edge the user took hold of.
    // On screen that reads as the neighbour's far side moving too.
    //
    // The window's cell touches the boundary only when its own edge sits on it,
    // so compare the two directly and keep walking outwards if they differ.
    let touches_boundary = *o == orientation && on_wanted_side && {
        let boundary = match orientation {
            Orientation::Horizontal => b.left,
            Orientation::Vertical => b.top,
        };
        let sub_area = if want_second { b } else { a };
        let sub: &Node = if want_second { second } else { first };
        layout_node(sub, sub_area)
            .into_iter()
            .find(|p| p.hwnd == hwnd)
            .is_some_and(|p| {
                let mine = match (orientation, want_second) {
                    (Orientation::Horizontal, true) => p.rect.left,
                    (Orientation::Horizontal, false) => p.rect.right,
                    (Orientation::Vertical, true) => p.rect.top,
                    (Orientation::Vertical, false) => p.rect.bottom,
                };
                (mine - boundary).abs() <= ADJACENT_TOLERANCE
            })
    };
    if touches_boundary {
        let span = match orientation {
            Orientation::Horizontal => area.width(),
            Orientation::Vertical => area.height(),
        };
        if span <= 0 {
            return false;
        }
        let origin = match orientation {
            Orientation::Horizontal => area.left,
            Orientation::Vertical => area.top,
        };
        let f = (edge_pos - origin) as f32 / span as f32;
        *ratio = f.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        return true;
    }
    false
}

/// Build a balanced partition of `order` over `area`.
///
/// Seeding by repeatedly splitting the largest leaf -- which is the right rule
/// for adding *one* window -- produces a staircase when applied to a whole
/// list: the first window keeps half the screen and the last gets a slice of a
/// slice. With eight windows the smallest cell is 1/128th of the area, which is
/// below every real application's minimum size, and the result looked less like
/// a tiling window manager than like a fault.
///
/// Halving the *list* instead gives cells within a factor of two of each other,
/// and the shape is a pure function of the window count -- so a restart that
/// has lost the tree rebuilds something recognisable rather than something new.
/// Orientation follows the cell's longer axis, which is what keeps the result
/// looking like a grid rather than like a column of letterboxes.
fn balanced(order: &[isize], area: Rect) -> Option<Node> {
    match order.len() {
        0 => None,
        1 => Some(Node::leaf(order[0])),
        n => {
            let half = n.div_ceil(2);
            let orientation = if area.width() >= area.height() {
                Orientation::Horizontal
            } else {
                Orientation::Vertical
            };
            // The ratio is the share of windows, not a flat half, so an odd
            // count divides the space in proportion to what each side holds.
            let ratio = half as f32 / n as f32;
            let (first_area, second_area) = split_area(area, orientation, ratio);
            let first = balanced(&order[..half], first_area)?;
            let second = balanced(&order[half..], second_area)?;
            Some(Node::Split {
                orientation,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            })
        }
    }
}

/// The two halves `area` divides into. Mirrors what `layout_node` does, so the
/// orientation decisions taken while seeding match the geometry that results.
fn split_area(area: Rect, orientation: Orientation, ratio: f32) -> (Rect, Rect) {
    match orientation {
        Orientation::Horizontal => {
            let cut = area.left + (area.width() as f32 * ratio).round() as i32;
            (
                Rect::new(area.left, area.top, cut, area.bottom),
                Rect::new(cut, area.top, area.right, area.bottom),
            )
        }
        Orientation::Vertical => {
            let cut = area.top + (area.height() as f32 * ratio).round() as i32;
            (
                Rect::new(area.left, area.top, area.right, cut),
                Rect::new(area.left, cut, area.right, area.bottom),
            )
        }
    }
}

/// A tree as it survives a restart.
///
/// The live tree holds window handles, and Windows does not preserve those
/// across a session -- a saved handle names whatever window happens to inherit
/// the number next time, which is worse than no memory at all. So a saved leaf
/// records *what kind of window* was there (its executable and class, the same
/// identity the geometry memory uses) and the tree is reattached by matching.
///
/// The shape and the ratios are the part actually worth keeping. Which handle
/// occupied a cell is an accident of the session; that a monitor was divided
/// into these proportions is a decision the user made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SavedNode {
    /// A window identity, and the cell it occupied.
    ///
    /// The identity alone is not enough to tell four Chrome windows apart --
    /// they share an executable and a class, so whichever one the pool happened
    /// to yield first took the first Chrome-shaped cell and the rest shuffled.
    /// The layout came back and the windows within it did not.
    ///
    /// Where each one *was* is the tie-breaker. Windows restores an application
    /// roughly where it left it, so the live window nearest a saved cell is
    /// almost always the one that used to be in it.
    Leaf { key: String, rect: Rect },
    Split {
        orientation: Orientation,
        ratio: f32,
        first: Box<SavedNode>,
        second: Box<SavedNode>,
    },
}

impl Tree {
    /// Convert to the saved form, naming each leaf by identity.
    ///
    /// A window whose identity cannot be determined is dropped and its sibling
    /// takes the space, exactly as closing it would do. Saving a leaf that can
    /// never be matched would leave a permanent hole in the restored layout.
    pub fn to_saved(
        &self,
        area: Rect,
        key_of: &dyn Fn(isize) -> Option<String>,
    ) -> Option<SavedNode> {
        // Each leaf's cell, so the restore can tell windows of the same kind
        // apart by where they were.
        let places: std::collections::HashMap<isize, Rect> = self
            .layout(area)
            .into_iter()
            .map(|p| (p.hwnd, p.rect))
            .collect();

        fn walk(
            node: &Node,
            key_of: &dyn Fn(isize) -> Option<String>,
            places: &std::collections::HashMap<isize, Rect>,
        ) -> Option<SavedNode> {
            match node {
                Node::Leaf(h) => key_of(*h).map(|key| SavedNode::Leaf {
                    key,
                    rect: places.get(h).copied().unwrap_or_default(),
                }),
                Node::Split {
                    orientation,
                    ratio,
                    first,
                    second,
                } => match (walk(first, key_of, places), walk(second, key_of, places)) {
                    (Some(a), Some(b)) => Some(SavedNode::Split {
                        orientation: *orientation,
                        ratio: *ratio,
                        first: Box::new(a),
                        second: Box::new(b),
                    }),
                    // One side survived: it inherits the whole area, which is
                    // what `remove` does when a window closes.
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                },
            }
        }
        self.root.as_ref().and_then(|r| walk(r, key_of, &places))
    }

    /// Rebuild from the saved form, claiming a live window for each leaf.
    ///
    /// `claim` is called with an identity and returns an unused window of that
    /// kind, or `None`. Leaves that cannot be filled collapse into their
    /// siblings, so two Chrome windows last time and one now restores the shape
    /// minus that cell rather than leaving a gap.
    ///
    /// Windows that match nothing saved are not this function's business: the
    /// caller inserts them afterwards, which puts them in the largest free
    /// space like any newly-appeared window.
    pub fn from_saved(
        saved: &SavedNode,
        claim: &mut dyn FnMut(&str, Rect) -> Option<isize>,
    ) -> Tree {
        fn walk(
            node: &SavedNode,
            claim: &mut dyn FnMut(&str, Rect) -> Option<isize>,
        ) -> Option<Node> {
            match node {
                SavedNode::Leaf { key, rect } => claim(key, *rect).map(Node::Leaf),
                SavedNode::Split {
                    orientation,
                    ratio,
                    first,
                    second,
                } => match (walk(first, claim), walk(second, claim)) {
                    (Some(a), Some(b)) => Some(Node::Split {
                        orientation: *orientation,
                        ratio: ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO),
                        first: Box::new(a),
                        second: Box::new(b),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                },
            }
        }
        Tree {
            root: walk(saved, claim),
        }
    }
}

/// Every split's boundary position in pixels, in a stable pre-order walk.
///
/// A split stores a *ratio* of its parent's area, so moving any boundary
/// changes the area every boundary inside it is a fraction of, and they all
/// slide. Dragging one edge visibly resizes the entire subtree, which is not
/// what anyone means by moving a boundary.
///
/// Recording where they all are, applying the one change, then putting the
/// others back where they were is what makes a drag local. The walk order is
/// deterministic, so the snapshot and the restore line up index for index.
fn boundaries(node: &Node, area: Rect, out: &mut Vec<i32>) {
    let Node::Split {
        orientation,
        ratio,
        first,
        second,
    } = node
    else {
        return;
    };
    let (a, b) = halves(area, *orientation, *ratio);
    out.push(match orientation {
        Orientation::Horizontal => b.left,
        Orientation::Vertical => b.top,
    });
    boundaries(first, a, out);
    boundaries(second, b, out);
}

/// Put every boundary back at the pixel position it held in `want`, except the
/// one at `skip`, which is the one the user just moved.
fn restore_boundaries(node: &mut Node, area: Rect, want: &[i32], next: &mut usize, skip: usize) {
    let Node::Split {
        orientation,
        ratio,
        first,
        second,
    } = node
    else {
        return;
    };
    let me = *next;
    *next += 1;

    if me != skip {
        let (span, origin) = match orientation {
            Orientation::Horizontal => (area.width(), area.left),
            Orientation::Vertical => (area.height(), area.top),
        };
        if span > 0 {
            if let Some(pos) = want.get(me) {
                let f = (*pos - origin) as f32 / span as f32;
                *ratio = f.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
            }
        }
    }

    let (a, b) = halves(area, *orientation, *ratio);
    restore_boundaries(first, a, want, next, skip);
    restore_boundaries(second, b, want, next, skip);
}

/// How many splits a subtree contains, for pre-order index arithmetic.
fn split_count(node: &Node) -> usize {
    match node {
        Node::Leaf(_) => 0,
        Node::Split { first, second, .. } => 1 + split_count(first) + split_count(second),
    }
}

/// Index of the split `set_ratio_in` will choose, in the same pre-order walk
/// [`boundaries`] uses.
///
/// The arithmetic is the fiddly part and getting it wrong is silent: the
/// restore then pins the wrong boundary and the drag spreads again. A node at
/// index `me` owns `me + 1 ..` for its first subtree and everything after that
/// for its second, so descending into the second child means skipping the
/// whole of the first -- counted, not walked.
fn index_of_chosen(
    node: &Node,
    hwnd: isize,
    orientation: Orientation,
    area: Rect,
    want_second: bool,
    me: usize,
) -> Option<usize> {
    let Node::Split {
        orientation: o,
        ratio,
        first,
        second,
    } = node
    else {
        return None;
    };
    let (a, b) = halves(area, *o, *ratio);
    let in_first = first.contains(hwnd);
    let (child, child_area, child_index) = if in_first {
        (first.as_ref(), a, me + 1)
    } else {
        (second.as_ref(), b, me + 1 + split_count(first))
    };

    // Innermost first, exactly as set_ratio_in does.
    if let Some(found) = index_of_chosen(
        child,
        hwnd,
        orientation,
        child_area,
        want_second,
        child_index,
    ) {
        return Some(found);
    }

    let on_wanted_side = if want_second {
        second.contains(hwnd)
    } else {
        first.contains(hwnd)
    };
    if *o == orientation && on_wanted_side {
        let boundary = match orientation {
            Orientation::Horizontal => b.left,
            Orientation::Vertical => b.top,
        };
        let sub_area = if want_second { b } else { a };
        let sub: &Node = if want_second { second } else { first };
        let touches = layout_node(sub, sub_area)
            .into_iter()
            .find(|p| p.hwnd == hwnd)
            .is_some_and(|p| {
                let mine = match (orientation, want_second) {
                    (Orientation::Horizontal, true) => p.rect.left,
                    (Orientation::Horizontal, false) => p.rect.right,
                    (Orientation::Vertical, true) => p.rect.top,
                    (Orientation::Vertical, false) => p.rect.bottom,
                };
                (mine - boundary).abs() <= ADJACENT_TOLERANCE
            });
        if touches {
            return Some(me);
        }
    }
    None
}

/// Smallest cell worth creating, in pixels along the split axis.
///
/// A window that reports no minimum will still take whatever it is given, and
/// giving it eight pixels is not a split, it is a defect that happens to be
/// expressible.
pub const MIN_CELL_PX: i32 = 48;

/// The share of `extent` the first child should take, honouring both minimums.
///
/// An even split is the intent, not the requirement. When one occupant cannot
/// live in half the cell but both can live in *some* division of it, refusing
/// the split throws away a layout the user asked for and could have had — so
/// the ratio moves as far from even as it must, and no further.
///
/// Returns `None` only when the cell genuinely cannot hold both, which is the
/// one case where "too small to split" is the truth rather than an artefact of
/// insisting on halves.
pub fn fit_ratio(extent: i32, first_min: i32, second_min: i32) -> Option<f32> {
    if extent <= 0 {
        return None;
    }
    let a = first_min.max(MIN_CELL_PX);
    let b = second_min.max(MIN_CELL_PX);
    if a + b > extent {
        return None;
    }
    let total = extent as f32;
    let lo = a as f32 / total;
    let hi = 1.0 - b as f32 / total;
    // `a + b <= extent` guarantees lo <= hi, so the clamp is always satisfiable.
    Some(0.5f32.clamp(lo, hi))
}

#[cfg(test)]
mod tests {
    use super::{fit_ratio, MIN_CELL_PX};

    #[test]
    fn a_roomy_cell_still_splits_evenly() {
        // The even split is the intent; it must survive when nothing opposes it.
        assert_eq!(fit_ratio(2000, 300, 300), Some(0.5));
        assert_eq!(fit_ratio(2000, 0, 0), Some(0.5));
    }

    #[test]
    fn a_demanding_first_occupant_shifts_the_boundary_just_enough() {
        // 1000 wide, the first needs 700: it gets exactly 700 and no more.
        let r = fit_ratio(1000, 700, 200).expect("this fits");
        assert!((r - 0.7).abs() < 1e-6, "got {r}");
        assert!(r * 1000.0 >= 700.0);
    }

    #[test]
    fn a_demanding_second_occupant_shifts_the_other_way() {
        let r = fit_ratio(1000, 200, 700).expect("this fits");
        assert!((r - 0.3).abs() < 1e-6, "got {r}");
        assert!((1.0 - r) * 1000.0 >= 700.0);
    }

    #[test]
    fn both_minimums_are_honoured_across_the_whole_feasible_range() {
        for extent in [400, 900, 1600, 3000] {
            for a in [0, 100, 300, 620, 1010] {
                for b in [0, 100, 300, 620, 1010] {
                    let Some(r) = fit_ratio(extent, a, b) else {
                        continue;
                    };
                    let first = (r * extent as f32).floor() as i32;
                    let second = extent - first;
                    assert!(
                        first >= a.max(MIN_CELL_PX) - 1,
                        "{extent}/{a}/{b}: first {first}"
                    );
                    assert!(
                        second >= b.max(MIN_CELL_PX) - 1,
                        "{extent}/{a}/{b}: second {second}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_split_is_refused_only_when_it_really_cannot_fit() {
        // 620 + 1010 needs 1630; 1600 cannot hold it, 1700 can.
        assert_eq!(fit_ratio(1600, 620, 1010), None);
        assert!(fit_ratio(1700, 620, 1010).is_some());
    }

    #[test]
    fn a_split_that_would_have_been_refused_at_fifty_fifty_now_succeeds() {
        // This is the whole point: half of 1000 is 500, which fails a 700
        // minimum, but 700/300 fits comfortably.
        // Half of 1000 is 500, which fails a 700 minimum outright.
        assert!(fit_ratio(1000, 700, 250).is_some());
    }

    #[test]
    fn a_zero_minimum_still_gets_a_usable_cell() {
        let r = fit_ratio(600, 500, 0).expect("this fits");
        let second = ((1.0 - r) * 600.0).round() as i32;
        assert!(second >= MIN_CELL_PX, "second cell only {second}px");
    }

    #[test]
    fn a_nonsensical_extent_is_refused_rather_than_dividing_by_zero() {
        assert_eq!(fit_ratio(0, 0, 0), None);
        assert_eq!(fit_ratio(-100, 0, 0), None);
    }

    #[test]
    fn the_ratio_is_always_a_usable_fraction() {
        for extent in [100, 500, 4000] {
            for a in [0, 50, 400] {
                for b in [0, 50, 400] {
                    if let Some(r) = fit_ratio(extent, a, b) {
                        assert!(r > 0.0 && r < 1.0, "{extent}/{a}/{b} gave {r}");
                        assert!(r.is_finite());
                    }
                }
            }
        }
    }

    use super::{balanced, split_area};

    const BIG: Rect = Rect {
        left: 0,
        top: 0,
        right: 3840,
        bottom: 2160,
    };

    fn seeded(n: usize) -> Vec<Placement> {
        let order: Vec<isize> = (1..=n as isize).collect();
        Tree::from_windows(&order, BIG).layout(BIG)
    }

    /// Two windows side by side: 1 on the left, 2 on the right.
    fn pair() -> (Tree, Rect) {
        let area = Rect::new(0, 0, 1000, 500);
        (Tree::from_windows(&[1, 2], area), area)
    }

    /// Identity for a handle, as the application's key function would give.
    fn keyed(h: isize) -> Option<String> {
        Some(format!("app{h}.exe|Class"))
    }

    #[test]
    fn a_tree_survives_a_round_trip_through_the_saved_form() {
        let area = Rect::new(0, 0, 2400, 1200);
        let order: Vec<isize> = (1..=6).collect();
        let mut t = Tree::from_windows(&order, area);
        // A dragged boundary, so the saved ratios are not all the default.
        let target = t
            .layout(area)
            .iter()
            .find(|p| p.rect.left > 0)
            .unwrap()
            .hwnd;
        t.set_ratio(target, Orientation::Horizontal, area, 700, true);
        let before = t.layout(area);

        let saved = t.to_saved(area, &keyed).expect("a populated tree saves");
        let mut pool: Vec<isize> = order.clone();
        let mut back = Tree::from_saved(&saved, &mut |key, _| {
            let i = pool
                .iter()
                .position(|h| keyed(*h).as_deref() == Some(key))?;
            Some(pool.remove(i))
        });
        let after = back.layout(area);

        assert_eq!(before.len(), after.len());
        for (a, b) in before.iter().zip(after.iter()) {
            assert_eq!(a.hwnd, b.hwnd, "the order changed");
            assert_eq!(a.rect, b.rect, "window {} moved", a.hwnd);
        }
        let _ = &mut back;
    }

    #[test]
    fn a_missing_window_collapses_its_cell_instead_of_leaving_a_hole() {
        // Six windows last session, four this one: the shape must still tile
        // the whole area rather than restoring gaps where the absent two were.
        let area = Rect::new(0, 0, 2400, 1200);
        let saved = Tree::from_windows(&(1..=6).collect::<Vec<isize>>(), area)
            .to_saved(area, &keyed)
            .unwrap();

        let mut pool: Vec<isize> = vec![1, 2, 4, 6];
        let back = Tree::from_saved(&saved, &mut |key, _| {
            let i = pool
                .iter()
                .position(|h| keyed(*h).as_deref() == Some(key))?;
            Some(pool.remove(i))
        });
        let placed = back.layout(area);
        assert_eq!(placed.len(), 4);
        let total: i64 = placed.iter().map(|p| p.rect.area()).sum();
        assert_eq!(
            total,
            area.area(),
            "the restored layout does not fill the area"
        );
    }

    #[test]
    fn an_unmatched_saved_tree_yields_nothing_rather_than_an_empty_shape() {
        // Nothing recognisable is running: better to seed fresh than to restore
        // a skeleton with no windows in it.
        let area = Rect::new(0, 0, 1000, 800);
        let saved = Tree::from_windows(&[1, 2, 3], area)
            .to_saved(area, &keyed)
            .unwrap();
        let back = Tree::from_saved(&saved, &mut |_, _| None);
        assert!(back.layout(area).is_empty());
    }

    #[test]
    fn windows_of_the_same_kind_go_back_to_their_own_cells() {
        // The reported fault: the shape came back but the windows within it
        // swapped. Identity cannot separate two Chrome windows -- they share an
        // executable and a class -- so whichever the pool yielded first took
        // the first Chrome-shaped cell.
        //
        // Where each one is now decides it. Window 11 sits on the right, so it
        // belongs in the right-hand cell however the pool is ordered.
        let area = Rect::new(0, 0, 1600, 800);
        let left = Rect::new(0, 0, 800, 800);
        let right = Rect::new(800, 0, 1600, 800);
        let saved = SavedNode::Split {
            orientation: Orientation::Horizontal,
            ratio: 0.5,
            first: Box::new(SavedNode::Leaf {
                key: "chrome.exe|C".into(),
                rect: left,
            }),
            second: Box::new(SavedNode::Leaf {
                key: "chrome.exe|C".into(),
                rect: right,
            }),
        };

        // 10 is on the right of the screen, 11 on the left -- the opposite of
        // the order the pool lists them in, so a first-come claim gets it wrong.
        let now = [(10isize, right), (11isize, left)];
        let mut pool: Vec<isize> = now.iter().map(|(h, _)| *h).collect();
        let centre = |r: Rect| ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let back = Tree::from_saved(&saved, &mut |_key, want| {
            let (wx, wy) = centre(want);
            let idx = pool
                .iter()
                .enumerate()
                .min_by_key(|(_, h)| {
                    let (cx, cy) = centre(now.iter().find(|(x, _)| x == *h).unwrap().1);
                    let (dx, dy) = ((cx - wx) as i64, (cy - wy) as i64);
                    dx * dx + dy * dy
                })
                .map(|(i, _)| i)?;
            Some(pool.remove(idx))
        });

        let placed = back.layout(area);
        assert_eq!(placed.len(), 2);
        let at = |x: i32| placed.iter().find(|p| p.rect.left == x).unwrap().hwnd;
        assert_eq!(
            at(800),
            10,
            "the window on the right should stay on the right"
        );
        assert_eq!(at(0), 11, "the window on the left should stay on the left");
    }

    #[test]
    fn dragging_one_boundary_leaves_every_window_that_does_not_touch_it_alone() {
        // The reported fault: drag one edge in the split layout and every
        // window on the monitor resizes, because each boundary is stored as a
        // fraction of an area that just changed.
        //
        // "Only two cells may move" would be wrong: a boundary can be shared by
        // more than two, since one side may be a stack. The property that
        // actually holds is that a window not touching the boundary must not
        // move at all.
        let area = Rect::new(0, 0, 2400, 1200);
        for n in 3..=9usize {
            let order: Vec<isize> = (1..=n as isize).collect();
            let mut t = Tree::from_windows(&order, area);
            let before = t.layout(area);

            let Some(target) = before.iter().find(|p| p.rect.left > area.left) else {
                continue;
            };
            let (h, boundary) = (target.hwnd, target.rect.left);
            assert!(t.set_ratio(h, Orientation::Horizontal, area, boundary - 90, true));
            let after = t.layout(area);

            // Integer halving cannot always reproduce a position exactly, so a
            // pixel or two of drift is not the fault under test.
            const DRIFT: i32 = 2;
            for (a, b) in before.iter().zip(after.iter()) {
                let touches = a.rect.left == boundary || a.rect.right == boundary;
                if touches {
                    continue;
                }
                assert!(
                    (a.rect.left - b.rect.left).abs() <= DRIFT
                        && (a.rect.right - b.rect.right).abs() <= DRIFT
                        && (a.rect.top - b.rect.top).abs() <= DRIFT
                        && (a.rect.bottom - b.rect.bottom).abs() <= DRIFT,
                    "{n} windows: {} does not touch x={boundary} but moved from {:?} to {:?}",
                    a.hwnd,
                    a.rect,
                    b.rect
                );
            }
            let dragged = after.iter().find(|p| p.hwnd == h).unwrap();
            assert!(
                (dragged.rect.left - (boundary - 90)).abs() <= 3,
                "{n} windows: the dragged edge went to {} not {}",
                dragged.rect.left,
                boundary - 90
            );
        }
    }

    #[test]
    fn a_drag_moves_the_boundary_the_window_borders_not_an_ancestor() {
        // 1 | (2 over 3): window 3's left edge borders the outer divider, but
        // window 2's does too -- and 3's *top* is an inner boundary. Dragging
        // 3's left must move the divider it actually touches, leaving the
        // inner horizontal split between 2 and 3 alone.
        let area = Rect::new(0, 0, 1000, 400);
        let mut t = Tree::default();
        t.insert_auto(1, area);
        t.split_at(1, 2, Orientation::Horizontal, false, 0.5);
        t.split_at(2, 3, Orientation::Vertical, false, 0.5);

        let before = t.layout(area);
        let w2_before = before.iter().find(|p| p.hwnd == 2).unwrap().rect;

        assert!(t.set_ratio(3, Orientation::Horizontal, area, 300, true));
        let after = t.layout(area);
        let w2 = after.iter().find(|p| p.hwnd == 2).unwrap().rect;
        let w3 = after.iter().find(|p| p.hwnd == 3).unwrap().rect;

        assert!((w3.left - 300).abs() <= 2, "window 3 left is {}", w3.left);
        // 2 shares the column with 3, so its left moves with the divider --
        // but its height must not have changed: no inner boundary moved.
        assert_eq!(w2.height(), w2_before.height(), "an inner split moved");
    }

    #[test]
    fn the_right_hand_window_can_be_resized_from_its_left_edge() {
        // Window 2 is the second child, so its left edge is the shared
        // boundary.
        let (mut t, area) = pair();
        assert!(t.set_ratio(2, Orientation::Horizontal, area, 300, true));
        let p = t.layout(area);
        let w2 = p.iter().find(|x| x.hwnd == 2).unwrap();
        assert!((w2.rect.left - 300).abs() <= 1, "got {}", w2.rect.left);
    }

    #[test]
    fn the_left_hand_window_can_be_resized_from_its_right_edge() {
        // Window 1 is the first child, so its right edge is the shared one.
        let (mut t, area) = pair();
        assert!(t.set_ratio(1, Orientation::Horizontal, area, 700, false));
        let p = t.layout(area);
        let w1 = p.iter().find(|x| x.hwnd == 1).unwrap();
        assert!((w1.rect.right - 700).abs() <= 1, "got {}", w1.rect.right);
    }

    #[test]
    fn an_outer_edge_has_no_boundary_to_move() {
        // Window 1's left edge and window 2's right edge are the edges of the
        // work area. There is nothing there to drag, and pretending otherwise
        // would move some unrelated boundary.
        let (mut t, area) = pair();
        assert!(!t.set_ratio(1, Orientation::Horizontal, area, 300, true));
        assert!(!t.set_ratio(2, Orientation::Horizontal, area, 700, false));
    }

    #[test]
    fn every_window_in_a_row_can_be_resized_from_both_of_its_inner_edges() {
        // The regression this fixes: each window had exactly one working edge,
        // decided by which side of its split it landed on.
        let area = Rect::new(0, 0, 1200, 500);
        let order: Vec<isize> = (1..=4).collect();
        for h in &order {
            let placed = Tree::from_windows(&order, area).layout(area);
            let me = placed.iter().find(|p| p.hwnd == *h).unwrap().rect;
            let has_left = me.left > area.left;
            let has_right = me.right < area.right;

            if has_left {
                let mut t = Tree::from_windows(&order, area);
                assert!(
                    t.set_ratio(*h, Orientation::Horizontal, area, me.left - 40, true),
                    "window {h} cannot be resized from its left edge"
                );
            }
            if has_right {
                let mut t = Tree::from_windows(&order, area);
                assert!(
                    t.set_ratio(*h, Orientation::Horizontal, area, me.right + 40, false),
                    "window {h} cannot be resized from its right edge"
                );
            }
        }
    }

    #[test]
    fn seeding_places_every_window_exactly_once() {
        for n in 1..=16 {
            let p = seeded(n);
            assert_eq!(p.len(), n);
            let mut seen: Vec<isize> = p.iter().map(|x| x.hwnd).collect();
            seen.sort();
            assert_eq!(seen, (1..=n as isize).collect::<Vec<_>>());
        }
    }

    #[test]
    fn seeded_cells_are_never_degenerate() {
        // The staircase this replaces gave the last window 1/2^(n-1) of the
        // area. Every real application has a minimum size well above that.
        for n in 2..=16 {
            let p = seeded(n);
            let smallest = p.iter().map(|x| x.rect.area()).min().unwrap();
            let largest = p.iter().map(|x| x.rect.area()).max().unwrap();
            assert!(
                largest <= smallest * 3,
                "{n} windows: {smallest} vs {largest} is not balanced"
            );
            let min_side = p
                .iter()
                .map(|x| x.rect.width().min(x.rect.height()))
                .min()
                .unwrap();
            assert!(
                min_side > 200,
                "{n} windows: a cell only {min_side}px across"
            );
        }
    }

    #[test]
    fn seeding_is_deterministic() {
        // A restart that has lost the tree must rebuild the same shape, or the
        // desktop reshuffles every time SuperTile starts.
        let order: Vec<isize> = (1..=9).collect();
        let a = Tree::from_windows(&order, BIG).layout(BIG);
        let b = Tree::from_windows(&order, BIG).layout(BIG);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.hwnd, y.hwnd);
            assert_eq!(x.rect, y.rect);
        }
    }

    #[test]
    fn seeded_cells_tile_the_area_without_gaps_or_overlap() {
        for n in 1..=12 {
            let p = seeded(n);
            let total: i64 = p.iter().map(|x| x.rect.area()).sum();
            assert_eq!(total, BIG.area(), "{n} windows do not fill the area");
            for (i, a) in p.iter().enumerate() {
                for b in &p[i + 1..] {
                    let overlap = (a.rect.right.min(b.rect.right) - a.rect.left.max(b.rect.left))
                        .max(0)
                        * (a.rect.bottom.min(b.rect.bottom) - a.rect.top.max(b.rect.top)).max(0);
                    assert_eq!(overlap, 0, "{n} windows: cells overlap");
                }
            }
        }
    }

    #[test]
    fn a_wide_area_is_cut_vertically_and_a_tall_one_horizontally() {
        let wide = Rect::new(0, 0, 4000, 1000);
        let tall = Rect::new(0, 0, 1000, 4000);
        let two = [1isize, 2];
        let w = Tree::from_windows(&two, wide).layout(wide);
        assert_eq!(
            w[0].rect.height(),
            wide.height(),
            "a wide area splits into columns"
        );
        let t = Tree::from_windows(&two, tall).layout(tall);
        assert_eq!(
            t[0].rect.width(),
            tall.width(),
            "a tall area splits into rows"
        );
    }

    #[test]
    fn splitting_an_area_loses_nothing() {
        for r in [0.25f32, 0.5, 0.75] {
            for o in [Orientation::Horizontal, Orientation::Vertical] {
                let (a, b) = split_area(BIG, o, r);
                assert_eq!(a.area() + b.area(), BIG.area());
            }
        }
    }

    #[test]
    fn seeding_an_empty_list_gives_an_empty_tree() {
        assert_eq!(balanced(&[], BIG), None);
        assert!(Tree::from_windows(&[], BIG).layout(BIG).is_empty());
    }

    use super::*;

    const AREA: Rect = Rect::new(0, 0, 1000, 800);

    fn rects(t: &Tree) -> Vec<(isize, Rect)> {
        t.layout(AREA)
            .into_iter()
            .map(|p| (p.hwnd, p.rect))
            .collect()
    }

    fn total(t: &Tree) -> i64 {
        t.layout(AREA).iter().map(|p| p.rect.area()).sum()
    }

    fn overlaps(t: &Tree) -> bool {
        let v = t.layout(AREA);
        for i in 0..v.len() {
            for j in (i + 1)..v.len() {
                if v[i].rect.intersection_area(&v[j].rect) > 0 {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn an_empty_tree_lays_out_nothing() {
        let t = Tree::default();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert!(t.layout(AREA).is_empty());
    }

    #[test]
    fn one_window_fills_the_area() {
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        assert_eq!(rects(&t), vec![(1, AREA)]);
    }

    // --- the operation the parametric layouts could not do ------------------

    #[test]
    fn splitting_a_cell_gives_half_to_each_and_leaves_neighbours_alone() {
        // Two columns; split the right one horizontally. The left column must
        // not move, and the right column's area must divide exactly in two.
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.split_at(1, 2, Orientation::Horizontal, false, 0.5);
        let before_left = rects(&t)[0].1;

        assert!(t.split_at(2, 3, Orientation::Horizontal, false, 0.5));
        let after = rects(&t);

        assert_eq!(after.len(), 3);
        assert_eq!(
            after[0],
            (1, before_left),
            "the untouched column must not move"
        );
        // Window 2 kept half of its own cell; 3 took the other half.
        let w2 = after.iter().find(|(h, _)| *h == 2).unwrap().1;
        let w3 = after.iter().find(|(h, _)| *h == 3).unwrap().1;
        assert_eq!(w2.width(), 250);
        assert_eq!(w3.width(), 250);
        assert_eq!(w2.right, w3.left, "the two halves must meet exactly");
        assert_eq!(w2.height(), 800, "a horizontal split keeps full height");
    }

    #[test]
    fn a_vertical_split_stacks_and_keeps_the_neighbours_span() {
        // Two columns; split the left one vertically. The right column must
        // still span the full height -- the thing a grid cannot express.
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.split_at(1, 2, Orientation::Horizontal, false, 0.5);
        assert!(t.split_at(1, 3, Orientation::Vertical, false, 0.5));

        let v = rects(&t);
        let w2 = v.iter().find(|(h, _)| *h == 2).unwrap().1;
        assert_eq!(w2.height(), 800, "the right column still spans full height");
        let w1 = v.iter().find(|(h, _)| *h == 1).unwrap().1;
        let w3 = v.iter().find(|(h, _)| *h == 3).unwrap().1;
        assert_eq!(w1.height(), 400);
        assert_eq!(w3.height(), 400);
        assert_eq!(w1.bottom, w3.top);
        assert_eq!(w1.width(), 500);
    }

    #[test]
    fn before_places_the_new_window_first() {
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.split_at(1, 2, Orientation::Horizontal, true, 0.5);
        let v = rects(&t);
        assert_eq!(v[0].0, 2, "new window takes the left half");
        assert_eq!(v[1].0, 1);
        assert!(v[0].1.right <= v[1].1.left);
    }

    #[test]
    fn splitting_an_absent_target_changes_nothing() {
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        assert!(!t.split_at(99, 2, Orientation::Horizontal, false, 0.5));
    }

    #[test]
    fn a_window_moved_by_a_split_does_not_appear_twice() {
        // Dropping an existing window onto another must move it, not clone it.
        let mut t = Tree::from_windows(&[1, 2, 3], AREA);
        assert!(t.split_at(3, 1, Orientation::Vertical, false, 0.5));
        let mut w = t.windows();
        w.sort_unstable();
        assert_eq!(w, vec![1, 2, 3]);
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn moving_a_window_out_of_a_row_collapses_the_cell_it_left() {
        // Left column split in two rows; right column whole. Move the bottom
        // row's window over to split the right column. The vacated cell must
        // disappear, so the window left behind regains the full left column --
        // not sit next to a hole.
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.split_at(1, 2, Orientation::Horizontal, false, 0.5); // 1 | 2
        t.split_at(1, 3, Orientation::Vertical, false, 0.5); // (1 over 3) | 2

        let before = rects(&t);
        assert_eq!(
            before.iter().find(|(h, _)| *h == 1).unwrap().1.height(),
            400
        );

        // Window 3 leaves the left column and splits the right one.
        assert!(t.split_at(2, 3, Orientation::Vertical, false, 0.5));

        let after = rects(&t);
        let w1 = after.iter().find(|(h, _)| *h == 1).unwrap().1;
        assert_eq!(
            w1.height(),
            800,
            "the vacated row must collapse into window 1"
        );
        assert_eq!(w1.width(), 500);
        assert_eq!(t.len(), 3);
        assert_eq!(total(&t), AREA.area());
        assert!(!overlaps(&t));
    }

    // --- destroying a split -------------------------------------------------

    #[test]
    fn removing_a_window_collapses_its_split() {
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.split_at(1, 2, Orientation::Horizontal, false, 0.5);
        assert!(t.remove(2));
        assert_eq!(
            rects(&t),
            vec![(1, AREA)],
            "the sibling takes the whole area"
        );
    }

    #[test]
    fn removing_from_a_nested_split_leaves_the_rest_intact() {
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.split_at(1, 2, Orientation::Horizontal, false, 0.5);
        t.split_at(2, 3, Orientation::Vertical, false, 0.5);
        assert_eq!(t.len(), 3);
        assert!(t.remove(3));
        assert_eq!(t.len(), 2);
        let v = rects(&t);
        assert_eq!(v[0].1.width(), 500);
        assert_eq!(v[1].1.width(), 500);
        assert_eq!(
            v[1].1.height(),
            800,
            "the collapsed side regains full height"
        );
    }

    #[test]
    fn removing_the_last_window_empties_the_tree() {
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        assert!(t.remove(1));
        assert!(t.is_empty());
        assert!(t.layout(AREA).is_empty());
    }

    #[test]
    fn removing_an_absent_window_is_a_no_op() {
        let mut t = Tree::from_windows(&[1, 2], AREA);
        assert!(!t.remove(99));
        assert_eq!(t.len(), 2);
    }

    // --- invariants ---------------------------------------------------------

    #[test]
    fn the_tree_always_tiles_the_area_exactly() {
        for n in 1..=12 {
            let order: Vec<isize> = (1..=n as isize).collect();
            let t = Tree::from_windows(&order, AREA);
            assert_eq!(t.len(), n, "n={n}");
            assert!(!overlaps(&t), "n={n} overlaps");
            assert_eq!(total(&t), AREA.area(), "n={n} does not tile exactly");
        }
    }

    #[test]
    fn splits_keep_tiling_exactly() {
        let mut t = Tree::from_windows(&[1, 2, 3], AREA);
        t.split_at(2, 4, Orientation::Vertical, false, 0.5);
        t.split_at(4, 5, Orientation::Horizontal, true, 0.5);
        assert!(!overlaps(&t));
        assert_eq!(total(&t), AREA.area());
        assert_eq!(t.len(), 5);
    }

    #[test]
    fn no_zone_is_ever_empty() {
        let order: Vec<isize> = (1..=16).collect();
        let t = Tree::from_windows(&order, AREA);
        for p in t.layout(AREA) {
            assert!(p.rect.width() > 0 && p.rect.height() > 0, "{p:?}");
        }
    }

    #[test]
    fn auto_insert_splits_the_largest_cell() {
        // Keeps the shape grid-like instead of degenerating into a staircase.
        let t = Tree::from_windows(&[1, 2, 3, 4], AREA);
        let v = t.layout(AREA);
        let biggest = v.iter().map(|p| p.rect.area()).max().unwrap();
        let smallest = v.iter().map(|p| p.rect.area()).min().unwrap();
        assert!(
            biggest <= smallest * 2,
            "cells should stay comparable: {v:?}"
        );
    }

    #[test]
    fn inserting_the_same_window_twice_is_ignored() {
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.insert_auto(1, AREA);
        assert_eq!(t.len(), 1);
    }

    // --- swap and reconcile -------------------------------------------------

    #[test]
    fn swap_exchanges_positions_without_changing_the_structure() {
        let mut t = Tree::from_windows(&[1, 2, 3], AREA);
        let before: Vec<Rect> = t.layout(AREA).iter().map(|p| p.rect).collect();
        t.swap(1, 3);
        let after = t.layout(AREA);
        let shapes: Vec<Rect> = after.iter().map(|p| p.rect).collect();
        assert_eq!(shapes, before, "the geometry is unchanged");
        assert_eq!(after[0].hwnd, 3);
        assert_eq!(after[2].hwnd, 1);
    }

    #[test]
    fn reconcile_adds_and_removes_to_match_the_live_set() {
        let mut t = Tree::from_windows(&[1, 2, 3], AREA);
        assert!(t.reconcile(&[2, 3, 4], AREA));
        let mut w = t.windows();
        w.sort_unstable();
        assert_eq!(w, vec![2, 3, 4]);
        assert_eq!(total(&t), AREA.area());
        // A second pass has nothing to do.
        assert!(!t.reconcile(&[2, 3, 4], AREA));
    }

    #[test]
    fn reconcile_to_nothing_empties_the_tree() {
        let mut t = Tree::from_windows(&[1, 2], AREA);
        assert!(t.reconcile(&[], AREA));
        assert!(t.is_empty());
    }

    // --- resize -------------------------------------------------------------

    #[test]
    fn set_ratio_moves_the_boundary_the_edge_belongs_to() {
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.split_at(1, 2, Orientation::Horizontal, false, 0.5);
        assert!(t.set_ratio(1, Orientation::Horizontal, AREA, 300, false));
        let v = rects(&t);
        assert_eq!(v[0].1.width(), 300);
        assert_eq!(v[1].1.width(), 700);
        assert_eq!(total(&t), AREA.area());
    }

    #[test]
    fn set_ratio_finds_the_innermost_matching_split() {
        // Right column split vertically; dragging window 2's bottom edge must
        // move the inner boundary, not the outer column divider.
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.split_at(1, 2, Orientation::Horizontal, false, 0.5);
        t.split_at(2, 3, Orientation::Vertical, false, 0.5);
        assert!(t.set_ratio(2, Orientation::Vertical, AREA, 600, false));

        let v = rects(&t);
        let w1 = v.iter().find(|(h, _)| *h == 1).unwrap().1;
        assert_eq!(w1.width(), 500, "the column divider must not have moved");
        let w2 = v.iter().find(|(h, _)| *h == 2).unwrap().1;
        assert_eq!(w2.bottom, 600);
    }

    #[test]
    fn set_ratio_refuses_when_no_split_runs_that_way() {
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.split_at(1, 2, Orientation::Horizontal, false, 0.5);
        // There is no vertical split anywhere.
        assert!(!t.set_ratio(1, Orientation::Vertical, AREA, 400, false));
    }

    #[test]
    fn a_boundary_cannot_collapse_a_child() {
        let mut t = Tree::default();
        t.insert_auto(1, AREA);
        t.split_at(1, 2, Orientation::Horizontal, false, 0.5);
        t.set_ratio(1, Orientation::Horizontal, AREA, -5000, false);
        for p in t.layout(AREA) {
            assert!(p.rect.width() > 0, "{p:?}");
        }
        t.set_ratio(1, Orientation::Horizontal, AREA, 99999, false);
        for p in t.layout(AREA) {
            assert!(p.rect.width() > 0, "{p:?}");
        }
        assert_eq!(total(&t), AREA.area());
    }

    #[test]
    fn a_non_finite_ratio_falls_back_to_half() {
        let t = Tree {
            root: Some(Node::Split {
                orientation: Orientation::Horizontal,
                ratio: f32::NAN,
                first: Box::new(Node::Leaf(1)),
                second: Box::new(Node::Leaf(2)),
            }),
        };
        let v = rects(&t);
        assert_eq!(v[0].1.width(), 500);
        assert_eq!(total(&t), AREA.area());
    }

    #[test]
    fn a_tree_round_trips_through_json() {
        let mut t = Tree::from_windows(&[1, 2, 3], AREA);
        t.set_ratio(1, Orientation::Horizontal, AREA, 250, false);
        let text = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Tree>(&text).unwrap(), t);
    }

    #[test]
    fn orientation_flips() {
        assert_eq!(Orientation::Horizontal.flipped(), Orientation::Vertical);
        assert_eq!(Orientation::Vertical.flipped(), Orientation::Horizontal);
    }
}
