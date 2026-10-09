//! Split-pane layout tree (split-panes-plan Phase 1; **tab-group model** since 0.1.8).
//!
//! A **pure data model** — no GUI, no rendering. The tree describes the panes' arrangement; each
//! pane now owns an ordered **tab group** (a list of `DocId`s plus which one is active), not a single
//! document. `layout_rects` turns the tree into on-screen rectangles, each showing its pane's ACTIVE
//! tab. The GUI renders each pane by pointing the existing renderer at that pane's active doc + view;
//! the verbs call the operations here.
//!
//! **Partition invariant:** every open document lives in exactly ONE pane's group — the groups
//! partition the set of open docs. A single pane therefore holds *all* the tabs (exactly the classic
//! single-strip behaviour); splitting moves a tab into its own group. The host keeps the invariant by
//! calling [`PaneTree::reconcile`] after it adds or removes documents — a self-healing pass that drops
//! closed docs, collapses emptied panes, and files any newly-opened doc into the focused group.
//!
//! The ≤1-pane-per-`DocId` rule (a buffer can't be in two groups at once — two panes would fight over
//! its one caret) falls straight out of the partition. Two `OPEN/NV` copies are distinct `DocId`s.

/// A stable per-document handle. Indices into `AppHost.docs` shift when tabs close, so the tree
/// references documents by this id instead (assigned once, in `Document::default`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct DocId(pub u32);

/// A stable per-pane handle (assigned from `PaneTree.next_pane`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct PaneId(pub u32);

/// Per-pane viewport state (scroll only — the caret/selection stay on the shared `Document`, one
/// caret per buffer). Mirrored into the GUI's scratch viewport when a pane is rendered.
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

/// One laid-out pane: its id, the document it shows (its ACTIVE tab), and its on-screen rectangle.
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

/// A leaf: one pane owning an ordered TAB GROUP (its `tabs`, with `active` the shown one) and a scroll
/// view. `tabs` is non-empty except transiently inside a tree mutation (reconcile/prune restore it).
#[derive(Clone, Debug)]
struct Pane {
    id: PaneId,
    tabs: Vec<DocId>,
    active: usize,
    view: ViewState,
}

impl Default for Pane {
    fn default() -> Self {
        // A transient placeholder (empty group) used only by `mem::take`/`unwrap_or_default` during a
        // tree mutation; a following prune/reconcile replaces or fills it.
        Pane { id: PaneId(0), tabs: Vec::new(), active: 0, view: ViewState::default() }
    }
}

impl Pane {
    fn single(id: PaneId, doc: DocId) -> Pane {
        Pane { id, tabs: vec![doc], active: 0, view: ViewState::default() }
    }

    /// The pane's visible document (its active tab). `DocId(0)` only for an empty transient group.
    fn active_doc(&self) -> DocId {
        self.tabs.get(self.active).copied().unwrap_or_default()
    }

    fn contains(&self, doc: DocId) -> bool {
        self.tabs.contains(&doc)
    }

