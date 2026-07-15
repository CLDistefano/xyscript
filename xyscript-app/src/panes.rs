//! Split-pane layout tree (split-panes-plan Phase 1).
//!
//! A **pure data model** — no GUI, no rendering. The tree describes which document (`DocId`) each
//! visible pane shows and how the panes are arranged; `layout_rects` turns it into on-screen
//! rectangles. The GUI (Phase 2, pragmatic path) renders each pane by pointing the existing renderer
//! at that pane's doc + view; the verbs (Phase 3) call the operations here.
//!
//! v1 invariant: **≤ 1 pane per `DocId`** — the same *buffer* can't appear in two panes at once (two
//! panes would fight over one caret). Two `OPEN/NV` copies of a file are *distinct* `DocId`s, so
//! splitting them is fine. See the plan's "Locked decisions".

/// A stable per-document handle. Indices into `AppHost.docs` shift when tabs close, so the tree
/// references documents by this id instead (assigned once, in `Document::default`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct DocId(pub u32);

/// A stable per-pane handle (assigned from `PaneTree.next_pane`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct PaneId(pub u32);

/// Per-pane viewport state (scroll only — the caret/selection stay on the shared `Document` in v1,
/// one caret per buffer). Mirrored into the GUI's scratch viewport when a pane is rendered.
#[derive(Clone, Copy, Debug)]
pub struct ViewState {
    pub scroll_line: usize,
    pub scroll_vertical: f32,
    pub follow: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        // A fresh pane follows its caret until the user scrolls (matches a new tab).
        ViewState { scroll_line: 0, scroll_vertical: 0.0, follow: true }
    }
}

/// How a `Split` node arranges its two children. Named by ARRANGEMENT (not H/V) — the verb layer maps
/// `NEW/H`/`THS` → `Stacked` (top/bottom) and `NEW/V`/`TVS` → `SideBySide` (left/right).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Split {
    /// Children stacked top (`a`) over bottom (`b`).
    Stacked,
    /// Children side by side, left (`a`) then right (`b`).
    SideBySide,
}

/// Which side of a split the NEW pane goes on when splitting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// New pane becomes child `a` (top / left); the existing pane becomes `b`.
    First,
    /// New pane becomes child `b` (bottom / right); the existing pane stays `a`.
    Second,
}

/// A rectangle in some coordinate space (the GUI passes pixels or points; the tree is unit-agnostic).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One laid-out pane: its id, the document it shows, and its on-screen rectangle.
#[derive(Clone, Copy, Debug)]
pub struct PaneRect {
    pub pane: PaneId,
    pub doc: DocId,
    pub rect: Rect,
}

/// A draggable split divider: the thin bar between two sibling panes, plus the area it divides (so a
/// drag can be turned back into a ratio). `index` is the split's position in a stable pre-order walk;
/// pass it to [`PaneTree::set_divider_ratio`] to resize.
#[derive(Clone, Copy, Debug)]
pub struct Divider {
    pub index: usize,
    /// true = a vertical bar you drag left/right (a `SideBySide` split); false = horizontal (`Stacked`).
    pub side_by_side: bool,
    /// The thin hit/paint rect of the bar itself.
    pub bar: Rect,
    /// The full rect this split divides (drag position ÷ this = the new ratio).
    pub area: Rect,
}

/// A leaf: one pane showing one document, with its own scroll view.
#[derive(Clone, Copy, Debug)]
struct Pane {
    id: PaneId,
    doc: DocId,
    view: ViewState,
}

impl Default for Pane {
    fn default() -> Self {
        Pane { id: PaneId(0), doc: DocId(0), view: ViewState::default() }
    }
}

/// The layout tree: either a single pane, or a split of two subtrees.
enum Node {
    Leaf(Pane),
    Split { dir: Split, ratio: f32, a: Box<Node>, b: Box<Node> },
}