    /// Remove `doc` from the group, keeping `active` pointed at the same tab where possible (and
    /// otherwise clamped). Returns whether anything was removed.
    fn remove(&mut self, doc: DocId) -> bool {
        let Some(i) = self.tabs.iter().position(|d| *d == doc) else { return false };
        self.tabs.remove(i);
        if self.active > i {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        true
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
    /// `(pane, active-doc)` per leaf — what the GUI lays out and renders (one visible doc per pane).
    fn leaves(&self, out: &mut Vec<(PaneId, DocId)>) {
        match self {
            Node::Leaf(p) => out.push((p.id, p.active_doc())),
            Node::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }

    /// Every `(pane, doc)` pair across all tab groups — for collapse/merge and diagnostics.
    fn all_tabs(&self, out: &mut Vec<(PaneId, DocId)>) {
        match self {
            Node::Leaf(p) => {
                for &d in &p.tabs {
                    out.push((p.id, d));
                }
            }
            Node::Split { a, b, .. } => {
                a.all_tabs(out);
                b.all_tabs(out);
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

    /// The id of the pane whose group holds `doc` (groups partition the docs, so at most one).
    fn pane_of_doc(&self, doc: DocId) -> Option<PaneId> {
        match self {
            Node::Leaf(p) => p.contains(doc).then_some(p.id),
            Node::Split { a, b, .. } => a.pane_of_doc(doc).or_else(|| b.pane_of_doc(doc)),
        }
    }

    fn contains_doc(&self, doc: DocId) -> bool {
        match self {
            Node::Leaf(p) => p.contains(doc),
            Node::Split { a, b, .. } => a.contains_doc(doc) || b.contains_doc(doc),
        }
    }

    fn collect_docs(&self, out: &mut std::collections::HashSet<DocId>) {
        match self {
            Node::Leaf(p) => {
                out.extend(p.tabs.iter().copied());
            }
            Node::Split { a, b, .. } => {
                a.collect_docs(out);
                b.collect_docs(out);
            }
        }
    }

    /// Drop from every group any doc not in `keep` (closed buffers), keeping each group's active tab
    /// pointed at the same doc where it survives.
    fn retain_docs(&mut self, keep: &[DocId]) {
        match self {
            Node::Leaf(p) => {
                let active_doc = p.tabs.get(p.active).copied();
                p.tabs.retain(|d| keep.contains(d));
                p.active = active_doc
                    .and_then(|ad| p.tabs.iter().position(|d| *d == ad))
                    .unwrap_or_else(|| p.active.min(p.tabs.len().saturating_sub(1)));
            }
            Node::Split { a, b, .. } => {
                a.retain_docs(keep);
                b.retain_docs(keep);
            }
        }
    }

    /// Remove `doc` from every group EXCEPT the pane `keep` (used after a split moves a doc into its
    /// new pane — it must leave its old group so the partition holds).
    fn remove_doc_except(&mut self, doc: DocId, keep: PaneId) {
        match self {
            Node::Leaf(p) => {
                if p.id != keep {
                    p.remove(doc);
                }
            }
            Node::Split { a, b, .. } => {
                a.remove_doc_except(doc, keep);
                b.remove_doc_except(doc, keep);
            }
        }
    }

    /// Consume this subtree and rebuild it dropping any leaf whose group is empty; the removed leaf's
    /// sibling is hoisted into its place. `None` = the whole subtree was empty.
    fn prune_empty(self) -> Option<Node> {
        match self {
            Node::Leaf(p) => (!p.tabs.is_empty()).then_some(Node::Leaf(p)),
            Node::Split { dir, ratio, a, b } => match (a.prune_empty(), b.prune_empty()) {
                (Some(a), Some(b)) => {
                    Some(Node::Split { dir, ratio, a: Box::new(a), b: Box::new(b) })
                }
                (Some(n), None) | (None, Some(n)) => Some(n), // sibling hoisted
                (None, None) => None,
            },
        }
    }

    fn clamp_actives(&mut self) {
        match self {
            Node::Leaf(p) => {
                if p.active >= p.tabs.len() {
                    p.active = p.tabs.len().saturating_sub(1);
                }
            }
            Node::Split { a, b, .. } => {
                a.clamp_actives();
                b.clamp_actives();
            }
        }
    }

    /// Replace the leaf `target` with a split of `(new)` and `(current)` per `new_on`. Returns whether
    /// the target was found.
    fn do_split(&mut self, target: PaneId, dir: Split, new_pane: Pane, new_on: Side) -> bool {
        match self {
            Node::Leaf(p) if p.id == target => {
                let cur = Box::new(Node::Leaf(p.clone()));
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
                a.do_split(target, dir, new_pane.clone(), new_on)
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
            Node::Leaf(p) => out.push(PaneRect { pane: p.id, doc: p.active_doc(), rect: area }),
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

    fn max_pane_id(&self) -> u32 {
        match self {
            Node::Leaf(p) => p.id.0,
            Node::Split { a, b, .. } => a.max_pane_id().max(b.max_pane_id()),
        }
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

/// The split-pane layout for the main window. One `Leaf` holding every tab = the classic single-pane
/// single-strip view.
pub struct PaneTree {
    root: Node,
    focus: PaneId,
    /// When set, only this pane is shown full-window (`TZ`); the tree is untouched so toggle restores.
    zoom: Option<PaneId>,
    next_pane: u32,
}

impl PaneTree {
    /// A fresh tree: one pane whose group is just `doc`.
    pub fn new(doc: DocId) -> PaneTree {
        let id = PaneId(1);
        PaneTree { root: Node::Leaf(Pane::single(id, doc)), focus: id, zoom: None, next_pane: 2 }
    }

    /// Number of panes (leaves).
    pub fn pane_count(&self) -> usize {
        let mut v = Vec::new();
        self.root.leaves(&mut v);
        v.len()
    }

    /// The `(pane, active-doc)` pairs in layout order (left→right, top→bottom).
    pub fn panes(&self) -> Vec<(PaneId, DocId)> {
        let mut v = Vec::new();
        self.root.leaves(&mut v);
        v
    }

    pub fn focus(&self) -> PaneId {
        self.focus
    }

    /// The document shown in the focused pane (its active tab).
    pub fn focused_doc(&self) -> DocId {
        self.root.find(self.focus).map(|p| p.active_doc()).unwrap_or_default()
    }

    /// The focused pane's tab group, in strip order.
    pub fn focused_tab_docs(&self) -> Vec<DocId> {
        self.root.find(self.focus).map(|p| p.tabs.clone()).unwrap_or_default()
    }

    /// The doc that `BUFNEXT`/`BUFPREV` would move to: the next/previous tab in the FOCUSED group,
    /// wrapping. `None` when the group has fewer than two tabs (nothing to cycle to).
    pub fn focused_cycle_target(&self, forward: bool) -> Option<DocId> {
        let p = self.root.find(self.focus)?;
        let n = p.tabs.len();
        if n < 2 {
            return None;
        }
        let i = p.active.min(n - 1);
        let j = if forward { (i + 1) % n } else { (i + n - 1) % n };
        Some(p.tabs[j])
    }

    /// Set focus to `pane` if it exists.
    pub fn set_focus(&mut self, pane: PaneId) {
        if self.root.find(pane).is_some() {
            self.focus = pane;
        }
    }

    /// Whether `doc` is a tab in some pane's group.
    pub fn contains_doc(&self, doc: DocId) -> bool {
        self.root.contains_doc(doc)
    }

    /// If `doc` is a tab in some pane, move focus to that pane, make `doc` that pane's active tab, and
    /// return `true`; otherwise leave the tree alone and return `false`. This is how the active buffer
    /// (tab click / `BUFNEXT` / `BUFALT` / `BUFGO`) drives the tree: the focus border and the pane's
    /// shown tab both follow the active doc into whichever pane owns it.
    pub fn focus_doc_if_shown(&mut self, doc: DocId) -> bool {
        let Some(pid) = self.root.pane_of_doc(doc) else { return false };
        self.focus = pid;
        if let Some(p) = self.root.find_mut(pid) {
            if let Some(i) = p.tabs.iter().position(|d| *d == doc) {
                p.active = i;
            }
        }
        true
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

    /// Reconcile the tree with the host's authoritative open-doc set — the self-healer that keeps the
    /// partition invariant without a precise tree edit at every `docs` push/remove. It (1) drops any
    /// tab whose doc is no longer open, (2) collapses panes left empty, (3) files any newly-opened doc
    /// (present in `all` but in no group) into the focused group, in `all` order, and (4) moves focus
    /// to the pane that owns `active` and makes it that pane's active tab. `all` must be non-empty and
    /// contain `active`.
    pub fn reconcile(&mut self, all: &[DocId], active: DocId) {
        // 1. Drop closed docs from every group.
        self.root.retain_docs(all);
        // 2. Collapse any pane whose group emptied.
        let root = std::mem::take(&mut self.root);
        match root.prune_empty() {
            Some(n) => self.root = n,
            None => {
                // Every group emptied — start over with one pane holding everything.
                let id = PaneId(self.next_pane);
                self.next_pane += 1;
                self.root = Node::Leaf(Pane { id, tabs: Vec::new(), active: 0, view: ViewState::default() });
                self.focus = id;
            }
        }
        if self.root.find(self.focus).is_none() {
            self.focus = self.first_pane();
        }
        // 3. File newly-opened docs into the focused group (in `all` order).
        let mut placed = std::collections::HashSet::new();
        self.root.collect_docs(&mut placed);
        let missing: Vec<DocId> = all.iter().copied().filter(|d| !placed.contains(d)).collect();
        if !missing.is_empty() {
            if let Some(p) = self.root.find_mut(self.focus) {
                p.tabs.extend(missing);
            }
        }
        self.root.clamp_actives();
        // 4. Focus the pane owning `active` and show it there.
        self.focus_doc_if_shown(active);
        if let Some(z) = self.zoom {
            if self.root.find(z).is_none() {
                self.zoom = None;
            }
        }
        self.bump_next_pane();
    }

    fn bump_next_pane(&mut self) {
        self.next_pane = self.next_pane.max(self.root.max_pane_id() + 1);
    }

    /// Split the focused pane, giving the new pane a group of just `incoming`; the new pane takes
    /// focus. `incoming` is MOVED out of whatever group currently holds it (the partition stays clean);
    /// any pane emptied by that move collapses. Errs only on an impossible tree state.
    pub fn split(&mut self, dir: Split, incoming: DocId, new_on: Side) -> Result<PaneId, &'static str> {
        let new_id = PaneId(self.next_pane);
        let new_pane = Pane::single(new_id, incoming);
        if !self.root.do_split(self.focus, dir, new_pane, new_on) {
            return Err("no focused pane to split");
        }
        self.next_pane += 1;
        // `incoming` now lives only in the new pane — strip it from every other group.
        self.root.remove_doc_except(incoming, new_id);
        let root = std::mem::take(&mut self.root);
        self.root = root
            .prune_empty()
            .unwrap_or_else(|| Node::Leaf(Pane::single(new_id, incoming)));
        self.root.clamp_actives();
        if self.root.find(new_id).is_some() {
            self.focus = new_id;
        } else {
            self.focus = self.first_pane();
        }
        self.zoom = None; // a new split leaves zoom (a full-window state) behind
        self.bump_next_pane();
        Ok(self.focus)
    }

    /// Close (un-split) the focused pane, hoisting its sibling. The focused pane's tabs are MERGED into
    /// the pane that receives focus (the documents stay open — only the pane goes away). No-op if it's
    /// the only pane. Focus moves to the first remaining pane.
    pub fn close_focused(&mut self) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }
        let removed = self.focus;
        let salvage = self.root.find(removed).map(|p| p.tabs.clone()).unwrap_or_default();
        let root = std::mem::take(&mut self.root);
        self.root = root.without(removed).unwrap_or_default();
        self.focus = self.first_pane();
        if let Some(p) = self.root.find_mut(self.focus) {
            for d in salvage {
                if !p.tabs.contains(&d) {
                    p.tabs.push(d);
                }
            }
        }
        if self.zoom == Some(removed) {
            self.zoom = None;
        }
        self.bump_next_pane();
        true
    }

    fn first_pane(&self) -> PaneId {
        self.panes().first().map(|&(p, _)| p).unwrap_or(PaneId(0))
    }

    /// Release the FOCUSED pane to full window (`FS`): discard the whole split structure, leaving the
    /// focused pane as the sole pane. The OTHER panes' tabs are merged into the focused group (their
    /// documents stay open), appended after the focused pane's own tabs so its active tab is unchanged.
    /// Permanent, unlike the [`toggle_zoom`](PaneTree::toggle_zoom) preview. No-op when already single.
    pub fn collapse_to_focused(&mut self) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }
        let Some(mut keep) = self.root.find(self.focus).cloned() else { return false };
        let mut every = Vec::new();
        self.root.all_tabs(&mut every);
        for (pid, d) in every {
            if pid != self.focus && !keep.tabs.contains(&d) {
                keep.tabs.push(d);
            }
        }
        self.root = Node::Leaf(keep);
        self.zoom = None;
        true
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
                return vec![PaneRect { pane: z, doc: p.active_doc(), rect: area }];
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

    /// Serialize the layout as whitespace-separated prefix tokens, for `$PANES` and the session
    /// structure signature. Each node is a leaf `L <k> <active> <doc0> … <dock-1>` (k tabs, `active`
    /// the shown index) or a split `S <h|v> <ratio> <a> <b>` (`h`=Stacked, `v`=SideBySide). Structure +
    /// ratios + each group's tabs; focus/zoom/scroll handled separately by the caller.
    pub fn serialize(&self) -> String {
        fn go(n: &Node, out: &mut String) {
            match n {
                Node::Leaf(p) => {
                    out.push_str(&format!("L {} {} ", p.tabs.len(), p.active));
                    for d in &p.tabs {
                        out.push_str(&format!("{} ", d.0));
                    }
                }
                Node::Split { dir, ratio, a, b } => {
                    let d = match dir {
                        Split::Stacked => 'h',
                        Split::SideBySide => 'v',
                    };
                    out.push_str(&format!("S {d} {ratio:.4} "));
                    go(a, out);
                    go(b, out);
                }
            }
        }
        let mut s = String::new();
        go(&self.root, &mut s);
        s.trim_end().to_string()
    }

    /// Like [`serialize`](Self::serialize) but each leaf also carries its [`ViewState`], using the `Lv`
    /// token: `Lv <k> <active> <scroll_line> <scroll_vertical> <follow> <doc0> … <dock-1>`. So `SESSION`
    /// restore brings back each pane's exact scroll offset. Splits are unchanged. Parsed by
    /// [`from_serialized`](Self::from_serialized), which also accepts the view-less `L …` form.
    pub fn serialize_session(&self) -> String {
        fn go(n: &Node, out: &mut String) {
            match n {
                Node::Leaf(p) => {
                    out.push_str(&format!(
                        "Lv {} {} {} {:.4} {} ",
                        p.tabs.len(),
                        p.active,
                        p.view.scroll_line,
                        p.view.scroll_vertical,
                        if p.view.follow { 1 } else { 0 },
                    ));
                    for d in &p.tabs {
                        out.push_str(&format!("{} ", d.0));
                    }
                }
                Node::Split { dir, ratio, a, b } => {
                    let d = match dir {
                        Split::Stacked => 'h',
                        Split::SideBySide => 'v',
                    };
                    out.push_str(&format!("S {d} {ratio:.4} "));
                    go(a, out);
                    go(b, out);
                }
            }
        }
        let mut s = String::new();
        go(&self.root, &mut s);
        s.trim_end().to_string()
    }

    /// Rebuild a tree from [`serialize`](Self::serialize) or [`serialize_session`](Self::serialize_session)
    /// tokens, mapping each saved `DocId` to a live one via `remap`. A tab whose doc `remap` rejects
    /// (not restored — e.g. a transient listing) is DROPPED from its group; a group left empty collapses;
    /// `None` only if the tokens don't parse or nothing survives. The caller then [`reconcile`](Self::reconcile)s
    /// to file any still-open docs the layout didn't mention. Fresh `PaneId`s are assigned; focus lands
    /// on the first leaf (the caller repoints it).
    pub fn from_serialized<F: Fn(DocId) -> Option<DocId>>(s: &str, remap: &F) -> Option<PaneTree> {
        fn parse<'a, I, F>(toks: &mut std::iter::Peekable<I>, next_pane: &mut u32, remap: &F) -> Option<Node>
        where
            I: Iterator<Item = &'a str>,
            F: Fn(DocId) -> Option<DocId>,
        {
            match toks.next()? {
                tok @ ("L" | "Lv") => {
                    let k: usize = toks.next()?.parse().ok()?;
                    let active: usize = toks.next()?.parse().ok()?;
                    let view = if tok == "Lv" {
                        let scroll_line: usize = toks.next()?.parse().ok()?;
                        let scroll_vertical: f32 = toks.next()?.parse().ok()?;
                        let follow = toks.next()? != "0";
                        ViewState { scroll_line, scroll_vertical, follow }
                    } else {
                        ViewState::default()
                    };
                    // Read k doc ids; drop any the remap rejects, tracking where `active` lands.
                    let mut tabs = Vec::with_capacity(k);
                    let mut new_active = 0usize;
                    for j in 0..k {
                        let old: u32 = toks.next()?.parse().ok()?;
                        if let Some(nd) = remap(DocId(old)) {
                            if j <= active {
                                new_active = tabs.len();
                            }
                            tabs.push(nd);
                        }
                    }
                    if new_active >= tabs.len() {
                        new_active = tabs.len().saturating_sub(1);
                    }
                    let id = PaneId(*next_pane);
                    *next_pane += 1;
                    Some(Node::Leaf(Pane { id, tabs, active: new_active, view }))
                }
                "S" => {
                    let dir = match toks.next()? {
                        "h" => Split::Stacked,
                        "v" => Split::SideBySide,
                        _ => return None,
                    };
                    let ratio: f32 = toks.next()?.parse().ok()?;
                    let a = Box::new(parse(toks, next_pane, remap)?);
                    let b = Box::new(parse(toks, next_pane, remap)?);
                    Some(Node::Split { dir, ratio: ratio.clamp(0.05, 0.95), a, b })
                }
                _ => None,
            }
        }
        let mut toks = s.split_whitespace().peekable();
        let mut next_pane = 1u32;
        let root = parse(&mut toks, &mut next_pane, remap)?;
        if toks.next().is_some() {
            return None; // trailing tokens → malformed
        }
        let root = root.prune_empty()?; // nothing survived the remap → caller falls back
        let mut leaves = Vec::new();
        root.leaves(&mut leaves);
        let focus = leaves.first().map(|(pid, _)| *pid)?;
        let next_pane = root.max_pane_id() + 1;
        Some(PaneTree { root, focus, zoom: None, next_pane })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 }
    }

    /// The active doc shown in each pane, in layout order.
    fn docs_of(t: &PaneTree) -> Vec<DocId> {
        t.panes().into_iter().map(|(_, d)| d).collect()
    }

    #[test]
    fn serialize_round_trips_and_remaps_docids() {
        // Build: SideBySide split of doc 1 and a Stacked split of docs 2 and 3.
        let mut t = PaneTree::new(DocId(1));
        let p2 = t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        t.set_focus(p2);
        t.split(Split::Stacked, DocId(3), Side::Second).unwrap();
        assert_eq!(t.pane_count(), 3);
        let s = t.serialize();

        // Round-trip with a +100 remap: same shape, three panes, doc ids shifted.
        let back = PaneTree::from_serialized(&s, &|d| Some(DocId(d.0 + 100))).expect("parses");
        assert_eq!(back.pane_count(), 3);
        let docs: std::collections::HashSet<u32> = back.panes().iter().map(|(_, d)| d.0).collect();
        assert_eq!(docs, [101, 102, 103].into_iter().collect());

        // A remap that rejects one single-tab pane's doc → that pane collapses; the rest survive.
        let back2 = PaneTree::from_serialized(&s, &|d| (d.0 != 2).then_some(d)).expect("still parses");
        assert_eq!(back2.pane_count(), 2);
        assert!(!back2.contains_doc(DocId(2)));
        assert!(back2.contains_doc(DocId(1)) && back2.contains_doc(DocId(3)));

        // Garbage tokens → None, not a panic.
        assert!(PaneTree::from_serialized("S x", &|d| Some(d)).is_none());
        assert!(PaneTree::from_serialized("", &|d| Some(d)).is_none());
    }

    #[test]
    fn serialize_session_round_trips_view_state_and_groups() {
        // Two panes; the left pane has TWO tabs (a group), the right one.
        let mut t = PaneTree::new(DocId(1));
        t.reconcile(&[DocId(1), DocId(9)], DocId(1)); // doc 9 joins the (single) focused group
        let p2 = t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        let p1 = t.panes().into_iter().find(|&(_, d)| d == DocId(1)).unwrap().0;
        t.set_view(p1, ViewState { scroll_line: 42, scroll_vertical: 7.5, follow: false });
        t.set_view(p2, ViewState { scroll_line: 0, scroll_vertical: 0.0, follow: true });
        // Left pane (p1) holds docs 1 and 9; right pane (p2) holds doc 2.
        assert!(t.contains_doc(DocId(1)) && t.contains_doc(DocId(9)) && t.contains_doc(DocId(2)));

        let s = t.serialize_session();
        let back = PaneTree::from_serialized(&s, &|d| Some(d)).expect("parses");
        assert!(back.contains_doc(DocId(1)) && back.contains_doc(DocId(9)) && back.contains_doc(DocId(2)));
        let bp1 = back.root.pane_of_doc(DocId(1)).unwrap();
        // doc 9 shares doc 1's pane (the group survived).
        assert_eq!(back.root.pane_of_doc(DocId(9)), Some(bp1));
        let v1 = back.view(bp1).unwrap();
        assert_eq!(v1.scroll_line, 42);
        assert!((v1.scroll_vertical - 7.5).abs() < 1e-3);
        assert!(!v1.follow);

        // The view-less `L` form still parses, with default views.
        let plain = t.serialize();
        let back2 = PaneTree::from_serialized(&plain, &|d| Some(d)).expect("parses");
        let bp = back2.root.pane_of_doc(DocId(1)).unwrap();
        assert!(back2.view(bp).unwrap().follow); // default
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
        assert_eq!(rects[0].doc, DocId(1));
        assert_eq!(rects[1].doc, DocId(2));
        assert_eq!(rects[0].rect.w, 400.0);
        assert_eq!(rects[1].rect.x, 400.0);
        assert_eq!(rects[0].rect.h, 600.0);
    }

    #[test]
    fn reconcile_files_new_docs_into_focused_group_and_drops_closed() {
        let mut t = PaneTree::new(DocId(1));
        // Open two more docs: they join the (sole) focused group.
        t.reconcile(&[DocId(1), DocId(2), DocId(3)], DocId(3));
        assert_eq!(t.pane_count(), 1);
        assert_eq!(t.focused_tab_docs(), vec![DocId(1), DocId(2), DocId(3)]);
        assert_eq!(t.focused_doc(), DocId(3)); // active followed

        // Split doc 2 into its own pane, then close doc 1 (still in the left group).
        t.focus_doc_if_shown(DocId(2));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        // Left group now [1,3], right group [2].
        assert!(t.contains_doc(DocId(1)) && t.contains_doc(DocId(3)) && t.contains_doc(DocId(2)));
        // Close doc 1: the LEFT pane stays (still shows doc 3); no collapse, no reshuffle.
        t.reconcile(&[DocId(2), DocId(3)], DocId(3));
        assert_eq!(t.pane_count(), 2);
        assert!(!t.contains_doc(DocId(1)));
        assert_eq!(docs_of(&t), vec![DocId(3), DocId(2)]); // left shows 3, right shows 2
    }

    #[test]
    fn closing_a_paned_tab_keeps_the_pane_when_group_has_more() {
        // Left pane group [1,4], right pane [2]. Close doc 1 → left pane stays, shows doc 4.
        let mut t = PaneTree::new(DocId(1));
        t.reconcile(&[DocId(1), DocId(4)], DocId(1));
        t.focus_doc_if_shown(DocId(2)); // not shown yet → false; reconcile adds it next
        t.reconcile(&[DocId(1), DocId(4), DocId(2)], DocId(2));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap(); // right = [2]; left = [1,4]
        assert_eq!(t.pane_count(), 2);
        t.reconcile(&[DocId(4), DocId(2)], DocId(4)); // closed doc 1
        assert_eq!(t.pane_count(), 2); // left pane survived
        assert!(t.contains_doc(DocId(4)) && t.contains_doc(DocId(2)));
    }

    #[test]
    fn split_moves_tab_out_of_its_old_group() {
        let mut t = PaneTree::new(DocId(1));
        t.reconcile(&[DocId(1), DocId(2)], DocId(1)); // one pane, group [1,2]
        // Split doc 2 into a new pane: it must LEAVE the left group.
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        assert_eq!(t.pane_count(), 2);
        let left = t.root.pane_of_doc(DocId(1)).unwrap();
        let right = t.root.pane_of_doc(DocId(2)).unwrap();
        assert_ne!(left, right);
        // Left group is just [1] now (2 moved out).
        t.set_focus(left);
        assert_eq!(t.focused_tab_docs(), vec![DocId(1)]);
    }

    #[test]
    fn one_pane_per_doc_guard_via_partition() {
        let mut t = PaneTree::new(DocId(1));
        t.reconcile(&[DocId(1), DocId(2)], DocId(1));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        // Each doc appears in exactly one pane.
        assert_eq!(t.root.pane_of_doc(DocId(1)).is_some(), true);
        assert_eq!(t.root.pane_of_doc(DocId(2)).is_some(), true);
        assert_ne!(t.root.pane_of_doc(DocId(1)), t.root.pane_of_doc(DocId(2)));
        let mut all = Vec::new();
        t.root.all_tabs(&mut all);
        assert_eq!(all.len(), 2); // no duplicates
    }

    #[test]
    fn close_focused_merges_tabs_into_sibling() {
        // Left [1,3], right [2] (focused). TUS (close_focused) → one pane holding all three.
        let mut t = PaneTree::new(DocId(1));
        t.reconcile(&[DocId(1), DocId(3)], DocId(1));
        t.reconcile(&[DocId(1), DocId(3), DocId(2)], DocId(2));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap(); // right = [2] focused
        assert!(t.close_focused());
        assert_eq!(t.pane_count(), 1);
        // All three docs survive as tabs in the remaining pane.
        assert!(t.contains_doc(DocId(1)) && t.contains_doc(DocId(2)) && t.contains_doc(DocId(3)));
    }

    #[test]
    fn collapse_to_focused_keeps_all_docs_as_tabs() {
        let mut t = PaneTree::new(DocId(1));
        t.reconcile(&[DocId(1), DocId(2), DocId(3)], DocId(1));
        t.focus_doc_if_shown(DocId(2));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap(); // left [1,3], right [2]
        t.focus_doc_if_shown(DocId(2));
        assert!(t.collapse_to_focused()); // FS keeps the right pane (doc 2), merges 1 & 3
        assert_eq!(t.pane_count(), 1);
        assert_eq!(t.focused_doc(), DocId(2)); // the kept pane's active tab unchanged
        assert!(t.contains_doc(DocId(1)) && t.contains_doc(DocId(3)));
    }

    #[test]
    fn bufnext_cycles_within_focused_group() {
        let mut t = PaneTree::new(DocId(1));
        t.reconcile(&[DocId(1), DocId(2), DocId(3)], DocId(1)); // group [1,2,3], active 1
        assert_eq!(t.focused_cycle_target(true), Some(DocId(2)));
        assert_eq!(t.focused_cycle_target(false), Some(DocId(3))); // wraps backward
        // Put doc 3 in its own pane; the left group is [1,2] → next from 1 is 2, wrap.
        t.focus_doc_if_shown(DocId(3));
        t.split(Split::SideBySide, DocId(3), Side::Second).unwrap(); // right [3] focused
        assert_eq!(t.focused_cycle_target(true), None); // single-tab group: nothing to cycle
        t.focus_doc_if_shown(DocId(1)); // focus the left group [1,2]
        assert_eq!(t.focused_cycle_target(true), Some(DocId(2)));
    }

    #[test]
    fn stacked_split_divides_height() {
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::Stacked, DocId(2), Side::Second).unwrap();
        let r = t.layout_rects(area());
        assert_eq!(r[0].rect.h, 300.0);
        assert_eq!(r[1].rect.y, 300.0);
        assert_eq!(r[0].rect.w, 800.0);
    }

    #[test]
    fn focus_cycle_wraps() {
        let mut t = PaneTree::new(DocId(1));
        let p2 = t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        let p1 = t.panes()[0].0;
        assert_eq!(t.focus(), p2);
        t.focus_cycle(true);
        assert_eq!(t.focus(), p1);
        t.focus_cycle(true);
        assert_eq!(t.focus(), p2);
        t.focus_cycle(false);
        assert_eq!(t.focus(), p1);
    }

    #[test]
    fn swap_focused_swaps_the_two_panes() {
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        assert!(t.swap_focused());
        assert_eq!(docs_of(&t), vec![DocId(2), DocId(1)]);
    }

    #[test]
    fn zoom_shows_only_the_focused_pane_and_toggles_back() {
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        t.toggle_zoom();
        let r = t.layout_rects(area());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].doc, DocId(2));
        assert_eq!(r[0].rect, area());
        assert_eq!(t.pane_count(), 2);
        t.toggle_zoom();
        assert_eq!(t.layout_rects(area()).len(), 2);
    }

    #[test]
    fn dividers_report_and_resize_the_right_split() {
        let mut t = PaneTree::new(DocId(1));
        t.split(Split::SideBySide, DocId(2), Side::Second).unwrap();
        let ds = t.dividers(area());
        assert_eq!(ds.len(), 1);
        assert!(ds[0].side_by_side);
        assert!((ds[0].bar.x + ds[0].bar.w / 2.0 - 400.0).abs() < 1.0);
        assert!(t.set_divider_ratio(0, 0.25));
        let r = t.layout_rects(area());
        assert_eq!(r[0].rect.w, 200.0);
        assert_eq!(r[1].rect.x, 200.0);
        t.toggle_zoom();
        assert!(t.dividers(area()).is_empty());
    }
}