impl Default for Node {
    // A transient placeholder used only by `mem::take` during `close`; replaced immediately.
    fn default() -> Self {
        Node::Leaf(Pane::default())
    }
}

impl Node {
    fn leaves(&self, out: &mut Vec<(PaneId, DocId)>) {
        match self {
            Node::Leaf(p) => out.push((p.id, p.doc)),
            Node::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }

    fn find(&self, id: PaneId) -> Option<&Pane> {
        match self {
            Node::Leaf(p) => (p.id == id).then_some(p),
            Node::Split { a, b, .. } => a.find(id).or_else(|| b.find(id)),
        }
    }

    fn find_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        match self {
            Node::Leaf(p) => (p.id == id).then_some(p),
            Node::Split { a, b, .. } => match a.find_mut(id) {
                Some(p) => Some(p),
                None => b.find_mut(id),
            },
        }
    }

    fn contains_doc(&self, doc: DocId) -> bool {
        match self {
            Node::Leaf(p) => p.doc == doc,
            Node::Split { a, b, .. } => a.contains_doc(doc) || b.contains_doc(doc),
        }
    }

    /// Replace the leaf `target` with a split of `(new)` and `(current)` per `new_on`. Returns whether
    /// the target was found.
    fn do_split(
        &mut self,
        target: PaneId,
        dir: Split,
        new_pane: Pane,
        new_on: Side,
    ) -> bool {
        match self {
            Node::Leaf(p) if p.id == target => {
                let cur = Box::new(Node::Leaf(*p)); // `Pane: Copy`
                let new = Box::new(Node::Leaf(new_pane));
                let (a, b) = match new_on {
                    Side::First => (new, cur),
                    Side::Second => (cur, new),
                };
                *self = Node::Split { dir, ratio: 0.5, a, b };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                a.do_split(target, dir, new_pane, new_on)
                    || b.do_split(target, dir, new_pane, new_on)
            }
        }
    }

    /// Consume this subtree and rebuild it without the pane `id`; the removed leaf's sibling is
    /// hoisted into its place. `None` = the whole subtree was the removed pane.
    fn without(self, id: PaneId) -> Option<Node> {
        match self {
            Node::Leaf(p) if p.id == id => None,
            leaf @ Node::Leaf(_) => Some(leaf),
            Node::Split { dir, ratio, a, b } => match (a.without(id), b.without(id)) {
                (Some(a), Some(b)) => {
                    Some(Node::Split { dir, ratio, a: Box::new(a), b: Box::new(b) })
                }
                (Some(n), None) | (None, Some(n)) => Some(n), // sibling hoisted
                (None, None) => None,                          // unreachable (one target)
            },
        }
    }

    /// Swap the two children of the split that directly contains leaf `id` (`TRV`).
    fn swap_sibling_of(&mut self, id: PaneId) -> bool {
        if let Node::Split { a, b, .. } = self {
            let here = matches!(a.as_ref(), Node::Leaf(p) if p.id == id)
                || matches!(b.as_ref(), Node::Leaf(p) if p.id == id);
            if here {
                std::mem::swap(a, b);
                return true;
            }
            return a.swap_sibling_of(id) || b.swap_sibling_of(id);
        }
        false
    }

    fn layout(&self, area: Rect, out: &mut Vec<PaneRect>) {
        match self {
            Node::Leaf(p) => out.push(PaneRect { pane: p.id, doc: p.doc, rect: area }),
            Node::Split { dir, ratio, a, b } => {
                let (ra, rb) = split_rect(area, *dir, *ratio);
                a.layout(ra, out);
                b.layout(rb, out);
            }
        }
    }

    /// Collect each split's divider bar, pre-order (this split, then child `a`, then `b`) so the index
    /// matches [`set_nth_ratio`](Node::set_nth_ratio).
    fn collect_dividers(&self, area: Rect, idx: &mut usize, out: &mut Vec<Divider>) {
        if let Node::Split { dir, ratio, a, b } = self {
            let my = *idx;
            *idx += 1;
            let (ra, rb) = split_rect(area, *dir, *ratio);
            const HALF: f32 = 3.0; // bar half-thickness
            let bar = match dir {
                Split::SideBySide => Rect { x: ra.x + ra.w - HALF, y: area.y, w: HALF * 2.0, h: area.h },
                Split::Stacked => Rect { x: area.x, y: ra.y + ra.h - HALF, w: area.w, h: HALF * 2.0 },
            };
            out.push(Divider {
                index: my,
                side_by_side: matches!(dir, Split::SideBySide),
                bar,
                area,
            });
            a.collect_dividers(ra, idx, out);
            b.collect_dividers(rb, idx, out);
        }
    }

    /// Set the ratio of the `target`-th split (same pre-order as [`collect_dividers`]).
    fn set_nth_ratio(&mut self, target: usize, idx: &mut usize, ratio: f32) -> bool {
        if let Node::Split { ratio: r, a, b, .. } = self {
            let my = *idx;
            *idx += 1;
            if my == target {
                *r = ratio;
                return true;
            }
            return a.set_nth_ratio(target, idx, ratio) || b.set_nth_ratio(target, idx, ratio);
        }
        false
    }
}

/// Split `area` into the two child rects by `ratio` (child `a`'s share), per direction.
fn split_rect(area: Rect, dir: Split, ratio: f32) -> (Rect, Rect) {
    let r = ratio.clamp(0.05, 0.95);
    match dir {
        Split::Stacked => {
            let ha = (area.h * r).round();
            (
                Rect { h: ha, ..area },
                Rect { y: area.y + ha, h: area.h - ha, ..area },
            )
        }
        Split::SideBySide => {
            let wa = (area.w * r).round();
            (
                Rect { w: wa, ..area },
                Rect { x: area.x + wa, w: area.w - wa, ..area },
            )
        }
    }
}

/// The split-pane layout for the main window. One `Leaf` = the classic single-pane view.
pub struct PaneTree {
    root: Node,
    focus: PaneId,
    /// When set, only this pane is shown full-window (`TZ`); the tree is untouched so toggle restores.
    zoom: Option<PaneId>,
    next_pane: u32,
}

impl PaneTree {
    /// A fresh tree: one pane showing `doc`.
    pub fn new(doc: DocId) -> PaneTree {
        let id = PaneId(1);
        PaneTree {
            root: Node::Leaf(Pane { id, doc, view: ViewState::default() }),
            focus: id,
            zoom: None,
            next_pane: 2,
        }
    }

    /// Number of panes (leaves).
    pub fn pane_count(&self) -> usize {
        let mut v = Vec::new();
        self.root.leaves(&mut v);
        v.len()
    }

    /// The `(pane, doc)` pairs in layout order (left→right, top→bottom).
    pub fn panes(&self) -> Vec<(PaneId, DocId)> {
        let mut v = Vec::new();
        self.root.leaves(&mut v);
        v
    }

    pub fn focus(&self) -> PaneId {
        self.focus
    }

    /// The document shown in the focused pane.
    pub fn focused_doc(&self) -> DocId {
        self.root.find(self.focus).map(|p| p.doc).unwrap_or(DocId(0))
    }

    /// Set focus to `pane` if it exists.
    pub fn set_focus(&mut self, pane: PaneId) {
        if self.root.find(pane).is_some() {
            self.focus = pane;
        }
    }

    /// Whether `doc` is already shown in some pane (the ≤1-pane-per-doc guard).
    pub fn contains_doc(&self, doc: DocId) -> bool {
        self.root.contains_doc(doc)
    }

    /// If `doc` is shown in some pane, move focus to that pane and return `true`; otherwise leave focus
    /// alone and return `false`. Used when the active buffer changes (`BUFNEXT`/`BUFALT`/tab click): if
    /// the newly-active buffer is already visible, the focus (and its border) should jump to it rather
    /// than the guard silently blocking a content swap.
    pub fn focus_doc_if_shown(&mut self, doc: DocId) -> bool {
        if let Some((pid, _)) = self.panes().into_iter().find(|&(_, d)| d == doc) {
            self.focus = pid;
            true
        } else {
            false
        }
    }

    /// The focused pane's view state (for the GUI to read/mirror).
    pub fn focused_view(&self) -> ViewState {
        self.root.find(self.focus).map(|p| p.view).unwrap_or_default()
    }

    /// Read a pane's view state.
    pub fn view(&self, pane: PaneId) -> Option<ViewState> {
        self.root.find(pane).map(|p| p.view)
    }

    /// Write a pane's view state (the GUI writes back scroll after rendering that pane).
    pub fn set_view(&mut self, pane: PaneId, view: ViewState) {
        if let Some(p) = self.root.find_mut(pane) {
            p.view = view;
        }
    }

    /// Point the focused pane at a different document (tab activation swaps content in place — it does
    /// NOT open a new pane). No-op if `doc` is already shown elsewhere (would violate the guard).
    pub fn set_focused_doc(&mut self, doc: DocId) {
        if self.root.find(self.focus).map(|p| p.doc) == Some(doc) {
            return;
        }
        if self.contains_doc(doc) {
            return; // shown in another pane already — don't duplicate
        }
        if let Some(p) = self.root.find_mut(self.focus) {
            p.doc = doc;
            p.view = ViewState::default();
        }
    }

    /// Split the focused pane, giving the new pane `incoming`; the new pane takes focus. Errs if
    /// `incoming` is already shown (≤1 pane per doc).
    pub fn split(&mut self, dir: Split, incoming: DocId, new_on: Side) -> Result<PaneId, &'static str> {
        if self.contains_doc(incoming) {
            return Err("that buffer is already shown in a pane");
        }
        let new_id = PaneId(self.next_pane);
        let new_pane = Pane { id: new_id, doc: incoming, view: ViewState::default() };
        if !self.root.do_split(self.focus, dir, new_pane, new_on) {
            return Err("no focused pane to split");
        }
        self.next_pane += 1;
        self.focus = new_id;
        self.zoom = None; // a new split leaves zoom (a full-window state) behind
        Ok(new_id)
    }

    /// Close (un-split) the focused pane, hoisting its sibling. No-op if it's the only pane. Focus
    /// moves to the first remaining pane.
    pub fn close_focused(&mut self) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }
        let removed = self.focus;
        let root = std::mem::take(&mut self.root);
        self.root = root.without(removed).unwrap_or_default();
        self.focus = self.first_pane();
        if self.zoom == Some(removed) {
            self.zoom = None;
        }
        true
    }

    fn first_pane(&self) -> PaneId {
        self.panes().first().map(|&(p, _)| p).unwrap_or(PaneId(0))
    }

    /// Release the FOCUSED pane to full window (`FS`): discard the whole split structure, leaving the
    /// focused pane as the sole full-window pane. Unlike [`close_focused`](PaneTree::close_focused)
    /// (which removes the focused pane and shows its sibling), this KEEPS the focused pane and drops
    /// the others. The dropped panes' buffers stay open as tabs. Permanent, unlike the [`toggle_zoom`]
    /// (PaneTree::toggle_zoom) preview. No-op (returns `false`) when already a single pane.
    pub fn collapse_to_focused(&mut self) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }
        if let Some(&p) = self.root.find(self.focus) {
            self.root = Node::Leaf(p); // `Pane: Copy`
            self.zoom = None;
            true
        } else {
            false
        }
    }

    /// Move focus to the next (or previous) pane in layout order, wrapping.
    pub fn focus_cycle(&mut self, forward: bool) {
        let panes = self.panes();
        if panes.len() < 2 {
            return;
        }
        let i = panes.iter().position(|&(p, _)| p == self.focus).unwrap_or(0);
        let n = panes.len();
        let j = if forward { (i + 1) % n } else { (i + n - 1) % n };
        self.focus = panes[j].0;
    }

    /// Swap the focused pane with its sibling in the immediately-containing split (`TRV`).
    pub fn swap_focused(&mut self) -> bool {
        self.root.swap_sibling_of(self.focus)
    }

    /// Toggle full-window zoom of the focused pane (`TZ`). The tree is untouched, so it restores.
    pub fn toggle_zoom(&mut self) {
        self.zoom = if self.zoom == Some(self.focus) { None } else { Some(self.focus) };
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoom.is_some()
    }

    /// Lay the tree out within `area`. A zoomed pane short-circuits to a single full-area rect.
    pub fn layout_rects(&self, area: Rect) -> Vec<PaneRect> {
        if let Some(z) = self.zoom {
            if let Some(p) = self.root.find(z) {
                return vec![PaneRect { pane: z, doc: p.doc, rect: area }];
            }
        }
        let mut out = Vec::new();
        self.root.layout(area, &mut out);
        out
    }

    /// The draggable dividers for `area` (empty when zoomed or single-pane). Pair with
    /// [`set_divider_ratio`](PaneTree::set_divider_ratio) using each `Divider::index`.
    pub fn dividers(&self, area: Rect) -> Vec<Divider> {
        if self.zoom.is_some() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut idx = 0;
        self.root.collect_dividers(area, &mut idx, &mut out);
        out
    }

    /// Set the `index`-th split's ratio (child `a`'s share), clamped to a sane range. Returns whether
    /// a split with that index exists.
    pub fn set_divider_ratio(&mut self, index: usize, ratio: f32) -> bool {
        let mut idx = 0;
        self.root.set_nth_ratio(index, &mut idx, ratio.clamp(0.05, 0.95))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 }
    }

    #[test]
    fn fresh_tree_is_one_pane() {
        let t = PaneTree::new(DocId(10));
        assert_eq!(t.pane_count(), 1);
        assert_eq!(t.focused_doc(), DocId(10));
        let rects = t.layout_rects(area());
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].rect, area()); // one pane fills the window
        assert_eq!(rects[0].doc, DocId(10));
    }

    #[test]
    fn split_creates_two_panes_and_focuses_the_new_one() {
        let mut t = PaneTree::new(DocId(1));
        let new = t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        assert_eq!(t.pane_count(), 2);
        assert_eq!(t.focus(), new); // the new pane takes focus
        assert_eq!(t.focused_doc(), DocId(2));
        let rects = t.layout_rects(area());
        assert_eq!(rects.len(), 2);
        // Side-by-side: doc 1 on the left, doc 2 on the right, halves.
        assert_eq!(rects[0].doc, DocId(1));
        assert_eq!(rects[1].doc, DocId(2));
        assert_eq!(rects[0].rect.w, 400.0);
        assert_eq!(rects[1].rect.x, 400.0);
        assert_eq!(rects[0].rect.h, 600.0); // full height (left/right split)
    }

    #[test]
    fn stacked_split_divides_height() {
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::Stacked, DocId(2), Side::Second).unwrap();
        let r = t.layout_rects(area());
        assert_eq!(r[0].rect.h, 300.0);
        assert_eq!(r[1].rect.y, 300.0);
        assert_eq!(r[0].rect.w, 800.0); // full width (top/bottom split)
    }

    #[test]
    fn one_pane_per_doc_guard() {
        let mut t = PaneTree::new(DocId(1));
        assert!(t.split(Split::SideBySide, DocId(1), Side::Second).is_err()); // doc 1 already shown
        assert_eq!(t.pane_count(), 1);
        assert!(t.split(Split::SideBySide, DocId(2), Side::Second).is_ok());
        assert!(t.split(Split::Stacked, DocId(1), Side::Second).is_err()); // still shown
    }

    #[test]
    fn split_then_close_round_trips() {
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap(); // focus = doc 2's pane
        assert_eq!(t.pane_count(), 2);
        assert!(t.close_focused()); // remove the focused (doc 2) pane
        assert_eq!(t.pane_count(), 1);
        assert_eq!(t.focused_doc(), DocId(1)); // back to the original, focused
        let r = t.layout_rects(area());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].rect, area());
        assert!(!t.close_focused()); // can't close the last pane
    }

    #[test]
    fn nested_collapse_keeps_the_sibling_subtree() {
        // (1 | (2 / 3)) — split doc1's pane side-by-side with 2, then split 2's pane stacked with 3.
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap(); // focus doc2
        t.split(Split::Stacked, DocId(3), Side::Second).unwrap(); // focus doc3, nested under doc2
        assert_eq!(t.pane_count(), 3);
        // Close doc3 → doc2's subtree collapses back to a single leaf; doc1 untouched.
        assert!(t.close_focused());
        let docs: Vec<DocId> = t.panes().into_iter().map(|(_, d)| d).collect();
        assert_eq!(docs, vec![DocId(1), DocId(2)]);
    }

    #[test]
    fn focus_cycle_wraps() {
        let mut t = PaneTree::new(DocId(1));
        let p2 = t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        let p1 = t.panes()[0].0;
        assert_eq!(t.focus(), p2);
        t.focus_cycle(true);
        assert_eq!(t.focus(), p1); // wrapped
        t.focus_cycle(true);
        assert_eq!(t.focus(), p2);
        t.focus_cycle(false);
        assert_eq!(t.focus(), p1);
    }

    #[test]
    fn swap_focused_swaps_the_two_panes() {
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        // Order is [doc1, doc2]; swap → [doc2, doc1].
        assert!(t.swap_focused());
        let docs: Vec<DocId> = t.panes().into_iter().map(|(_, d)| d).collect();
        assert_eq!(docs, vec![DocId(2), DocId(1)]);
    }

    #[test]
    fn zoom_shows_only_the_focused_pane_and_toggles_back() {
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap(); // focus doc2
        t.toggle_zoom();
        let r = t.layout_rects(area());
        assert_eq!(r.len(), 1); // only the zoomed (focused) pane
        assert_eq!(r[0].doc, DocId(2));
        assert_eq!(r[0].rect, area());
        assert_eq!(t.pane_count(), 2); // the tree is untouched
        t.toggle_zoom();
        assert_eq!(t.layout_rects(area()).len(), 2); // restored
    }

    #[test]
    fn dividers_report_and_resize_the_right_split() {
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        let ds = t.dividers(area());
        assert_eq!(ds.len(), 1);
        assert!(ds[0].side_by_side);
        // The bar sits at the 50% seam (x ≈ 400 in an 800-wide area).
        assert!((ds[0].bar.x + ds[0].bar.w / 2.0 - 400.0).abs() < 1.0);
        // Resize the split to 25% and re-layout: the left pane shrinks to a quarter.
        assert!(t.set_divider_ratio(0, 0.25));
        let r = t.layout_rects(area());
        assert_eq!(r[0].rect.w, 200.0);
        assert_eq!(r[1].rect.x, 200.0);
        // A zoomed tree exposes no dividers.
        t.toggle_zoom();
        assert!(t.dividers(area()).is_empty());
    }

    #[test]
    fn set_focused_doc_swaps_content_not_pane_count() {
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap(); // focus doc2
        t.set_focused_doc(DocId(9)); // activate a different tab in the focused pane
        assert_eq!(t.pane_count(), 2); // no new pane
        assert_eq!(t.focused_doc(), DocId(9));
        t.set_focused_doc(DocId(1)); // doc1 is shown in the OTHER pane → guard blocks the swap
        assert_eq!(t.focused_doc(), DocId(9));
    }
}
