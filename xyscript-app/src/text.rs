//! Text engine (`development-plan.md` Stage 2; `byte-text-engine-plan.md` step 2).
//!
//! An insert/delete buffer with a cursor and an optional selection — "a correct,
//! observable model the interpreter can mutate." The byte-oriented re-architecture
//! (the EditPad model) swaps the storage from a UTF-8 `ropey` rope to **raw bytes
//! (`Vec<u8>`) plus a switchable [`Encoding`] view**, behind the *same public API*,
//! so the proven editor loop, verbs, undo, and viewport are unchanged. The file's
//! bytes are the truth; the encoding decodes them for display and encodes typed
//! text back to bytes (a character the encoding can't represent encodes to `?`).
//!
//! **All positions (cursor, selection anchor) are byte offsets** that always sit
//! on a **character boundary**. For single-byte encodings 1 byte == 1 char, so
//! this is trivial; for **UTF-8** (step 7) a char spans 1–4 bytes, so motion and
//! deletion step by whole characters and `column`/vertical motion count
//! characters. The same code path serves both — the [`Charset`] reports
//! `lead_len`/`is_continuation`/`char_count`, and single-byte is just the 1:1
//! case. `encode(decode(bytes)) == bytes` keeps undo/redo byte-exact; undo ops
//! are byte vectors regardless of encoding.
//!
//! Step 6 (BE1) swaps the underlying storage from a flat `Vec<u8>` to a
//! [`ByteRope`] (chunked, newline-indexed) so 100–300 MB files edit without lag.
//! The change is **behind this same public API** — the rope speaks byte offsets,
//! exactly as the `Vec<u8>` did — so the editor loop, verbs, undo, and viewport
//! are untouched. (The only signature change: [`Buffer::bytes`] now materializes
//! a `Vec<u8>`, since a rope has no single contiguous slice.)

use crate::byte_rope::ByteRope;
use crate::encoding::{utf8, Charset, Encoding};

/// Result of matching the bracket at/adjacent to the caret ([`Buffer::bracket_match`], `BALPAIRS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalMatch {
    /// The caret isn't on a bracket.
    None,
    /// The two bracket byte offsets (opening first) to highlight.
    Matched(usize, usize),
    /// The caret is on a bracket with no partner (byte offset + its char) — report, don't highlight.
    Unmatched(usize, u8),
}

/// A secondary caret in a multi-caret set (the OVERLAY model). The PRIMARY caret is the
/// `Buffer`'s own `cursor`/`anchor`/`goal_col`, so all single-caret code is unchanged;
/// `Buffer::secondary` holds the extra carets and is EMPTY in normal use. Each secondary has a
/// position, an optional selection anchor, and its own sticky goal column. Editing and motion
/// fan out to the primary plus every secondary only when the set is non-empty. The buffer-global
/// modes (`defining`/`persistent`/`sel_head`/`defined`) are NOT per-secondary in Tier 1 — they
/// apply uniformly. See the multi-caret specs (multiselect-verbs.txt / multiselect-specials.txt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    /// Caret byte offset (char boundary), like the primary `cursor`.
    pub pos: usize,
    /// Selection anchor (char boundary); `Some` = this caret spans `ordered(anchor, pos)`.
    pub anchor: Option<usize>,
    /// This caret's sticky goal column for vertical motion (mirrors the primary `goal_col`).
    pub goal_col: Option<usize>,
}

/// A single text buffer: a byte rope, a current encoding view, a cursor, and an
/// optional selection.
#[derive(Debug, Clone)]
pub struct Buffer {
    /// The document's bytes — the source of truth, never altered by viewing.
    rope: ByteRope,
    /// The current encoding view (a single-byte code page or UTF-8).
    enc: Charset,
    /// Cursor position as a byte offset (on a char boundary) in `0..=rope.len()`.
    cursor: usize,
    /// Selection anchor (byte offset, char boundary); `Some` means an active
    /// selection between `anchor` and the active end (in either order).
    anchor: Option<usize>,
    /// `DF` "defined text" mode (auto-track, XyWrite/QN-style): while true, plain
    /// cursor motion **extends** the selection (the anchor is held, the cursor is
    /// the live active end) without needing Shift. `DF`/`DZ` close it; `XD`/`YD`
    /// cancel. The selection is the standard `ordered(anchor, cursor)` span.
    defining: bool,
    /// Persistent-selection mode: when true, plain (non-Shift) cursor motion keeps
    /// the selection instead of clearing it — the caret **decouples** and roams off
    /// the block (see `sel_head`). Set globally via `SPS`/`CPS`/`TPS`.
    persistent: bool,
    /// The selection's frozen active end while the caret has roamed off it under
    /// persistent selection. `None` = coupled (the active end is the cursor — the
    /// normal case); `Some(h)` = decoupled: the span is `ordered(anchor, h)` and the
    /// caret (`cursor`) is independent. `DM` re-couples; any edit/clear resets it.
    sel_head: Option<usize>,
    /// Whether the current selection is a **defined block** (`DF`/`DZ`/`DFA`/`DM`) —
    /// which **persists across plain cursor motion** (XyWrite "defined text"), the
    /// caret decoupling to roam off it. A *transient* selection (Shift+motion / mouse
    /// drag) has `defined = false` and collapses on a plain motion (modern behavior).
    defined: bool,
    /// Memo checkpoints for the encoding-offset conversions, so a forward sweep (e.g. a
    /// GUI repainting the caret every frame while paging) decodes only the delta since the
    /// last call instead of the whole prefix. Each holds `(rope rev, from, to)`; reused
    /// only when the rev still matches and the new query is at/after the checkpoint, so
    /// the result stays exact. Reset on an encoding switch (the decode itself changes).
    dec_hint: std::cell::Cell<(u64, usize, usize)>, // (rev, rope_off, decoded_off)
    rope_hint: std::cell::Cell<(u64, usize, usize)>, // (rev, decoded_off, rope_off)
    /// Sparse `(rope_off, decoded_off)` checkpoints every ~[`DEC_INDEX_SPACING`] rope bytes,
    /// built once for a LARGE non-identity buffer so `decoded_offset`/`rope_offset` never walk
    /// the whole prefix from 0 — the first access at the bottom of a 100+ MB single-byte file was
    /// otherwise an O(file) decode (a multi-hundred-ms freeze). `(rev, checkpoints)`; the `rev`
    /// tags which content it describes, so an edit (rev bump) simply retires it (offsets after the
    /// edit shift) and access falls back to the `dec_hint` walk until the next whole-buffer load.
    dec_index: std::cell::RefCell<(u64, Vec<(usize, usize)>)>,
    /// True iff the view is UTF-8 **and the bytes are valid UTF-8**, so a rope byte
    /// offset equals its decoded-string offset (the `decoded_offset`/`rope_offset` O(1)
    /// identity). Recomputed only on a whole-buffer load or an encoding switch — edits
    /// preserve it (inserts come from `&str`, deletes land on char boundaries). When
    /// false (single-byte view, or invalid bytes shown lossily as U+FFFD), the offset
    /// conversions fall back to the exact decode-and-count path.
    utf8_identity: bool,
    /// Sticky goal column (in characters) for vertical motion (`LU`/`LD`/`UP`/`DOWN`/`PU`/`PD`): the
    /// column the caret tries to keep as it moves up/down, so passing through a SHORT or BLANK line
    /// doesn't collapse it. `None` until a vertical move arms it; any non-vertical move clears it
    /// (via `prepare_move` and the direct caret setters).
    goal_col: Option<usize>,
    /// Secondary carets for multi-caret editing (the OVERLAY model): EMPTY in the normal
    /// single-caret case, so every existing method is unchanged. When non-empty, editing and
    /// motion fan out to the primary (`cursor`/`anchor`/`goal_col`) plus each of these. Kept
    /// sorted by `pos` and deduplicated (coincident carets merge). See [`Caret`].
    secondary: Vec<Caret>,
    /// The multi-caret BUILDING state (bare keyboard `CARETADD`): while true, plain motion moves
    /// the PRIMARY caret only — so the keyboard can roam and plant more carets. The first edit
    /// clears it (motion then moves all carets); collapse/`CARETONE` also clears it.
    building: bool,
}

/// The "no valid checkpoint" sentinel rev (no real rope rev reaches it).
const NO_HINT: (u64, usize, usize) = (u64::MAX, 0, 0);

/// Rope bytes between [`Buffer::dec_index`] checkpoints — the worst-case walk for a cold
/// `decoded_offset`/`rope_offset` once the index is built (a 256 KB decode ≈ sub-millisecond).
const DEC_INDEX_SPACING: usize = 256 * 1024;
/// Only large buffers get an index; under this a cold whole-prefix walk is already trivial, and
/// building/holding the checkpoints isn't worth it (small edited files stay simple).
const DEC_INDEX_MIN_LEN: usize = 1024 * 1024;

impl Default for Buffer {
    fn default() -> Buffer {
        // New documents default to UTF-8 (the authoring use): every character is
        // typeable. Opening a file picks a view by detection (Document::load);
        // single-byte views remain for legacy/encoded files.
        Buffer {
            rope: ByteRope::new(),
            enc: utf8(),
            cursor: 0,
            anchor: None,
            defining: false,
            persistent: false,
            sel_head: None,
            defined: false,
            dec_hint: std::cell::Cell::new(NO_HINT),
            rope_hint: std::cell::Cell::new(NO_HINT),
            dec_index: std::cell::RefCell::new((u64::MAX, Vec::new())),
            utf8_identity: true, // empty UTF-8 buffer: offsets coincide
            goal_col: None,
            secondary: Vec::new(),
            building: false,
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Multi-caret overlay (Tier 1). The single-caret path is the `secondary`-empty case; these methods
// manage the extra carets and the keyboard "building" state. See the multi-caret specs.
// ---------------------------------------------------------------------------------------------------
impl Buffer {
    /// True when a multi-caret set is active (there is at least one secondary caret).
    pub fn is_multi(&self) -> bool {
        !self.secondary.is_empty()
    }

    /// Number of active carets — primary plus secondaries (`$CARETS`).
    pub fn caret_count(&self) -> usize {
        1 + self.secondary.len()
    }

    /// The secondary carets (read-only; sorted by `pos`).
    pub fn secondary_carets(&self) -> &[Caret] {
        &self.secondary
    }

    /// Replace the secondary-caret set wholesale (kept sorted). Used by undo/redo to restore the
    /// multi-caret set snapshot recorded with a multi-caret edit.
    pub fn set_secondary_carets(&mut self, mut carets: Vec<Caret>) {
        carets.sort_by_key(|c| c.pos);
        self.secondary = carets;
    }

    /// Whether the multi-caret BUILDING state is active (`CARETADD` planting from the keyboard).
    pub fn building(&self) -> bool {
        self.building
    }

    /// Enter/leave the building state. Leaving is also implied by `collapse_carets` and (later) by
    /// the first edit; entering happens on a bare-keyboard `CARETADD`.
    pub fn set_building(&mut self, on: bool) {
        self.building = on;
    }

    /// Collapse to the single primary caret (`CARETONE`): drop every secondary and end building.
    /// The primary (`cursor`/`anchor`/`goal_col`) is left exactly as it is.
    pub fn collapse_carets(&mut self) {
        self.secondary.clear();
        self.building = false;
    }

    /// `CARETADD` primitive: toggle a secondary caret at byte offset `pos`. Adds one if none is
    /// there, else removes the existing one. Never touches the PRIMARY (use `collapse_carets` for
    /// that). A caret coincident with the primary is allowed (they separate when the primary moves,
    /// and merge on the next `normalize_carets` if still coincident). Returns the new caret count.
    pub fn add_caret(&mut self, pos: usize) -> usize {
        if let Some(i) = self.secondary.iter().position(|c| c.pos == pos) {
            self.secondary.remove(i);
        } else {
            self.secondary.push(Caret { pos, anchor: None, goal_col: None });
            self.secondary.sort_by_key(|c| c.pos);
        }
        self.caret_count()
    }

    /// The primary caret's index within the document-order caret list (`$CARETP`): the number of
    /// carets sitting before it. Invariant with `caret_positions`: `caret_positions()[primary_index()]
    /// == cursor()`.
    pub fn primary_index(&self) -> usize {
        self.secondary.iter().filter(|c| c.pos < self.cursor).count()
    }

    /// Every caret position (primary + secondaries), sorted ascending — the fan-out order for
    /// edits/motion (callers apply edits HIGHEST-first so earlier offsets don't shift).
    pub fn caret_positions(&self) -> Vec<usize> {
        let mut v: Vec<usize> = std::iter::once(self.cursor).chain(self.secondary.iter().map(|c| c.pos)).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Merge coincident carets: drop any secondary sharing the primary's position or another
    /// secondary's, and keep the list sorted. Call after motion/edits move carets onto each other.
    pub fn normalize_carets(&mut self) {
        if self.secondary.is_empty() {
            return;
        }
        let cursor = self.cursor;
        self.secondary.retain(|c| c.pos != cursor); // fold a secondary on the primary into it
        self.secondary.sort_by_key(|c| c.pos);
        self.secondary.dedup_by_key(|c| c.pos);
    }

    /// Apply a motion (a closure that moves the PRIMARY caret) to EVERY caret — each keeping its own
    /// selection anchor and goal column — then merge coincident carets. Used when a multi-caret set
    /// is active so one motion verb moves the whole set. During the BUILDING state (or with no
    /// secondaries) it moves the primary only, so the keyboard can roam to plant more carets.
    pub fn move_all_carets(&mut self, mut motion: impl FnMut(&mut Buffer)) {
        if self.building || self.secondary.is_empty() {
            motion(self);
            return;
        }
        // Move each secondary by loading it into the primary slot, applying the motion, saving back.
        let (sc, sa, sg) = (self.cursor, self.anchor, self.goal_col);
        let secs = std::mem::take(&mut self.secondary);
        let mut moved: Vec<Caret> = Vec::with_capacity(secs.len());
        for c in secs {
            self.cursor = c.pos;
            self.anchor = c.anchor;
            self.goal_col = c.goal_col;
            motion(self);
            moved.push(Caret { pos: self.cursor, anchor: self.anchor, goal_col: self.goal_col });
        }
        // Restore the real primary and move it LAST (so the viewport + classic specials track it).
        self.cursor = sc;
        self.anchor = sa;
        self.goal_col = sg;
        self.secondary = moved;
        motion(self);
        self.normalize_carets();
    }
}

impl Buffer {
    pub fn new() -> Buffer {
        Buffer::default()
    }

    /// Build a buffer from a string (stored as UTF-8 — a Rust `&str` *is* UTF-8),
    /// cursor at the end.
    // Deliberate infallible constructor, not `std::str::FromStr` (no parse error).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Buffer {
        let enc = utf8();
        let rope = ByteRope::from_bytes(enc.encode(s));
        let cursor = rope.len();
        Buffer { rope, enc, cursor, ..Buffer::default() }
    }

    /// Build a buffer from raw bytes viewed through `enc`, cursor at the end.
    /// (The byte-exact open path; callers reposition the caret as needed.)
    pub fn from_bytes(bytes: Vec<u8>, enc: Charset) -> Buffer {
        // Validate the input bytes directly (no extra copy) so the O(1) offset identity
        // is only claimed when the view really is byte-for-byte UTF-8.
        let utf8_identity = enc.name() == "utf-8" && std::str::from_utf8(&bytes).is_ok();
        let rope = ByteRope::from_bytes(bytes);
        let cursor = rope.len();
        let b = Buffer { rope, enc, cursor, utf8_identity, ..Buffer::default() };
        b.build_dec_index(); // bound cold offset walks at open (huge single-byte files)
        b
    }

    /// The full buffer contents, decoded through the current encoding. O(n): it
    /// materializes the whole document (the whole-buffer-render path); the viewport
    /// uses [`line_text`](Buffer::line_text) per line instead.
    pub fn text(&self) -> String {
        self.enc.decode(&self.rope.to_vec())
    }

    /// The raw bytes (the truth; byte-exact save writes these verbatim).
    /// Materializes from the rope, so callers should treat it as a snapshot.
    pub fn bytes(&self) -> Vec<u8> {
        self.rope.to_vec()
    }

    /// A clone of the current encoding view (for line-ending conversion, which rebuilds the buffer
    /// from rewritten bytes but must keep the same charset).
    pub fn encoding(&self) -> Charset {
        self.enc.clone()
    }

    /// Cursor position (byte offset).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Number of bytes in the buffer (== characters for single-byte encodings).
    pub fn len(&self) -> usize {
        self.rope.len()
    }

    /// The raw rope byte at `pos`, or `None` past the end. For cheap ASCII peeks (bracket/quote
    /// detection in `TYPEPAIRS`) without materializing `text()`.
    pub fn byte_at(&self, pos: usize) -> Option<u8> {
        (pos < self.rope.len()).then(|| self.rope.byte(pos))
    }

    pub fn is_empty(&self) -> bool {
        self.rope.is_empty()
    }

    /// The buffer's monotonic edit counter (see [`ByteRope::rev`]). Bumped on every
    /// byte mutation; encoding switches do not touch it (the bytes are unchanged).
    pub fn revision(&self) -> u64 {
        self.rope.rev()
    }

    pub fn has_selection(&self) -> bool {
        self.anchor.is_some()
    }

    /// The raw selection anchor (the end opposite the cursor), if any. Needed by
    /// undo to restore the exact selection state (which end the cursor was on).
    pub fn anchor(&self) -> Option<usize> {
        self.anchor
    }

    /// The selection as an ordered `(start, end)` byte range, if any (public).
    pub fn selection_span(&self) -> Option<(usize, usize)> {
        self.selection_range()
    }

    /// Map a **rope byte offset** (cursor/selection space) to the byte offset in
    /// the decoded [`text`](Buffer::text) string. They coincide for UTF-8; under a
    /// single-byte view (CP437/Latin-1) a byte can decode to a multi-byte char, so
    /// the two offset spaces differ. A renderer that indexes the decoded string by
    /// a rope offset must translate through this or it will slice mid-character.
    pub fn decoded_offset(&self, rope_off: usize) -> usize {
        let off = rope_off.min(self.rope.len());
        if self.utf8_identity {
            return off; // rope bytes *are* the (valid) UTF-8 string; offsets coincide
        }
        // Decode only the delta from the CLOSEST known (rope, decoded) anchor. The moving
        // `dec_hint` checkpoint keeps a forward/backward sweep (paging) cheap; the sparse
        // `dec_index` bounds a COLD or far jump (e.g. the first paint at the bottom of a huge
        // single-byte file) to one inter-checkpoint span instead of decoding from 0. Exact from
        // any anchor: the decoded length of a byte range doesn't depend on where the walk starts,
        // and every anchor/`off` is a char boundary.
        let rev = self.rope.rev();
        let (mut a_rope, mut a_dec) = (0usize, 0usize); // walk-from-0 fallback
        let mut best = off; // distance from the current best anchor
        let (h_rev, h_rope, h_dec) = self.dec_hint.get();
        if h_rev == rev && off.abs_diff(h_rope) < best {
            best = off.abs_diff(h_rope);
            (a_rope, a_dec) = (h_rope, h_dec);
        }
        if let Some((r, d)) = self.dec_index_anchor(rev, off) {
            if off.abs_diff(r) < best {
                (a_rope, a_dec) = (r, d);
            }
        }
        let result = if off >= a_rope {
            a_dec + self.enc.decode(&self.rope.slice_to_vec(a_rope, off)).len()
        } else {
            a_dec - self.enc.decode(&self.rope.slice_to_vec(off, a_rope)).len()
        };
        self.dec_hint.set((rev, off, result));
        result
    }

    /// The `dec_index` checkpoint bracketing `off` most closely (floor or ceil), if the index is
    /// built and still describes the current `rev`. `None` when there's no usable index (small
    /// buffer, UTF-8, or retired by an edit), so the caller falls back to the `dec_hint` walk.
    fn dec_index_anchor(&self, rev: u64, off: usize) -> Option<(usize, usize)> {
        let idx = self.dec_index.borrow();
        if idx.0 != rev || idx.1.is_empty() {
            return None;
        }
        let cps = &idx.1;
        // Largest checkpoint with rope_off <= off (floor); the ceil neighbour may be nearer for a
        // backward walk. `off` beyond the last checkpoint just uses the last (still bounded, since
        // checkpoints run to `len`).
        let i = cps.partition_point(|&(r, _)| r <= off).saturating_sub(1);
        let floor = cps[i];
        match cps.get(i + 1) {
            Some(&ceil) if off.abs_diff(ceil.0) < off.abs_diff(floor.0) => Some(ceil),
            _ => Some(floor),
        }
    }

    /// Like [`dec_index_anchor`](Buffer::dec_index_anchor) but keyed on the DECODED component, for
    /// [`rope_offset`](Buffer::rope_offset)'s inverse walk. The checkpoints are monotonic in both
    /// coordinates, so the same table serves both directions.
    fn rope_index_anchor(&self, rev: u64, decoded_off: usize) -> Option<(usize, usize)> {
        let idx = self.dec_index.borrow();
        if idx.0 != rev || idx.1.is_empty() {
            return None;
        }
        let cps = &idx.1;
        let i = cps.partition_point(|&(_, d)| d <= decoded_off).saturating_sub(1);
        let floor = cps[i];
        match cps.get(i + 1) {
            Some(&ceil) if decoded_off.abs_diff(ceil.1) < decoded_off.abs_diff(floor.1) => Some(ceil),
            _ => Some(floor),
        }
    }

    /// Build the sparse decoded-offset index for the current content, but only for a LARGE
    /// non-identity buffer (a UTF-8-identity view needs none — offsets coincide). One decode pass
    /// over the rope in [`DEC_INDEX_SPACING`] steps; called on a whole-buffer load / encoding
    /// switch, so the O(file) cost lands at open, not on the first scroll to the bottom.
    fn build_dec_index(&self) {
        if self.utf8_identity || self.rope.len() < DEC_INDEX_MIN_LEN {
            *self.dec_index.borrow_mut() = (u64::MAX, Vec::new());
            return;
        }
        let rev = self.rope.rev();
        let len = self.rope.len();
        let mut cps: Vec<(usize, usize)> = Vec::with_capacity(len / DEC_INDEX_SPACING + 2);
        let (mut rope_pos, mut dec_pos) = (0usize, 0usize);
        cps.push((0, 0));
        while rope_pos < len {
            let next = self.snap_boundary((rope_pos + DEC_INDEX_SPACING).min(len)).max(rope_pos + 1).min(len);
            dec_pos += self.enc.decode(&self.rope.slice_to_vec(rope_pos, next)).len();
            rope_pos = next;
            cps.push((rope_pos, dec_pos));
        }
        *self.dec_index.borrow_mut() = (rev, cps);
    }

    /// Inverse of [`decoded_offset`](Buffer::decoded_offset): map a byte offset in
    /// the decoded string back to a rope byte offset (e.g. a mouse hit-test result
    /// → a cursor position). Walks char boundaries accumulating decoded lengths.
    pub fn rope_offset(&self, decoded_off: usize) -> usize {
        if self.utf8_identity {
            return decoded_off.min(self.rope.len());
        }
        let len = self.rope.len();
        // Walk from the cached checkpoint — forward or backward — so paging up is as cheap
        // as paging down. The result (the first boundary whose decoded length reaches
        // `decoded_off`) is the same whichever direction we approach from.
        let rev = self.rope.rev();
        // Start from the CLOSEST known anchor: the warm `rope_hint` (sustained sweep) or the nearest
        // `dec_index` checkpoint (bounds a cold/far jump to one inter-checkpoint span). Same index as
        // `decoded_offset` — its `(rope, decoded)` pairs are monotonic, so it's searchable by the
        // decoded component too.
        let (h_rev, h_dec, h_rope) = self.rope_hint.get();
        let (mut rope_pos, mut dec_pos) = (0usize, 0usize);
        let mut best = decoded_off; // distance (in decoded bytes) from the walk-from-0 fallback
        if h_rev == rev && decoded_off.abs_diff(h_dec) < best {
            best = decoded_off.abs_diff(h_dec);
            (rope_pos, dec_pos) = (h_rope, h_dec);
        }
        if let Some((r, d)) = self.rope_index_anchor(rev, decoded_off) {
            if decoded_off.abs_diff(d) < best {
                (rope_pos, dec_pos) = (r, d);
            }
        }
        if dec_pos <= decoded_off {
            while rope_pos < len && dec_pos < decoded_off {
                let next = self.next_boundary(rope_pos);
                dec_pos += self.enc.decode(&self.rope.slice_to_vec(rope_pos, next)).len();
                rope_pos = next;
            }
        } else {
            // Step back over chars while the previous boundary still reaches the target.
            while rope_pos > 0 {
                let prev = self.prev_boundary(rope_pos);
                let prev_dec = dec_pos - self.enc.decode(&self.rope.slice_to_vec(prev, rope_pos)).len();
                if prev_dec < decoded_off {
                    break;
                }
                rope_pos = prev;
                dec_pos = prev_dec;
            }
        }
        self.rope_hint.set((rev, dec_pos, rope_pos));
        rope_pos
    }

    /// Like [`decoded_offset`](Buffer::decoded_offset) but anchored to a CALLER-KNOWN
    /// (`anchor_rope`, `anchor_decoded`) correspondence instead of the internal moving
    /// checkpoint. Both must be char boundaries that correspond. Decodes only the delta between
    /// the anchor and `rope_off` in a single pass, so a caller that already holds a nearby fixed
    /// point (e.g. a render window's start line) translates in O(delta) — never a document-global
    /// walk — and neither reads nor disturbs the shared checkpoint (so it can't be thrashed by,
    /// or thrash, other callers within the same frame).
    pub fn decoded_offset_from(&self, rope_off: usize, anchor_rope: usize, anchor_decoded: usize) -> usize {
        let off = rope_off.min(self.rope.len());
        if self.utf8_identity {
            return off; // rope bytes *are* the UTF-8 string; offsets coincide
        }
        if off >= anchor_rope {
            anchor_decoded + self.enc.decode(&self.rope.slice_to_vec(anchor_rope, off)).len()
        } else {
            anchor_decoded - self.enc.decode(&self.rope.slice_to_vec(off, anchor_rope)).len()
        }
    }

    /// Inverse of [`decoded_offset_from`](Buffer::decoded_offset_from): a decoded-string offset →
    /// rope offset, anchored to a known correspondence. Walks char boundaries from the anchor
    /// (forward or backward) until the decoded length reaches `decoded_off` — bounded to the
    /// caller's window, never a document-global walk, and independent of the shared checkpoint.
    pub fn rope_offset_from(&self, decoded_off: usize, anchor_rope: usize, anchor_decoded: usize) -> usize {
        if self.utf8_identity {
            return decoded_off.min(self.rope.len());
        }
        let len = self.rope.len();
        let (mut rope_pos, mut dec_pos) = (anchor_rope.min(len), anchor_decoded);
        if dec_pos <= decoded_off {
            while rope_pos < len && dec_pos < decoded_off {
                let next = self.next_boundary(rope_pos);
                dec_pos += self.enc.decode(&self.rope.slice_to_vec(rope_pos, next)).len();
                rope_pos = next;
            }
        } else {
            while rope_pos > 0 {
                let prev = self.prev_boundary(rope_pos);
                let prev_dec = dec_pos - self.enc.decode(&self.rope.slice_to_vec(prev, rope_pos)).len();
                if prev_dec < decoded_off {
                    break;
                }
                rope_pos = prev;
                dec_pos = prev_dec;
            }
        }
        rope_pos
    }

    /// Read a byte range as a decoded string.
    pub fn slice(&self, start: usize, end: usize) -> String {
        self.enc.decode(&self.rope.slice_to_vec(start, end))
    }

    /// Decode just the slice covering decoded-byte range `[dec_lo, dec_hi]` (clamped and
    /// boundary-snapped), WITHOUT materializing the whole document. Returns the decoded text and the
    /// absolute decoded-byte offset of its first character (so a caller can rebase offsets found
    /// inside it back to whole-document coordinates). The live language checker uses this to bound
    /// its work to the viewport — a full `text()` on a 100+ MB file is far too expensive per re-check.
    pub fn decoded_region(&self, dec_lo: usize, dec_hi: usize) -> (String, usize) {
        let rope_lo = self.snap_boundary(self.rope_offset(dec_lo));
        let rope_hi = self.snap_boundary(self.rope_offset(dec_hi));
        if rope_hi <= rope_lo {
            return (String::new(), self.decoded_offset(rope_lo));
        }
        (self.slice(rope_lo, rope_hi), self.decoded_offset(rope_lo))
    }

    /// Set the cursor and selection anchor directly (clamped to length). Used by
    /// undo/redo to restore the caret; does not move text. Every byte offset is a
    /// valid boundary for single-byte encodings, so clamping is just `min(len)`.
    /// Snap the caret off a mid-`\r\n` (or multibyte) boundary. A guard called before any edit so an
    /// insert/delete can never split a CRLF (stranding a `\r`/`\n`), however the caret got positioned.
    pub fn snap_caret(&mut self) {
        self.cursor = self.snap_boundary(self.cursor);
    }

    pub fn set_caret(&mut self, cursor: usize, anchor: Option<usize>) {
        // Snap both ends off a mid-`\r\n` (or multibyte) boundary so the caret can never wedge inside
        // a CRLF (a click/search edge that maps there would otherwise split it on the next insert).
        self.cursor = self.snap_boundary(cursor);
        self.anchor = anchor.map(|a| self.snap_boundary(a));
        self.defining = false; // a direct caret placement (mouse/undo) ends defining
        self.sel_head = None; // re-couple: the active end is the cursor again
        self.defined = false; // a drag/undo selection is transient, not a defined block
        self.goal_col = None; // a direct placement resets the vertical goal column
    }

    /// Place the caret at `off` from a mouse click. Like a plain motion: under
    /// persistent selection this is *navigation* — it keeps the block (the caret
    /// decouples and roams), and only `XD`/`YD` clears it; otherwise it clears any
    /// selection. (Mouse *drag* uses [`set_caret`] to make a fresh selection.)
    pub fn click_caret(&mut self, off: usize) {
        let off = self.snap_boundary(off); // never land inside a `\r\n` (a click at end-of-line maps there)
        self.defining = false;
        if self.anchor.is_some() && self.defined {
            // Navigation: a click keeps a *defined* block (the caret roams off it).
            if self.sel_head.is_none() {
                self.sel_head = Some(self.cursor); // freeze the block; the caret roams
            }
        } else {
            // No selection, or a transient one: clear it and place the caret.
            self.anchor = None;
            self.sel_head = None;
            self.defined = false;
        }
        self.cursor = off;
        self.goal_col = None; // a click resets the vertical goal column
    }

    /// Move only the caret to `off`, keeping any selection fixed as a scope: the block decouples
    /// and the caret roams over it, exactly like persistent-selection navigation. Used by `/S`
    /// search so the selection survives as a repeatable search scope (`selection_range` keeps
    /// reporting the same span via `sel_head`). No selection → just a plain caret move.
    pub fn roam_to(&mut self, off: usize) {
        let off = self.snap_boundary(off); // never land inside a `\r\n`
        if self.anchor.is_some() {
            if self.sel_head.is_none() {
                self.sel_head = Some(self.cursor); // freeze the block; the caret roams off it
            }
            self.defined = true; // the scope persists across further motion/searches
        }
        self.defining = false;
        self.cursor = off;
        self.goal_col = None; // a search/scope move resets the vertical goal column
    }

    /// Extend the selection to `off`: keep the current anchor (e.g. a `DF` define) and make `off`
    /// the active *coupled* edge, so the span becomes `ordered(anchor, off)`. With no anchor yet,
    /// anchor at the current caret first (so `SE/X` from a bare caret starts a fresh selection there).
    /// This is the grow-the-block counterpart to [`roam_to`] (which keeps the block fixed), used by
    /// the `/X` extend-search so a search grows the selection instead of replacing it.
    pub fn extend_to(&mut self, off: usize) {
        let off = self.snap_boundary(off); // never land inside a `\r\n`
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.sel_head = None; // the caret is the active end again (coupled)
        // Stay in defining mode so a follow-up plain motion keeps auto-tracking the active edge
        // (e.g. `SE/X /./` then `CL` shrinks the span to exclude the period). The span is the
        // standard `ordered(anchor, cursor)` form.
        self.defining = true;
        self.defined = true;
        self.cursor = off;
        self.goal_col = None; // an extend-to-offset resets the vertical goal column
    }

    /// Whether persistent-selection mode is on.
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    /// Set persistent-selection mode (`SPS`/`CPS`/`TPS`). Turning it **off** also
    /// re-couples any roamed selection (the caret becomes the active end again).
    pub fn set_persistent(&mut self, on: bool) {
        // Persistent-selection now governs only *edit* behavior (don't-replace +
        // ride-along); motion-persistence is unconditional, so this leaves the
        // selection/decoupled state alone.
        self.persistent = on;
    }

    /// The frozen active end while the caret has roamed off the block under
    /// persistent selection (`None` when coupled). Lets the renderer show the block
    /// independently of the caret.
    pub fn sel_head(&self) -> Option<usize> {
        self.sel_head
    }

    /// Read a raw byte range (the byte-faithful form the undo log records).
    pub fn slice_bytes(&self, start: usize, end: usize) -> Vec<u8> {
        self.rope.slice_to_vec(start, end)
    }

    /// Encode a string to bytes through the current view (a char the view can't
    /// represent becomes `?`). Used by recorded edits that build their own bytes.
    pub fn encode(&self, s: &str) -> Vec<u8> {
        self.enc.encode(s)
    }

    /// The byte range of the character starting at `at`: `[at, next-boundary)`. An
    /// empty `(at, at)` at end-of-buffer (no character there).
    pub fn char_span_at(&self, at: usize) -> (usize, usize) {
        (at, self.next_boundary(at))
    }

    /// The character to the right of the caret, decoded through the current view — what `$CH`
    /// reports. A `\r\n` break is one logical character, reported as `"\n"`; empty at
    /// end-of-buffer (nothing to the right).
    pub fn char_right(&self) -> String {
        let at = self.cursor;
        let end = self.step_right(at);
        if end <= at {
            return String::new();
        }
        let bytes = self.rope.slice_to_vec(at, end);
        if bytes == b"\r\n" {
            return "\n".to_string();
        }
        self.enc.decode(&bytes)
    }

    /// Low-level edit for undo/redo: at byte offset `at`, remove `remove` bytes
    /// then splice in `insert` **verbatim** (no re-encoding — the undo log holds
    /// the exact bytes). Does **not** touch the cursor/selection — the caller
    /// restores the caret explicitly.
    pub fn replace_range_bytes(&mut self, at: usize, remove: usize, insert: &[u8]) {
        self.rope.delete(at, remove);
        self.rope.insert(at, insert);
    }

    /// Switch the encoding view. The bytes — the document's truth — are untouched;
    /// `text()`/`render()` re-decode the same bytes through the new view instantly
    /// (the EditPad live-reinterpret). The cursor/anchor byte offsets are clamped
    /// to char boundaries of the new view so motion stays well-formed.
    pub fn set_encoding(&mut self, enc: Charset) {
        self.enc = enc;
        // The decode changes, so the offset-conversion checkpoints are no longer valid,
        // and the UTF-8 identity must be re-checked against the actual bytes (a manual
        // reinterpret can view non-UTF-8 bytes as UTF-8, where they show lossily).
        self.dec_hint.set(NO_HINT);
        self.rope_hint.set(NO_HINT);
        self.utf8_identity =
            self.enc.name() == "utf-8" && std::str::from_utf8(&self.rope.to_vec()).is_ok();
        self.build_dec_index(); // re-index for the new decode (or clear it if now identity/small)
        // A view switch can land the caret mid-character (e.g. CP437→UTF-8); snap
        // both ends back to the nearest preceding boundary.
        self.cursor = self.snap_boundary(self.cursor);
        self.anchor = self.anchor.map(|a| self.snap_boundary(a));
        self.sel_head = self.sel_head.map(|h| self.snap_boundary(h));
    }

    /// The current encoding's canonical name.
    pub fn encoding_name(&self) -> &str {
        self.enc.name()
    }

    /// The selected text, or `""` when there is no selection.
    pub fn selection_text(&self) -> String {
        match self.selection_range() {
            Some((a, b)) => self.slice(a, b),
            None => String::new(),
        }
    }

    /// The selection as an ordered `(start, end)` byte range, if any. The active end
    /// is `sel_head` when the caret has roamed off the block (persistent selection),
    /// else the cursor. Both ends are clamped to the buffer length for safety (a
    /// stale offset after an un-tracked edit can't slice out of bounds).
    fn selection_range(&self) -> Option<(usize, usize)> {
        let len = self.rope.len();
        let head = self.sel_head.unwrap_or(self.cursor).min(len);
        self.anchor.map(|a| {
            let a = a.min(len);
            if a <= head {
                (a, head)
            } else {
                (head, a)
            }
        })
    }

    // ---- defined-text ("DF") selection -------------------------------------
    //
    // The XyWrite/QN "define" selection, **auto-track**: `DF` drops an anchor and
    // turns on defining; while defining, *plain* cursor motion extends/shrinks the
    // selection live (no Shift needed — see `prepare_move`). `DF` again or `DZ`
    // closes it; `XD`/`YD` cancel. The span is the standard `ordered(anchor, cursor)`.

    /// True while a `DF` defined-text selection is open.
    pub fn is_defining(&self) -> bool {
        self.defining
    }

    /// `DF`: toggle. Opens a defined-text selection anchored at the cursor when none
    /// is open; otherwise closes the open one. Returns `true` if it opened.
    pub fn toggle_define(&mut self) -> bool {
        if self.defining {
            self.close_define();
            false
        } else {
            self.anchor = Some(self.cursor);
            self.defining = true;
            self.defined = true; // a DF block persists across motion
            true
        }
    }

    /// `DZ` / `DF`-close: freeze the open selection as a standard selection; an
    /// empty span (the caret never moved off the anchor) cancels it. No-op when not
    /// defining.
    pub fn close_define(&mut self) {
        if !self.defining {
            return;
        }
        self.defining = false;
        let head = self.sel_head.unwrap_or(self.cursor);
        if self.anchor == Some(head) {
            self.anchor = None; // empty span -> cancel
            self.sel_head = None;
            self.defined = false;
        }
        // else: keep `defined = true` — a closed block persists across motion.
    }

    /// `DM`: re-grab the current selection and resume live extending (auto-track),
    /// **without changing its boundaries** — re-anchor at the edge *farther* from the
    /// caret so the caret becomes the live active edge. For a just-closed selection
    /// (caret on an edge) this leaves the span unchanged and simply re-enables
    /// extending; for a *roamed* persistent block (caret off the block) it reshapes
    /// the block so the nearer edge follows the caret. `DM` never closes the
    /// selection (only `DZ`/`DF` do). No-op when there's no selection.
    pub fn modify_define(&mut self) {
        let Some(a) = self.anchor else {
            return;
        };
        let head = self.sel_head.unwrap_or(self.cursor);
        let (lo, hi) = if a <= head { (a, head) } else { (head, a) };
        let c = self.cursor as isize;
        // Anchor at the edge farther from the caret; the caret is the active edge.
        let far = if (c - lo as isize).abs() >= (c - hi as isize).abs() { lo } else { hi };
        self.anchor = Some(far);
        self.sel_head = None; // re-couple: the active end is the caret again
        self.defining = true; // resume live extending
        self.defined = true; // a re-grabbed block stays a defined block
    }

    /// End defining mode before an edit consumes the selection. With auto-track the
    /// span is already the standard `(anchor, cursor)` form, so this only clears the
    /// flag. No-op when not defining.
    pub fn commit_define(&mut self) {
        self.defining = false;
    }

    /// 1-based line number of the cursor.
    pub fn line(&self) -> usize {
        self.rope.byte_to_line(self.cursor) + 1
    }

    /// 0-based column of the cursor: the number of **characters** since the line
    /// start (== bytes for single-byte encodings).
    pub fn column(&self) -> usize {
        let start = self.line_start(self.cursor);
        self.enc.char_count(&self.rope.slice_to_vec(start, self.cursor))
    }

    /// Total line count (number of `\n` plus one; a trailing newline yields a
    /// final empty line).
    pub fn line_count(&self) -> usize {
        self.rope.line_count()
    }

    /// The logical line index the caret sits on (0-based). Used by the directory
    /// listing to map the caret's line back to its `DirEntry`.
    pub fn cursor_line(&self) -> usize {
        self.rope.byte_to_line(self.cursor)
    }

    /// Whole-document word count (whitespace-delimited; streamed, no full copy).
    pub fn word_count(&self) -> usize {
        self.rope.word_count()
    }

    /// Whole-document character count (decoded chars; streamed).
    pub fn char_count(&self) -> usize {
        self.rope.char_count(self.enc.is_single_byte())
    }

    /// Word count of the current selection (0 when there is no selection).
    pub fn selection_word_count(&self) -> usize {
        crate::byte_rope::count_words(self.selection_text().as_bytes())
    }

    /// Character count of the current selection (0 when there is no selection).
    pub fn selection_char_count(&self) -> usize {
        self.selection_text().chars().count()
    }

    /// The text of line `line` (0-based), without its trailing line break.
    /// Out-of-range lines return `""`.
    pub fn line_text(&self, line: usize) -> String {
        if line >= self.line_count() {
            return String::new();
        }
        let start = self.rope.line_to_byte(line);
        let end = self.line_end(start);
        self.slice(start, end)
    }

    /// The **byte range** `[start, end)` of line `line`'s content (excluding its
    /// trailing line break). Lets the highlighter map document token offsets onto
    /// a line. Out-of-range lines return `(len, len)`.
    pub fn line_byte_range(&self, line: usize) -> (usize, usize) {
        if line >= self.line_count() {
            let n = self.rope.len();
            return (n, n);
        }
        let start = self.rope.line_to_byte(line);
        (start, self.line_end(start))
    }

    /// The 0-based logical line containing byte offset `byte` (clamped to the buffer). Inverse of
    /// [`line_byte_range`](Buffer::line_byte_range)'s start; O(chunks). Lets the renderer bound a
    /// window by a BYTE budget (long-paragraph files) rather than a logical-line count.
    pub fn line_at_byte(&self, byte: usize) -> usize {
        self.rope.byte_to_line(byte.min(self.rope.len()))
    }

    // ---- mutation ----------------------------------------------------------

    /// Insert text at the cursor, replacing any selection first — **except under
    /// persistent selection**, where the selection is *kept*: the text is inserted
    /// at the caret and the block's offsets ride along so the same characters stay
    /// selected (PS-2 "don't-replace").
    pub fn insert(&mut self, s: &str) {
        let keep = self.persistent && self.has_selection();
        if keep && self.sel_head.is_none() {
            self.sel_head = Some(self.cursor); // freeze the block before editing at the caret
        }
        let at = self.cursor;
        if !keep {
            if self.sel_head.is_some() {
                self.clear_selection(); // decoupled: drop the roamed block, edit at the caret
            } else {
                self.delete_selection(); // coupled: replace the selection
            }
        }
        let ins = self.enc.encode(s);
        let n = ins.len();
        self.rope.insert(self.cursor, &ins);
        self.cursor += n;
        if keep {
            self.shift_selection_for_insert(at, n);
        }
    }

    /// Insert `s` at the cursor **without** replacing the selection; the selection
    /// rides along (the same characters stay selected). Returns the bytes inserted.
    /// Used by `CP` (copy selection to cursor) regardless of persistent-selection
    /// mode — distinct from [`insert`](Buffer::insert), which replaces unless PS is on.
    pub fn insert_keeping_selection(&mut self, s: &str) -> usize {
        if self.has_selection() && self.sel_head.is_none() {
            self.sel_head = Some(self.cursor); // freeze the block before inserting
        }
        let at = self.cursor;
        let ins = self.enc.encode(s);
        let n = ins.len();
        self.rope.insert(self.cursor, &ins);
        self.cursor += n;
        self.shift_selection_for_insert(at, n);
        n
    }

    /// Delete the character before the cursor (Backspace), or the selection. Under
    /// persistent selection the block is kept (char-delete only) and rides along.
    pub fn delete_back(&mut self) {
        let keep = self.persistent && self.has_selection();
        if keep && self.sel_head.is_none() {
            self.sel_head = Some(self.cursor);
        }
        if !keep {
            if self.sel_head.is_some() {
                self.clear_selection(); // decoupled: drop the block, char-delete at the caret
            } else if self.delete_selection() {
                return; // coupled: deleted the selection
            }
        }
        if self.cursor > 0 {
            // `step_left` steps over a whole `\r\n` so Backspace deletes BOTH bytes — otherwise a
            // line-join leaves the `\r` stranded mid-line (`msg\r $a`). For non-CRLF it's a no-op
            // (== `prev_boundary`).
            let prev = self.step_left(self.cursor);
            let m = self.cursor - prev;
            self.rope.delete(prev, m);
            self.cursor = prev;
            if keep {
                self.shift_selection_for_delete(prev, m);
            }
        }
    }

    /// Delete the character at the cursor (Delete/Del), or the selection. Under
    /// persistent selection the block is kept (char-delete only) and rides along.
    pub fn delete_forward(&mut self) {
        let keep = self.persistent && self.has_selection();
        if keep && self.sel_head.is_none() {
            self.sel_head = Some(self.cursor);
        }
        if !keep {
            if self.sel_head.is_some() {
                self.clear_selection(); // decoupled: drop the block, char-delete at the caret
            } else if self.delete_selection() {
                return; // coupled: deleted the selection
            }
        }
        if self.cursor < self.rope.len() {
            // `step_right` steps over a whole `\r\n` so Delete removes BOTH bytes — otherwise
            // deleting at a line end strips only the `\r`, leaving a bare `\n` (a mixed-ending file).
            let next = self.step_right(self.cursor);
            let m = next - self.cursor;
            self.rope.delete(self.cursor, m);
            if keep {
                self.shift_selection_for_delete(self.cursor, m);
            }
        }
    }

    /// The byte span a Backspace at `at` would remove — `[step_left(at), at)`, stepping over a
    /// whole `\r\n` / multi-byte char. Empty (`start == at`) at the start of the buffer. Used by the
    /// multi-caret delete fan-out to compute each caret's deletion char-correctly.
    pub fn delete_back_span(&self, at: usize) -> (usize, usize) {
        (self.step_left(at), at)
    }

    /// The byte span a Delete at `at` would remove — `[at, step_right(at))`. Empty at end-of-buffer.
    pub fn delete_forward_span(&self, at: usize) -> (usize, usize) {
        (at, self.step_right(at))
    }

    /// Ride-along after an insert of `n` bytes at `at`: shift the block's endpoints so
    /// the same characters stay selected. Start-gravity `<=` (insert at the start
    /// pushes the block right); end-gravity `<` (insert exactly at the end is left
    /// outside the block; insert *inside* grows it).
    fn shift_selection_for_insert(&mut self, at: usize, n: usize) {
        let (Some(a), Some(h)) = (self.anchor, self.sel_head) else {
            return;
        };
        let (lo, hi) = if a <= h { (a, h) } else { (h, a) };
        let lo2 = if at <= lo { lo + n } else { lo };
        let hi2 = if at < hi { hi + n } else { hi };
        self.anchor = Some(lo2);
        self.sel_head = Some(hi2);
    }

    /// Ride-along after deleting `m` bytes at `at`: shift the block's endpoints, and
    /// drop the selection if it collapsed to empty.
    fn shift_selection_for_delete(&mut self, at: usize, m: usize) {
        let adj = |p: usize| {
            if p <= at {
                p
            } else if p >= at + m {
                p - m
            } else {
                at
            }
        };
        self.anchor = self.anchor.map(adj);
        self.sel_head = self.sel_head.map(adj);
        if self.anchor == self.sel_head {
            self.anchor = None;
            self.sel_head = None;
            self.defined = false;
        }
    }

    /// Delete the current selection. Returns `true` if anything was deleted.
    pub fn delete_selection(&mut self) -> bool {
        if let Some((a, b)) = self.selection_range() {
            self.rope.delete(a, b - a);
            self.cursor = a;
            self.anchor = None;
            self.defining = false;
            self.sel_head = None;
            self.defined = false;
            true
        } else {
            false
        }
    }

    /// Clear any selection (without deleting text). `XD`/`YD`, and also ends any
    /// open defined-text mode.
    pub fn clear_selection(&mut self) {
        self.anchor = None;
        self.defining = false;
        self.sel_head = None;
        self.defined = false;
    }

    /// Select the whole document (`DFA`): anchor at the top, cursor at the end.
    pub fn select_all(&mut self) {
        self.defining = false;
        self.sel_head = None;
        self.anchor = Some(0);
        self.cursor = self.rope.len();
        self.defined = true; // a defined block (persists across motion)
    }

    /// Promote the current (transient) selection to a **defined block**, so it persists across
    /// plain motion / a click like a `DF`/`DW` selection rather than being cleared. Used for a
    /// mouse DRAG under persistent-selection mode. No-op with no selection.
    pub fn mark_defined(&mut self) {
        if self.anchor.is_some() {
            self.defined = true;
        }
    }

    /// Select `[start, end)` as a **defined block** (`DW`/`DL`/`DS`/`DP`): like
    /// `select_all` but over an arbitrary range. Persists across plain motion.
    pub fn define_range(&mut self, start: usize, end: usize) {
        let len = self.rope.len();
        self.defining = false;
        self.sel_head = None;
        self.anchor = Some(start.min(len));
        self.cursor = end.min(len);
        self.defined = true;
    }

    // ---- motion ------------------------------------------------------------
    //
    // Each motion takes `extend`: when true the selection anchor is established
    // (if not already) and kept, so the motion grows/shrinks a selection
    // (Shift+motion); when false the selection is cleared first.

    /// One character to the right of `at`, stepping over a whole `\r\n` break as a
    /// single unit so the caret never lands between the `\r` and the `\n`.
    /// A `\r` that is NOT the lead byte of a `\r\n` pair — a stray carriage return. The display
    /// strips every `\r`, so the caret must never *rest* on a lone CR: it would render at the same x
    /// as the following character, so stepping onto it and then off it looks like two stops at one
    /// visual spot. Horizontal motion skips these. (The `\r` of a real `\r\n` is fine to rest on —
    /// it's the end-of-line position, and the `\n` after it is what gets skipped.)
    fn is_lone_cr(&self, at: usize) -> bool {
        self.rope.byte(at) == b'\r'
            && !(at + 1 < self.rope.len() && self.rope.byte(at + 1) == b'\n')
    }

    fn step_right(&self, at: usize) -> usize {
        let len = self.rope.len();
        if at >= len {
            return len;
        }
        let mut n = if self.rope.byte(at) == b'\r' && at + 1 < len && self.rope.byte(at + 1) == b'\n'
        {
            at + 2 // a `\r\n` break is one unit
        } else {
            self.next_boundary(at)
        };
        // Never land on a stray CR (it's stripped from the display → a phantom stop); skip any run.
        while n < len && self.is_lone_cr(n) {
            n = self.next_boundary(n);
        }
        n
    }

    /// One character to the left of `at`, stepping over a whole `\r\n` break (lands
    /// at the end of the previous line's content, never inside the break).
    fn step_left(&self, at: usize) -> usize {
        if at == 0 {
            return 0;
        }
        let prev = self.prev_boundary(at);
        let mut p = if self.rope.byte(prev) == b'\n' && prev > 0 && self.rope.byte(prev - 1) == b'\r'
        {
            prev - 1 // a `\r\n` break is one unit
        } else {
            prev
        };
        // Never land on a stray CR; skip any run of them leftward.
        while p > 0 && self.is_lone_cr(p) {
            p = self.prev_boundary(p);
        }
        p
    }

    pub fn move_left(&mut self, extend: bool) {
        self.prepare_move(extend);
        self.cursor = self.step_left(self.cursor);
    }

    pub fn move_right(&mut self, extend: bool) {
        self.prepare_move(extend);
        self.cursor = self.step_right(self.cursor);
    }

    pub fn move_line_start(&mut self, extend: bool) {
        self.prepare_move(extend);
        self.cursor = self.line_start(self.cursor);
    }

    pub fn move_line_end(&mut self, extend: bool) {
        self.prepare_move(extend);
        self.cursor = self.line_end(self.cursor);
    }

    pub fn move_doc_start(&mut self, extend: bool) {
        self.prepare_move(extend);
        self.cursor = 0;
    }

    pub fn move_doc_end(&mut self, extend: bool) {
        self.prepare_move(extend);
        self.cursor = self.rope.len();
    }

    pub fn move_up(&mut self, extend: bool) {
        // Sticky goal column: keep the column from the last vertical move (so a short/blank line
        // doesn't collapse it), or capture the current one. `prepare_move` clears it; re-arm after.
        let goal = self.goal_col.unwrap_or_else(|| self.col_chars(self.cursor));
        self.prepare_move(extend);
        let line = self.rope.byte_to_line(self.cursor);
        if line == 0 {
            self.cursor = 0; // already on the first line
        } else {
            let prev_start = self.rope.line_to_byte(line - 1);
            self.cursor = self.advance_chars(prev_start, self.line_end(prev_start), goal);
        }
        self.goal_col = Some(goal); // hold the column for the next vertical move
    }

    pub fn move_down(&mut self, extend: bool) {
        let goal = self.goal_col.unwrap_or_else(|| self.col_chars(self.cursor));
        self.prepare_move(extend);
        let line = self.rope.byte_to_line(self.cursor);
        if line + 1 >= self.rope.line_count() {
            self.cursor = self.rope.len(); // already on the last line
        } else {
            let next_start = self.rope.line_to_byte(line + 1);
            self.cursor = self.advance_chars(next_start, self.line_end(next_start), goal);
        }
        self.goal_col = Some(goal); // hold the column for the next vertical move
    }

    // ---- word / paragraph / line motion (B2) -------------------------------

    /// The decoded character starting at byte `at` (`None` at end of buffer).
    fn char_at(&self, at: usize) -> Option<char> {
        if at >= self.rope.len() {
            return None;
        }
        let end = self.next_boundary(at);
        self.enc.decode(&self.rope.slice_to_vec(at, end)).chars().next()
    }

    /// `NW` — to the start of the next word (skip the current word's tail, then the
    /// following non-word run). A "word" char is alphanumeric or `_`.
    pub fn move_next_word(&mut self, extend: bool) {
        self.prepare_move(extend);
        let len = self.rope.len();
        let mut i = self.cursor;
        while i < len && self.char_at(i).is_some_and(is_word) {
            i = self.next_boundary(i);
        }
        while i < len && self.char_at(i).is_some_and(|c| !is_word(c)) {
            i = self.next_boundary(i);
        }
        self.cursor = i;
    }

    /// `PW` — to the start of the current/previous word.
    pub fn move_prev_word(&mut self, extend: bool) {
        self.prepare_move(extend);
        let mut i = self.cursor;
        while i > 0 && self.char_at(self.prev_boundary(i)).is_some_and(|c| !is_word(c)) {
            i = self.prev_boundary(i);
        }
        while i > 0 && self.char_at(self.prev_boundary(i)).is_some_and(is_word) {
            i = self.prev_boundary(i);
        }
        self.cursor = i;
    }

    /// Whether line `line` is blank (empty or whitespace only) — a paragraph break.
    fn line_is_blank(&self, line: usize) -> bool {
        self.line_text(line).trim().is_empty()
    }

    /// The first line of the paragraph containing `line` (scanning up over the
    /// paragraph's own non-blank lines).
    fn para_start_line(&self, mut line: usize) -> usize {
        while line > 0 && self.line_is_blank(line) {
            line -= 1; // a blank line: attach to the paragraph above
        }
        while line > 0 && !self.line_is_blank(line - 1) {
            line -= 1;
        }
        line
    }

    /// `NP` — to the start of the next paragraph (blank-line delimited).
    pub fn move_next_para(&mut self, extend: bool) {
        self.prepare_move(extend);
        let n = self.rope.line_count();
        let mut line = self.rope.byte_to_line(self.cursor);
        while line < n && !self.line_is_blank(line) {
            line += 1;
        }
        while line < n && self.line_is_blank(line) {
            line += 1;
        }
        self.cursor = if line < n { self.rope.line_to_byte(line) } else { self.rope.len() };
    }

    /// `PP` — to the start of the current paragraph, or the previous one if already
    /// there.
    pub fn move_prev_para(&mut self, extend: bool) {
        self.prepare_move(extend);
        let line = self.rope.byte_to_line(self.cursor);
        let start = self.para_start_line(line);
        let start_byte = self.rope.line_to_byte(start);
        if self.cursor > start_byte {
            self.cursor = start_byte; // jump to the start of this paragraph first
        } else if start == 0 {
            self.cursor = 0;
        } else {
            // Already at this paragraph's start: skip the blank gap, go to the
            // previous paragraph's start.
            let mut l = start - 1;
            while l > 0 && self.line_is_blank(l) {
                l -= 1;
            }
            self.cursor = self.rope.line_to_byte(self.para_start_line(l));
        }
    }

    /// `NS` — to the start of the next sentence.
    pub fn move_next_sentence(&mut self, extend: bool) {
        self.prepare_move(extend);
        self.cursor = self.sentence_end_from(self.cursor);
    }

    /// `PS` — to the start of the current sentence, or the previous one if already
    /// there.
    pub fn move_prev_sentence(&mut self, extend: bool) {
        self.prepare_move(extend);
        let start = self.sentence_start_at(self.cursor);
        self.cursor = if self.cursor > start {
            start // jump to the start of this sentence first
        } else if start == 0 {
            0
        } else {
            self.sentence_start_at(start - 1) // already at the start: the previous one
        };
    }

    /// `NL` — to the start of the next line.
    pub fn move_next_line_start(&mut self, extend: bool) {
        self.prepare_move(extend);
        let n = self.rope.line_count();
        let line = self.rope.byte_to_line(self.cursor);
        self.cursor =
            if line + 1 < n { self.rope.line_to_byte(line + 1) } else { self.rope.len() };
    }

    /// `PL` — to the start of the previous line (or the current line's start).
    pub fn move_prev_line_start(&mut self, extend: bool) {
        self.prepare_move(extend);
        let line = self.rope.byte_to_line(self.cursor);
        let cur_start = self.rope.line_to_byte(line);
        self.cursor = if self.cursor > cur_start {
            cur_start // not at line start yet → go there
        } else if line > 0 {
            self.rope.line_to_byte(line - 1)
        } else {
            0
        };
    }

    /// Move the caret to logical line `line` (clamped), keeping the current column.
    /// Used by screen-top/bottom (`HM`/`BS`).
    pub fn move_to_line(&mut self, line: usize, extend: bool) {
        // Sticky goal column (same as move_up/move_down) so PU/PD across short lines keep the column.
        let goal = self.goal_col.unwrap_or_else(|| self.col_chars(self.cursor));
        self.prepare_move(extend);
        let target = line.min(self.rope.line_count().saturating_sub(1));
        let start = self.rope.line_to_byte(target);
        self.cursor = self.advance_chars(start, self.line_end(start), goal);
        self.goal_col = Some(goal);
    }

    /// Move the caret `delta` logical lines (clamped), keeping the column. Used by
    /// page up/down (`PU`/`PD`); the goal column is taken once so it doesn't drift.
    pub fn move_lines(&mut self, delta: i32, extend: bool) {
        let line = self.rope.byte_to_line(self.cursor) as i64;
        let n = self.rope.line_count() as i64;
        let target = (line + delta as i64).clamp(0, (n - 1).max(0)) as usize;
        self.move_to_line(target, extend);
    }

    /// `LL` — one char left, but **never** crossing to the previous line.
    pub fn move_left_same_line(&mut self, extend: bool) {
        self.prepare_move(extend);
        if self.cursor > self.line_start(self.cursor) {
            self.cursor = self.step_left(self.cursor);
        }
    }

    /// `LR` — one char right, but **never** crossing to the next line.
    pub fn move_right_same_line(&mut self, extend: bool) {
        self.prepare_move(extend);
        if self.cursor < self.line_end(self.cursor) {
            self.cursor = self.step_right(self.cursor);
        }
    }

    /// `JMP n` — to absolute character offset `n` (clamped to the buffer).
    pub fn move_to_char(&mut self, n: usize, extend: bool) {
        self.prepare_move(extend);
        let len = self.rope.len();
        let mut i = 0usize;
        let mut chars = 0usize;
        while i < len && chars < n {
            i = self.next_boundary(i);
            chars += 1;
        }
        self.cursor = self.snap_boundary(i); // counting by char can land mid-`\r\n`; never rest there
    }

    /// Place the caret at byte offset `off` (clamped), with motion selection
    /// semantics (`extend` grows/keeps the selection; a plain move clears a
    /// transient one but keeps a defined block). Used for visual wrapped-line moves.
    pub fn move_to_offset(&mut self, off: usize, extend: bool) {
        self.prepare_move(extend);
        // Snap off a mid-`\r\n`: a visual vertical move (arrow up/down) can target the `\n` offset,
        // and while extending a selection that edge would otherwise strand a byte on delete.
        self.cursor = self.snap_boundary(off);
    }

    /// `JMP +n` / `JMP -n` — move `delta` characters from the current position
    /// (forward if positive, back if negative), stepping over `\r\n` as one. Out of
    /// range silently clamps to the start/end of the buffer (no error). Counts
    /// characters the same way the arrow keys move.
    pub fn move_by_char(&mut self, delta: i64, extend: bool) {
        self.prepare_move(extend);
        if delta >= 0 {
            for _ in 0..delta {
                if self.cursor >= self.rope.len() {
                    break;
                }
                self.cursor = self.step_right(self.cursor);
            }
        } else {
            for _ in 0..-delta {
                if self.cursor == 0 {
                    break;
                }
                self.cursor = self.step_left(self.cursor);
            }
        }
    }

    /// `RE` target — the end of the rest of the line from the cursor. If the cursor
    /// is already at the line's content end, returns the start of the next line (so
    /// `RE` there deletes the break and joins the line up).
    pub fn rest_of_line_end(&self) -> usize {
        let end = self.line_end(self.cursor);
        if self.cursor < end {
            end
        } else {
            self.step_right(self.cursor) // at EOL: eat the line break
        }
    }

    /// `RW` span — the whole word the cursor is in, plus a trailing space/tab. When
    /// the cursor isn't on a word, the run of non-word chars at the cursor (so `RW`
    /// on punctuation/spaces clears that run).
    pub fn word_span(&self) -> (usize, usize) {
        let len = self.rope.len();
        let c = self.cursor;
        let on_word = self.char_at(c).is_some_and(is_word)
            || (c > 0 && self.char_at(self.prev_boundary(c)).is_some_and(is_word));
        if on_word {
            let mut start = c;
            while start > 0 && self.char_at(self.prev_boundary(start)).is_some_and(is_word) {
                start = self.prev_boundary(start);
            }
            let mut end = c;
            while end < len && self.char_at(end).is_some_and(is_word) {
                end = self.next_boundary(end);
            }
            while end < len && self.char_at(end).is_some_and(|ch| ch == ' ' || ch == '\t') {
                end = self.next_boundary(end);
            }
            (start, end)
        } else {
            let mut end = c;
            while end < len
                && self.char_at(end).is_some_and(|ch| !is_word(ch) && ch != '\n' && ch != '\r')
            {
                end = self.next_boundary(end);
            }
            (c, end)
        }
    }

    /// `RL` span — the whole line the cursor is on, including its line break (so the
    /// line disappears). The last line (no trailing break) extends to the buffer end.
    pub fn line_span(&self) -> (usize, usize) {
        let line = self.rope.byte_to_line(self.cursor);
        let start = self.rope.line_to_byte(line);
        let n = self.rope.line_count();
        let end =
            if line + 1 < n { self.rope.line_to_byte(line + 1) } else { self.rope.len() };
        (start, end)
    }

    /// `RP` span — the whole paragraph the cursor is in, plus the following blank
    /// line(s) (blank-line delimited; see [`para_start_line`](Self::para_start_line)).
    pub fn para_span(&self) -> (usize, usize) {
        let line = self.rope.byte_to_line(self.cursor);
        let start_line = self.para_start_line(line);
        let start = self.rope.line_to_byte(start_line);
        let n = self.rope.line_count();
        let mut l = start_line;
        while l < n && !self.line_is_blank(l) {
            l += 1; // past this paragraph's content
        }
        while l < n && self.line_is_blank(l) {
            l += 1; // past the trailing blank gap
        }
        let end = if l < n { self.rope.line_to_byte(l) } else { self.rope.len() };
        (start, end)
    }

    /// `RS` span — the whole sentence the cursor is in, plus its trailing whitespace.
    pub fn sentence_span(&self) -> (usize, usize) {
        let start = self.sentence_start_at(self.cursor);
        let end = self.sentence_end_from(start);
        (start, end)
    }

    /// Drop trailing whitespace from a span (so a `define` selects content, not the
    /// gap after it).
    fn trim_trailing_ws(&self, start: usize, mut end: usize) -> (usize, usize) {
        while end > start && self.char_at(self.prev_boundary(end)).is_some_and(|c| c.is_whitespace()) {
            end = self.prev_boundary(end);
        }
        (start, end)
    }

    /// `DW` select span — the word at the cursor, **without** trailing whitespace.
    pub fn word_select_span(&self) -> (usize, usize) {
        let (s, e) = self.word_span();
        self.trim_trailing_ws(s, e)
    }

    /// The word under the caret (no surrounding whitespace), decoded — what `$WD` reports.
    /// Empty when the caret isn't on a word (e.g. on whitespace/punctuation).
    pub fn word_at(&self) -> String {
        let (s, e) = self.word_select_span();
        self.slice(s, e)
    }

    /// The span `[start, cursor)` of the word ENDING at the caret — i.e. the word the user just
    /// typed (`AR` auto-replace and the live-spell "don't underline the word being typed" both look
    /// here). Scans left over word chars (`is_word`) AND apostrophes, so a contraction stays ONE word
    /// while it's being typed — e.g. after `doesn'` the word is still `doesn'`, not the finished
    /// `doesn`, so its squiggle keeps waiting until a real separator (space/period/…) lands. `None`
    /// unless the run holds at least one real word char (a lone apostrophe/quote isn't a word).
    pub fn word_before_cursor(&self) -> Option<(usize, usize)> {
        let end = self.cursor;
        let mut start = end;
        let mut has_word = false;
        while start > 0 {
            let prev = self.prev_boundary(start);
            match self.slice(prev, start).chars().next() {
                Some(c) if is_word(c) => {
                    has_word = true;
                    start = prev;
                }
                Some('\'') => start = prev, // apostrophe: part of a contraction, not a boundary
                _ => break,
            }
        }
        (has_word && start < end).then_some((start, end))
    }

    /// `DL` select span — the whole line including its break (a movable line unit).
    pub fn line_select_span(&self) -> (usize, usize) {
        self.line_span()
    }

    /// `DS` select span — the sentence at the cursor, without trailing whitespace.
    pub fn sentence_select_span(&self) -> (usize, usize) {
        let (s, e) = self.sentence_span();
        self.trim_trailing_ws(s, e)
    }

    /// `DP` select span — the paragraph's lines (incl. internal breaks) up to the
    /// last non-blank line's content end; excludes the separating blank line(s).
    pub fn para_select_span(&self) -> (usize, usize) {
        let line = self.rope.byte_to_line(self.cursor);
        let start_line = self.para_start_line(line);
        let start = self.rope.line_to_byte(start_line);
        let n = self.rope.line_count();
        let mut l = start_line;
        while l + 1 < n && !self.line_is_blank(l + 1) {
            l += 1;
        }
        let end = self.line_end(self.rope.line_to_byte(l));
        (start, end)
    }

    /// The start of the sentence containing `pos`: the position after the most
    /// recent `.`/`!`/`?`-plus-whitespace boundary at or before `pos`.
    fn sentence_start_at(&self, pos: usize) -> usize {
        let len = self.rope.len();
        let mut i = 0;
        let mut start = 0;
        while i < pos {
            let term = self.char_at(i).is_some_and(is_sentence_term);
            let ni = self.next_boundary(i);
            if term {
                let mut j = ni;
                while j < len && self.char_at(j).is_some_and(is_closing) {
                    j = self.next_boundary(j);
                }
                if j >= len || self.char_at(j).is_some_and(|c| c.is_whitespace()) {
                    while j < len && self.char_at(j).is_some_and(|c| c.is_whitespace()) {
                        j = self.next_boundary(j);
                    }
                    if j <= pos {
                        start = j;
                    }
                }
            }
            i = ni;
        }
        start
    }

    /// The end of the sentence beginning at/after `from`: past the next
    /// `.`/`!`/`?` that is followed by whitespace/EOL, plus that trailing whitespace.
    fn sentence_end_from(&self, from: usize) -> usize {
        let len = self.rope.len();
        let mut i = from;
        while i < len {
            let term = self.char_at(i).is_some_and(is_sentence_term);
            let ni = self.next_boundary(i);
            if term {
                let mut j = ni;
                while j < len && self.char_at(j).is_some_and(is_closing) {
                    j = self.next_boundary(j);
                }
                if j >= len || self.char_at(j).is_some_and(|c| c.is_whitespace()) {
                    while j < len && self.char_at(j).is_some_and(|c| c.is_whitespace()) {
                        j = self.next_boundary(j);
                    }
                    return j;
                }
            }
            i = ni;
        }
        len
    }

    /// Establish/clear the selection before a motion.
    /// - `extend` (Shift+motion) or `defining` (DF auto-track): keep/establish the
    ///   anchor and **couple** the head to the cursor, so the motion grows the span.
    /// - persistent selection with an active block: **keep** the block and decouple —
    ///   freeze the head once (`sel_head`) so the caret roams off it.
    /// - otherwise: a plain motion drops the selection.
    fn prepare_move(&mut self, extend: bool) {
        self.goal_col = None; // a move resets the vertical goal; vertical moves re-arm it after
        if extend || self.defining {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
            self.sel_head = None; // coupled: the head follows the cursor
            if self.defining {
                self.defined = true; // building a DF block
            } else if extend {
                self.defined = false; // Shift+motion makes a *transient* selection
            }
        } else if self.anchor.is_some() && self.defined {
            // A *defined* block (DF/DZ/DFA/DM) persists across plain motion (XyWrite
            // "defined text"): the caret decouples and roams off it; only an edit or
            // `XD`/`YD` clears it. Freeze the active end once. (A transient selection
            // — Shift/mouse — falls through and collapses.)
            if self.sel_head.is_none() {
                self.sel_head = Some(self.cursor);
            }
        } else {
            self.anchor = None;
            self.sel_head = None;
            self.defined = false;
        }
    }

    // ---- character boundaries (byte offsets; driven by the encoding) -------

    /// Byte offset of the next character boundary at or after `at` (the char that
    /// starts at `at` spans `lead_len` bytes). Clamped to the buffer end.
    fn next_boundary(&self, at: usize) -> usize {
        let len = self.rope.len();
        if at >= len {
            return len;
        }
        (at + self.enc.lead_len(self.rope.byte(at))).min(len)
    }

    /// Byte offset of the previous character boundary before `at` — scan back over
    /// continuation bytes to the lead byte (at most a char's worth).
    fn prev_boundary(&self, at: usize) -> usize {
        if at == 0 {
            return 0;
        }
        let mut p = at - 1;
        while p > 0 && self.enc.is_continuation(self.rope.byte(p)) && at - p < 4 {
            p -= 1;
        }
        p
    }

    /// Snap `at` to the nearest preceding character boundary (for a live encoding
    /// switch that may land mid-character).
    fn snap_boundary(&self, at: usize) -> usize {
        let at = at.min(self.rope.len());
        if at == 0 || at == self.rope.len() {
            return at;
        }
        // Never rest BETWEEN a CR and its LF: a caret there (e.g. a click that maps to end-of-line,
        // or a search match edge) would split the `\r\n` on the next insert, stranding both bytes.
        // Snap to the end of the line's content (just before the `\r`), matching `line_end`.
        if self.rope.byte(at) == b'\n' && self.rope.byte(at - 1) == b'\r' {
            return at - 1;
        }
        if self.enc.is_continuation(self.rope.byte(at)) {
            self.prev_boundary(at)
        } else {
            at
        }
    }

    /// Number of characters from the start of `pos`'s line up to `pos`.
    fn col_chars(&self, pos: usize) -> usize {
        let start = self.line_start(pos);
        self.enc.char_count(&self.rope.slice_to_vec(start, pos))
    }

    /// The byte offset `cols` characters past `start`, clamped to `end`.
    fn advance_chars(&self, start: usize, end: usize, cols: usize) -> usize {
        let mut p = start;
        for _ in 0..cols {
            if p >= end {
                break;
            }
            p = self.next_boundary(p).min(end);
        }
        p
    }

    // ---- line geometry (byte offsets; delegated to the rope's index) -------

    /// Byte offset of the start of the line containing `pos`.
    fn line_start(&self, pos: usize) -> usize {
        self.rope.line_to_byte(self.rope.byte_to_line(pos))
    }

    /// Byte offset of the end of the line containing `pos` — just before the
    /// line's trailing break (`\n` or `\r\n`), or end-of-buffer for the last
    /// line. A trailing `\r` is trimmed so the caret sits before it, not between
    /// `\r` and `\n`.
    fn line_end(&self, pos: usize) -> usize {
        let line = self.rope.byte_to_line(pos);
        let start = self.rope.line_to_byte(line);
        let has_nl = line + 1 < self.rope.line_count();
        // For a line with a trailing newline, line_to_byte(line+1) points just
        // past the '\n', so the content ends one byte earlier; the last line has
        // no newline and ends at the buffer end.
        let mut end = if has_nl {
            self.rope.line_to_byte(line + 1) - 1
        } else {
            self.rope.len()
        };
        if end > start && self.rope.byte(end - 1) == b'\r' {
            end -= 1;
        }
        end
    }

    /// Plan an indent (`out=false`) / outdent (`out=true`) of the line(s) the caret or selection
    /// covers. `indent` is inserted at the start of each line when indenting (a tab, or `width`
    /// spaces for soft tabs); an outdent removes one leading tab OR up to `width` leading spaces.
    /// Returns `(block_start, block_end, new_text, new_cursor, new_anchor)` in ROPE offsets — the
    /// caret/anchor already mapped into the post-edit text — or `None` if nothing would change (e.g.
    /// outdent with no leading indent). PURE: does not mutate; the caller applies + records the edit.
    pub fn plan_indent(
        &self,
        out: bool,
        indent: &str,
        width: usize,
    ) -> Option<(usize, usize, String, usize, Option<usize>)> {
        let cursor = self.cursor;
        let sel = self.selection_range();
        let (region_start, region_end) = sel.unwrap_or((cursor, cursor));
        let first_ls = self.line_start(region_start);
        // Last affected line: the line containing `region_end` — but a SELECTION that ends exactly at
        // a line start (column 0) shouldn't pull that next line into the block.
        let last_probe = if sel.is_some() && region_end > first_ls && self.line_start(region_end) == region_end {
            region_end - 1
        } else {
            region_end
        };
        let block_end = self.line_end(last_probe); // end of the last line's content (before its break)
        let block = self.slice(first_ls, block_end); // decoded; internal breaks kept, no trailing break

        let mut new_block = String::with_capacity(block.len() + 8);
        let mut net: isize = 0; // total bytes added − removed (ASCII, so == rope-byte delta)
        let (mut first_added, mut first_removed) = (0usize, 0usize);
        for (i, seg) in block.split('\n').enumerate() {
            if i > 0 {
                new_block.push('\n');
            }
            let (added, removed): (usize, usize) = if out {
                let b = seg.as_bytes();
                if b.first() == Some(&b'\t') {
                    new_block.push_str(&seg[1..]);
                    (0, 1)
                } else {
                    let n = b.iter().take(width).take_while(|&&c| c == b' ').count();
                    new_block.push_str(&seg[n..]);
                    (0, n)
                }
            } else {
                // Indent: skip a truly blank line (no content beyond an optional CRLF `\r`).
                let content = seg.strip_suffix('\r').unwrap_or(seg);
                if content.is_empty() {
                    new_block.push_str(seg);
                    (0, 0)
                } else {
                    new_block.push_str(indent);
                    new_block.push_str(seg);
                    (indent.len(), 0)
                }
            };
            if i == 0 {
                first_added = added;
                first_removed = removed;
            }
            net += added as isize - removed as isize;
        }
        if new_block == block {
            return None; // nothing to do (e.g. outdent with no indentation)
        }

        // New caret / anchor (rope offsets in the post-edit text).
        let (new_cursor, new_anchor) = if sel.is_some() {
            // Re-select the whole reflowed block (anchor at its start, caret at its end).
            let block_len = (block_end as isize - first_ls as isize + net) as usize;
            (first_ls + block_len, Some(first_ls))
        } else {
            // No selection: only the caret's own (first) line changed. Shift the caret by that line's
            // leading-whitespace change; an outdent that ate past the caret clamps it to the line start.
            let c = if out {
                if cursor >= first_ls + first_removed {
                    cursor - first_removed
                } else {
                    first_ls
                }
            } else {
                cursor + first_added
            };
            (c, None)
        };
        Some((first_ls, block_end, new_block, new_cursor, new_anchor))
    }

    /// The matching-bracket result for a caret at `pos` (`BALPAIRS`). The bracket AT `pos` (to the
    /// right of the caret) is considered first, then the one just BEFORE it. `()`, `[]`, `{}` are
    /// matched nesting-aware for the SAME pair type; the scan is bounded (`CAP` bytes each way) so a
    /// huge file can't cost an unbounded walk. Operates on rope bytes — brackets are ASCII in every
    /// supported encoding.
    pub fn bracket_match(&self, pos: usize) -> BalMatch {
        const CAP: usize = 200_000;
        let len = self.rope.len();
        let is_bracket = |b: u8| matches!(b, b'(' | b'[' | b'{' | b')' | b']' | b'}');
        let is_open = |b: u8| matches!(b, b'(' | b'[' | b'{');
        let partner = |b: u8| match b {
            b'(' => b')',
            b'[' => b']',
            b'{' => b'}',
            b')' => b'(',
            b']' => b'[',
            b'}' => b'{',
            _ => 0,
        };
        // The bracket the caret is "on": the char at `pos`, else the char just before it.
        let (bp, ch) = if pos < len && is_bracket(self.rope.byte(pos)) {
            (pos, self.rope.byte(pos))
        } else if pos > 0 && is_bracket(self.rope.byte(pos - 1)) {
            (pos - 1, self.rope.byte(pos - 1))
        } else {
            return BalMatch::None;
        };
        let want = partner(ch);
        let mut depth = 1usize;
        if is_open(ch) {
            let end = (bp + 1 + CAP).min(len);
            let mut i = bp + 1;
            while i < end {
                let b = self.rope.byte(i);
                if b == ch {
                    depth += 1;
                } else if b == want {
                    depth -= 1;
                    if depth == 0 {
                        return BalMatch::Matched(bp, i);
                    }
                }
                i += 1;
            }
        } else {
            let stop = bp.saturating_sub(CAP);
            let mut i = bp;
            while i > stop {
                i -= 1;
                let b = self.rope.byte(i);
                if b == ch {
                    depth += 1;
                } else if b == want {
                    depth -= 1;
                    if depth == 0 {
                        return BalMatch::Matched(i, bp);
                    }
                }
            }
        }
        BalMatch::Unmatched(bp, ch)
    }

    /// The leading whitespace (spaces/tabs) of the line containing `pos`, from the line start UP TO
    /// `pos` — the string an auto-indent copies onto the next line when `ENTER` splits here.
    pub fn indent_at(&self, pos: usize) -> String {
        let ls = self.line_start(pos);
        self.slice(ls, pos).chars().take_while(|&c| c == ' ' || c == '\t').collect()
    }

    /// The current line's indentation in COLUMNS (`$INDENTED`): leading spaces count 1 each, a tab
    /// advances to the next multiple of `tab_width` (tab stops), stopping at the first non-whitespace.
    pub fn indent_columns(&self, tab_width: usize) -> usize {
        let tw = tab_width.max(1);
        let ls = self.line_start(self.cursor);
        let mut col = 0;
        for c in self.slice(ls, self.line_end(self.cursor)).chars() {
            match c {
                ' ' => col += 1,
                '\t' => col = (col / tw + 1) * tw,
                _ => break,
            }
        }
        col
    }

    /// Render the buffer with a cursor marker, for the bare display and tests.
    /// The selection (if any) is bracketed with `[` … `]`. For single-byte
    /// encodings the decoded char index equals the byte offset, so markers placed
    /// by char index land on the right byte.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let sel = self.selection_range();
        let bytes = self.rope.to_vec();
        let len = bytes.len();
        // Walk character boundaries by *byte* offset, so markers (which are byte
        // offsets on char boundaries) land correctly for multi-byte encodings.
        let mut bi = 0;
        while bi < len {
            if let Some((a, b)) = sel {
                if bi == a {
                    out.push('[');
                }
                if bi == b {
                    out.push(']');
                }
            }
            if bi == self.cursor {
                out.push('‸');
            }
            let next = self.next_boundary(bi);
            out.push_str(&self.enc.decode(&bytes[bi..next]));
            bi = next;
        }
        // Trailing markers at end-of-buffer.
        if let Some((_, b)) = sel {
            if b == len {
                out.push(']');
            }
        }
        if self.cursor == len {
            out.push('‸');
        }
        out
    }

    /// Search the decoded text for `pattern`. Forward: the first match whose start
    /// is `>= from` (a rope offset). Backward: the last match whose start is
    /// `< from`. Returns the match span as **rope** offsets, or `None`. A single
    /// directional pass — wrap-around is the caller's policy.
    pub fn search(&self, pattern: &Pattern, from: usize, forward: bool) -> Option<(usize, usize)> {
        let text = self.text();
        let from_dec = self.decoded_offset(from);
        let (s, e) = pattern.find_dir(&text, from_dec, forward)?;
        Some((self.rope_offset(s), self.rope_offset(e)))
    }

    /// Every forward match of `pattern` with start in the rope range `[lo, hi)`, as `(start, end)`
    /// rope offsets, up to `cap` hits. Unlike calling [`search`](Self::search) in a loop, this
    /// materializes the document text ONCE and scans it — O(n) total, not O(n) per match — which
    /// matters for the FIND dialog's live highlight-all (a common letter can hit thousands of times).
    pub fn find_all(&self, pattern: &Pattern, lo: usize, hi: usize, cap: usize) -> Vec<(usize, usize)> {
        let text = self.text(); // one allocation, reused across every match
        let hi_dec = self.decoded_offset(hi);
        let from = self.decoded_offset(lo);
        // A SINGLE forward pass. `find_all_bounded` walks the matcher once (the regex `find_iter` /
        // the literal `find_forward` both advance from the previous match) — NOT `find_dir` in a loop,
        // whose regex arm restarts `find_iter` from offset 0 on every call (O(matches × n) = O(n²);
        // it froze a big GREP+EMPH results tab for ~0.8 s). Matches come in order, so stop at `hi`.
        pattern
            .find_all_bounded(&text, from, cap)
            .into_iter()
            .take_while(|&(_, e)| e <= hi_dec)
            .map(|(s, e)| (self.rope_offset(s), self.rope_offset(e)))
            .collect()
    }
}

/// Options for literal matching.
#[derive(Debug, Clone, Copy)]
pub struct SearchOpts {
    /// Match case exactly (`/A`). Default search is case-insensitive.
    pub case_sensitive: bool,
    /// Match only whole words (`/W`) — the match must be bounded by non-word
    /// characters (a "word" char is alphanumeric or `_`).
    pub whole_word: bool,
}

/// A compiled search pattern: a hand-rolled literal matcher (CRLF-aware, Unicode
/// case-folding) or a `regex::Regex` (the `/R` flag). Built once per command and
/// reused for find + replace so the regex compiles a single time.
pub enum Pattern {
    Literal { needle: String, case_sensitive: bool, whole_word: bool },
    Regex(regex::Regex),
}

impl Pattern {
    /// Build a pattern from a search request. `regex` selects the engine; `/A`
    /// (case_sensitive) and `/W` (whole_word) apply to both. Returns the regex
    /// compile error message on a bad `/R` pattern.
    pub fn build(
        needle: &str,
        case_sensitive: bool,
        whole_word: bool,
        regex: bool,
    ) -> Result<Pattern, String> {
        if regex {
            // `/W` wraps the user's regex in word boundaries; `/A` toggles case.
            let body =
                if whole_word { format!(r"\b(?:{needle})\b") } else { needle.to_string() };
            regex::RegexBuilder::new(&body)
                .case_insensitive(!case_sensitive)
                .build()
                .map(Pattern::Regex)
                .map_err(|e| format!("invalid regex: {e}"))
        } else {
            Ok(Pattern::Literal {
                needle: needle.to_string(),
                case_sensitive,
                whole_word,
            })
        }
    }

    fn opts(case_sensitive: bool, whole_word: bool) -> SearchOpts {
        SearchOpts { case_sensitive, whole_word }
    }

    /// The first match with start `>= from` (forward) or the last with start
    /// `< from` (backward), as decoded-string byte offsets.
    fn find_dir(&self, text: &str, from: usize, forward: bool) -> Option<(usize, usize)> {
        match self {
            Pattern::Literal { needle, case_sensitive, whole_word } => {
                if needle.is_empty() {
                    return None;
                }
                let opts = Self::opts(*case_sensitive, *whole_word);
                if forward {
                    find_forward(text, needle, from, opts)
                } else {
                    find_backward(text, needle, from, opts)
                }
            }
            Pattern::Regex(re) => {
                if forward {
                    re.find_iter(text).find(|m| m.start() >= from).map(|m| (m.start(), m.end()))
                } else {
                    re.find_iter(text)
                        .take_while(|m| m.start() < from)
                        .last()
                        .map(|m| (m.start(), m.end()))
                }
            }
        }
    }

    /// Every non-overlapping match with start `>= from`, as decoded-string spans.
    pub fn find_all_from(&self, text: &str, from: usize) -> Vec<(usize, usize)> {
        match self {
            Pattern::Literal { needle, case_sensitive, whole_word } => {
                find_all(text, needle, from, Self::opts(*case_sensitive, *whole_word))
            }
            Pattern::Regex(re) => re
                .find_iter(text)
                .filter(|m| m.start() >= from)
                .map(|m| (m.start(), m.end()))
                .collect(),
        }
    }

    /// Up to `cap` non-overlapping matches with start `>= from`, as decoded-string spans, in a SINGLE
    /// forward pass (both the regex `find_iter` and the literal `find_forward` advance from the last
    /// match — never restart from 0). This is the O(n) primitive [`Buffer::find_all`] must use;
    /// calling [`find_dir`](Self::find_dir) per match instead makes the regex path O(n²).
    pub fn find_all_bounded(&self, text: &str, from: usize, cap: usize) -> Vec<(usize, usize)> {
        match self {
            Pattern::Literal { needle, case_sensitive, whole_word } => {
                if needle.is_empty() {
                    return Vec::new();
                }
                let opts = Self::opts(*case_sensitive, *whole_word);
                let mut out = Vec::new();
                let mut pos = from;
                while out.len() < cap {
                    match find_forward(text, needle, pos, opts) {
                        Some((s, e)) => {
                            out.push((s, e));
                            pos = if e > s { e } else { e + 1 }; // step over a zero-width match
                        }
                        None => break,
                    }
                }
                out
            }
            Pattern::Regex(re) => re
                .find_iter(text)
                .filter(|m| m.start() >= from)
                .take(cap)
                .map(|m| (m.start(), m.end()))
                .collect(),
        }
    }

    /// The replacement text for a single, already-matched substring `matched`
    /// (used by interactive change-verify, which replaces one match at a time).
    /// For a regex this expands capture references against `matched`; for a literal
    /// pattern the replacement is returned verbatim.
    pub fn expand_match(&self, matched: &str, replacement: &str) -> String {
        match self {
            Pattern::Literal { .. } => replacement.to_string(),
            Pattern::Regex(re) => {
                let mut out = String::new();
                match re.captures(matched) {
                    Some(c) => c.expand(replacement, &mut out),
                    None => out.push_str(replacement),
                }
                out
            }
        }
    }

    /// Build the replaced text for every match at/after decoded offset `from`.
    /// Returns `(span_start, span_end, new_region, count)` in decoded offsets — the
    /// caller splices `new_region` over `[span_start, span_end)` — or `None` when
    /// there are no matches. For a regex, `replacement` may use capture references
    /// (`$1`, `${name}`, `$0`; `$$` for a literal `$`); for a literal pattern the
    /// replacement is inserted verbatim.
    /// Replace matches at/after byte offset `from`. `limit = Some(n)` stops after `n` replacements
    /// (`CH/n`); `None` replaces all. Returns `(first_start, last_end, replaced_region, count)`, or
    /// `None` if there are no matches.
    /// With `keep_case`, each replacement is recased to match the CASE of the text it replaces (lower /
    /// Capitalized / UPPER), via [`crate::host::key_case`]/[`crate::host::apply_key_case`] — the `/K`
    /// switch. For a regex, captures are expanded first and the whole result is recased to the whole
    /// match's case.
    pub fn replace_all(
        &self,
        text: &str,
        replacement: &str,
        from: usize,
        limit: Option<usize>,
        keep_case: bool,
    ) -> Option<(usize, usize, String, usize)> {
        // Recase `replacement` (or an already-expanded regex result) to match `matched`'s case.
        let cased = |matched: &str, repl: &str| -> String {
            crate::host::apply_key_case(crate::host::key_case(matched), repl)
        };
        let cap = limit.unwrap_or(usize::MAX);
        match self {
            Pattern::Literal { .. } => {
                let mut matches = self.find_all_from(text, from);
                matches.truncate(cap);
                let (&(fs, _), &(_, le)) = (matches.first()?, matches.last()?);
                let mut region = String::new();
                let mut pos = fs;
                for &(s, e) in &matches {
                    region.push_str(&text[pos..s]);
                    if keep_case {
                        region.push_str(&cased(&text[s..e], replacement));
                    } else {
                        region.push_str(replacement);
                    }
                    pos = e;
                }
                Some((fs, le, region, matches.len()))
            }
            Pattern::Regex(re) => {
                let caps: Vec<regex::Captures> = re
                    .captures_iter(text)
                    .filter(|c| c.get(0).unwrap().start() >= from)
                    .take(cap)
                    .collect();
                let fs = caps.first()?.get(0).unwrap().start();
                let le = caps.last()?.get(0).unwrap().end();
                let mut region = String::new();
                let mut pos = fs;
                for c in &caps {
                    let m = c.get(0).unwrap();
                    region.push_str(&text[pos..m.start()]);
                    if keep_case {
                        let mut expanded = String::new();
                        c.expand(replacement, &mut expanded); // $1, ${name}, $$ → $
                        region.push_str(&cased(m.as_str(), &expanded));
                    } else {
                        c.expand(replacement, &mut region);
                    }
                    pos = m.end();
                }
                Some((fs, le, region, caps.len()))
            }
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A sentence-terminating punctuation mark.
fn is_sentence_term(c: char) -> bool {
    matches!(c, '.' | '!' | '?')
}

/// A closing quote/bracket that may trail a sentence terminator (`it ended."`).
fn is_closing(c: char) -> bool {
    matches!(c, '"' | '\'' | ')' | ']' | '}' | '»' | '”' | '’')
}

/// Two chars equal under the active case policy.
fn chars_eq(a: char, b: char, case_sensitive: bool) -> bool {
    a == b || (!case_sensitive && a.to_lowercase().eq(b.to_lowercase()))
}

/// The match is bounded by non-word chars on both sides (or the text edge).
fn is_whole_word(text: &str, start: usize, end: usize) -> bool {
    let before_ok = text[..start].chars().next_back().is_none_or(|c| !is_word(c));
    let after_ok = text[end..].chars().next().is_none_or(|c| !is_word(c));
    before_ok && after_ok
}

/// If `pattern` matches `text` starting at byte `i`, return the match's end byte
/// offset in `text` (which may differ from `i + pattern.len()` under
/// case-insensitive folding or a `\n`→`\r\n` line-ending match). Else `None`.
/// `i` must be a char boundary.
///
/// A `\n` in the pattern matches a whole line ending — `\r\n` **or** `\n` — so a
/// search like `t\n` finds an end-of-line `t` regardless of the file's CR/LF
/// convention (our documents keep CRLF byte-exact, but treat it as one break).
fn match_at(text: &str, i: usize, pattern: &str, case_sensitive: bool) -> Option<usize> {
    let bytes = text.as_bytes();
    let len = text.len();
    let mut pos = i;
    for pch in pattern.chars() {
        if pch == '\n' {
            if pos + 1 < len && bytes[pos] == b'\r' && bytes[pos + 1] == b'\n' {
                pos += 2; // CRLF — consume the whole break
            } else if pos < len && bytes[pos] == b'\n' {
                pos += 1; // bare LF
            } else {
                return None;
            }
        } else {
            let hch = text[pos..].chars().next()?;
            if !chars_eq(hch, pch, case_sensitive) {
                return None;
            }
            pos += hch.len_utf8();
        }
    }
    Some(pos)
}

fn step(text: &str, i: usize) -> usize {
    text[i..].chars().next().map_or(1, |c| c.len_utf8())
}

fn find_forward(text: &str, pat: &str, from: usize, opts: SearchOpts) -> Option<(usize, usize)> {
    let mut i = from.min(text.len());
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    while i < text.len() {
        if let Some(end) = match_at(text, i, pat, opts.case_sensitive) {
            if !opts.whole_word || is_whole_word(text, i, end) {
                return Some((i, end));
            }
        }
        i += step(text, i);
    }
    None
}

fn find_backward(text: &str, pat: &str, from: usize, opts: SearchOpts) -> Option<(usize, usize)> {
    let limit = from.min(text.len());
    let mut best = None;
    let mut i = 0usize;
    while i < limit {
        if let Some(end) = match_at(text, i, pat, opts.case_sensitive) {
            if !opts.whole_word || is_whole_word(text, i, end) {
                best = Some((i, end));
            }
        }
        i += step(text, i);
    }
    best
}

/// Every non-overlapping match (decoded byte spans) at or after `from`.
pub(crate) fn find_all(
    text: &str,
    pat: &str,
    from: usize,
    opts: SearchOpts,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if pat.is_empty() {
        return out;
    }
    let mut i = from.min(text.len());
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    while i < text.len() {
        if let Some(end) = match_at(text, i, pat, opts.case_sensitive) {
            if !opts.whole_word || is_whole_word(text, i, end) {
                out.push((i, end));
                i = if end > i { end } else { i + step(text, i) };
                continue;
            }
        }
        i += step(text, i);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{cp437, latin1, Encoding};

    #[test]
    fn multicaret_overlay_add_toggle_collapse_normalize() {
        let mut b = Buffer::from_str("hello world foo bar");
        // Starts single-caret: no secondaries, the overlay is inert.
        assert!(!b.is_multi());
        assert_eq!(b.caret_count(), 1);
        b.set_caret(0, None); // primary at offset 0
        // Add two secondary carets (CARETADD).
        assert_eq!(b.add_caret(6), 2);
        assert_eq!(b.add_caret(12), 3);
        assert!(b.is_multi());
        assert_eq!(b.caret_positions(), vec![0, 6, 12]); // primary + secondaries, sorted
        // CARETADD onto an existing caret toggles it off.
        assert_eq!(b.add_caret(6), 2);
        assert_eq!(b.caret_positions(), vec![0, 12]);
        // Building-state flag.
        assert!(!b.building());
        b.set_building(true);
        assert!(b.building());
        // A secondary coincident with the primary folds away on normalize.
        b.add_caret(0);
        assert_eq!(b.caret_count(), 3);
        b.normalize_carets();
        assert_eq!(b.caret_count(), 2);
        assert_eq!(b.caret_positions(), vec![0, 12]);
        // CARETONE collapses to the primary and ends building.
        b.collapse_carets();
        assert!(!b.is_multi());
        assert_eq!(b.caret_count(), 1);
        assert!(!b.building());
    }

    #[test]
    fn move_all_carets_moves_all_normally_but_primary_only_while_building() {
        let mut b = Buffer::from_str("abcdef");
        b.set_caret(0, None); // primary at 0
        b.add_caret(2);
        b.add_caret(4);
        // Normal: every caret moves right.
        b.move_all_carets(|bb| bb.move_right(false));
        assert_eq!(b.caret_positions(), vec![1, 3, 5]);
        // Building: only the primary moves; secondaries sit still (so the keyboard can plant more).
        b.set_building(true);
        b.move_all_carets(|bb| bb.move_right(false)); // primary 1->2; secondaries 3,5 stay
        assert_eq!(b.caret_positions(), vec![2, 3, 5]);
    }

    #[test]
    fn regex_find_all_over_many_matches_is_linear() {
        // Regression guard for the O(n²) `find_all` (its `find_dir` regex arm restarted `find_iter`
        // from offset 0 on every match — a big GREP+EMPH results tab froze ~0.8 s). A dense pattern
        // that hits the cap forces the pathological path: `cap` matches, each of which the old code
        // re-scanned all preceding matches to reach. With the single-pass fix this is trivially fast;
        // if the quadratic returns, this test crawls (12.5M match ops for a 5000 cap).
        let mut s = String::new();
        for _ in 0..6000 {
            s.push_str("self here\n");
        }
        let buf = Buffer::from_str(&s);
        let pat = Pattern::build("self", false, false, true).unwrap(); // /R regex, like GREP→EMPH
        let hits = buf.find_all(&pat, 0, buf.len(), 5000);
        assert_eq!(hits.len(), 5000, "capped at 5000");
        // In order, non-overlapping, each spanning exactly the 4-byte match.
        assert!(hits.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(hits.iter().all(|&(a, b)| b - a == 4));
    }

    #[test]
    fn encoding_offset_conversions_are_direction_independent() {
        // CP437 high bytes decode to multi-byte UTF-8, so rope and decoded offsets
        // diverge. The checkpoint memo must give identical results whether queried
        // forward (PgDn) or backward (PgUp) — verified against a hint-free reference.
        let mut bytes = Vec::new();
        for i in 0..400u32 {
            bytes.push((b'a' as u32 + (i % 26)) as u8);
            if i % 3 == 0 {
                bytes.push(0xAE); // « → 2 UTF-8 bytes
            }
            if i % 7 == 0 {
                bytes.push(0xE0); // α → 2 UTF-8 bytes
            }
        }
        let buf = Buffer::from_bytes(bytes.clone(), cp437());
        let rlen = bytes.len();
        let dlen = cp437().decode(&bytes).len();
        let ref_dec = |off: usize| cp437().decode(&bytes[0..off]).len();
        let ref_rope = |dec: usize| {
            let (mut rp, mut dp) = (0usize, 0usize);
            while rp < rlen && dp < dec {
                let n = rp + 1; // cp437 is single-byte: every offset is a boundary
                dp += cp437().decode(&bytes[rp..n]).len();
                rp = n;
            }
            rp
        };
        // decoded_offset: forward sweep, then backward, then scattered.
        for off in 0..=rlen {
            assert_eq!(buf.decoded_offset(off), ref_dec(off), "fwd d2d {off}");
        }
        for off in (0..=rlen).rev() {
            assert_eq!(buf.decoded_offset(off), ref_dec(off), "bwd d2d {off}");
        }
        for &off in &[rlen, 0, rlen / 2, 5, rlen - 1, 100, 3] {
            assert_eq!(buf.decoded_offset(off), ref_dec(off), "rand d2d {off}");
        }
        // rope_offset: forward sweep then backward.
        for dec in 0..=dlen {
            assert_eq!(buf.rope_offset(dec), ref_rope(dec), "fwd r2o {dec}");
        }
        for dec in (0..=dlen).rev() {
            assert_eq!(buf.rope_offset(dec), ref_rope(dec), "bwd r2o {dec}");
        }
        // round-trip on every boundary, after the hints have been left mid-sweep.
        for off in 0..=rlen {
            assert_eq!(buf.rope_offset(buf.decoded_offset(off)), off, "roundtrip {off}");
        }
    }

    #[test]
    fn insert_advances_cursor() {
        let mut b = Buffer::new();
        b.insert("hi");
        assert_eq!(b.text(), "hi");
        assert_eq!(b.cursor(), 2);
    }

    #[test]
    fn insert_in_middle() {
        let mut b = Buffer::from_str("ac");
        b.move_left(false); // between a and c
        b.insert("b");
        assert_eq!(b.text(), "abc");
        assert_eq!(b.cursor(), 2);
    }

    #[test]
    fn backspace_and_delete() {
        let mut b = Buffer::from_str("abc");
        b.delete_back(); // removes 'c'
        assert_eq!(b.text(), "ab");
        b.move_doc_start(false);
        b.delete_forward(); // removes 'a'
        assert_eq!(b.text(), "b");
        assert_eq!(b.cursor(), 0);
    }

    #[test]
    fn horizontal_motion_clamps() {
        let mut b = Buffer::from_str("ab");
        b.move_doc_start(false);
        b.move_left(false); // clamp at 0
        assert_eq!(b.cursor(), 0);
        b.move_doc_end(false);
        b.move_right(false); // clamp at len
        assert_eq!(b.cursor(), 2);
    }

    #[test]
    fn line_and_column() {
        let b = Buffer::from_str("ab\ncde");
        // cursor at end (after 'e'): line 2, column 3.
        assert_eq!(b.line(), 2);
        assert_eq!(b.column(), 3);
        assert_eq!(b.line_count(), 2);
    }

    #[test]
    fn vertical_motion_keeps_column() {
        let mut b = Buffer::from_str("abcd\nef");
        // Start at end (line 2, col 2). Up -> line 1, col 2 (clamped to len 4).
        b.move_up(false);
        assert_eq!((b.line(), b.column()), (1, 2));
        b.move_down(false);
        assert_eq!((b.line(), b.column()), (2, 2));
    }

    #[test]
    fn line_start_end() {
        let mut b = Buffer::from_str("hello\nworld");
        b.move_line_start(false);
        assert_eq!(b.column(), 0);
        b.move_line_end(false);
        assert_eq!(b.column(), 5);
    }

    #[test]
    fn horizontal_motion_skips_crlf_as_one_unit() {
        // "ab\r\ncd": a0 b1 \r2 \n3 c4 d5. The caret must never land on the \n (3).
        let mut b = Buffer::from_str("ab\r\ncd");
        b.move_doc_start(false); // 0
        b.move_right(false); // 1 (after 'a')
        b.move_right(false); // 2 (after 'b' = end of line 0, on the \r)
        assert_eq!(b.cursor(), 2);
        b.move_right(false); // skip \r\n -> 4 (start of line 1)
        assert_eq!(b.cursor(), 4);
        b.move_left(false); // back over \r\n -> 2 (end of line 0)
        assert_eq!(b.cursor(), 2);
        b.move_left(false); // 1
        assert_eq!(b.cursor(), 1);
    }

    #[test]
    fn move_to_offset_snaps_out_of_crlf() {
        // Visual vertical motion (arrow up/down) targets a rope offset that can be the `\n` of a
        // `\r\n`; `move_to_offset` must snap it so a defining selection's edge never lands mid-break.
        let mut b = Buffer::from_str("ab\r\ncd");
        b.move_to_offset(3, false); // the \n (mid-CRLF)
        assert_eq!(b.cursor(), 2);
        // Extending to it leaves the selection edge at end-of-content, so deleting won't strand a byte.
        let mut b = Buffer::from_str("ab\r\ncd");
        b.set_caret(0, None);
        b.move_to_offset(3, true); // extend toward the \n → snaps to 2
        assert_eq!(b.selection_span(), Some((0, 2)));
    }

    #[test]
    fn caret_never_rests_between_cr_and_lf() {
        // "ab\r\ncd": a0 b1 \r2 \n3 c4 d5. Placing the caret on the \n (offset 3, mid-CRLF) — as a
        // click at end-of-line maps to — must snap to end-of-line content (offset 2, the \r), so the
        // next insert can't split the break and strand the bytes.
        let mut b = Buffer::from_str("ab\r\ncd");
        b.set_caret(3, None);
        assert_eq!(b.cursor(), 2);
        b.insert("X");
        assert_eq!(b.bytes(), b"abX\r\ncd"); // \r\n stays intact, no stray bytes

        // click_caret (the mouse path) snaps the same way.
        let mut b = Buffer::from_str("ab\r\ncd");
        b.click_caret(3);
        assert_eq!(b.cursor(), 2);

        // A caret ON the \r (offset 2, end-of-content) is already valid — left alone.
        let mut b = Buffer::from_str("ab\r\ncd");
        b.set_caret(2, None);
        assert_eq!(b.cursor(), 2);
    }

    #[test]
    fn horizontal_motion_skips_a_stray_cr() {
        // "ab\rcd": a0 b1 \r2 c3 d4 — a LONE CR (no LF). The display strips the \r, so the caret must
        // hop over it in one step, never resting on the \r (which would render at the same x as 'c').
        let mut b = Buffer::from_str("ab\rcd");
        b.move_doc_start(false); // 0
        b.move_right(false); // 1 (after 'a')
        assert_eq!(b.cursor(), 1);
        b.move_right(false); // skip the stray \r at 2 → land on 3 (before 'c'), not 2
        assert_eq!(b.cursor(), 3);
        b.move_left(false); // back over the stray \r → 1, not 2
        assert_eq!(b.cursor(), 1);

        // A run of stray CRs is skipped as one hop too. "a\r\r\rb": a0 \r1 \r2 \r3 b4. The CRs strip
        // away, so the display is just "ab"; the caret hops a↔b directly across the whole run.
        let mut b = Buffer::from_str("a\r\r\rb");
        b.move_doc_start(false); // 0 (before 'a')
        b.move_right(false); // skip "\r\r\r" → 4 (before 'b')
        assert_eq!(b.cursor(), 4);
        b.move_left(false); // back over the run → 0 (before 'a')
        assert_eq!(b.cursor(), 0);
    }

    #[test]
    fn line_end_strips_crlf() {
        // On a CRLF line, end-of-line sits before the '\r', not between \r and \n.
        let mut b = Buffer::from_str("hello\r\nworld");
        b.move_doc_start(false);
        b.move_line_end(false);
        assert_eq!(b.column(), 5); // before '\r'
        assert_eq!(b.cursor(), 5);
    }

    #[test]
    fn utf8_motion_and_columns_are_char_based() {
        // Default is UTF-8: "café" is 5 bytes but 4 characters.
        let mut b = Buffer::from_str("café");
        assert_eq!(b.len(), 5); // bytes (é = 2)
        assert_eq!(b.column(), 4); // chars (cursor at end)
        assert_eq!(b.bytes(), vec![b'c', b'a', b'f', 0xC3, 0xA9]);
        b.move_left(false); // step over 'é' (2 bytes) in one move
        assert_eq!(b.cursor(), 3);
        assert_eq!(b.column(), 3);
        b.delete_forward(); // deletes the whole 'é'
        assert_eq!(b.text(), "caf");
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn utf8_default_makes_any_char_typeable() {
        // The authoring win: em-dash / smart quotes / emoji are no longer '?'.
        let mut b = Buffer::new();
        b.insert("a—“”😀");
        assert_eq!(b.text(), "a—“”😀");
    }

    #[test]
    fn single_byte_view_substitutes_question_mark() {
        // A single-byte (Latin-1) view still substitutes '?' for what it can't
        // represent — the legacy/encoded-file behavior.
        let mut b = Buffer::from_bytes(b"a".to_vec(), latin1());
        b.move_doc_end(false);
        b.insert("—b"); // em-dash not in Latin-1
        assert_eq!(b.text(), "a?b");
    }

    #[test]
    fn utf8_vertical_motion_preserves_char_column() {
        // Char column 3 on a 2-byte-per-char line maps to the right byte offset
        // on the ASCII line below.
        let mut b = Buffer::from_str("áéíóú\nxyzw");
        b.move_doc_start(false);
        for _ in 0..3 {
            b.move_right(false);
        }
        assert_eq!(b.column(), 3);
        assert_eq!(b.cursor(), 6); // 3 chars × 2 bytes
        b.move_down(false);
        assert_eq!(b.column(), 3); // same char column on "xyzw"
        assert_eq!(b.cursor(), 14); // byte offset of 'w'
    }

    #[test]
    fn from_bytes_decodes_via_encoding() {
        // The XyWrite function-code markers decode as guillemets under CP437.
        let b = Buffer::from_bytes(vec![0xAE, 0x46, 0x43, 0xAF], cp437());
        assert_eq!(b.text(), "«FC»");
        assert_eq!(b.len(), 4);
    }

    #[test]
    fn set_encoding_reinterprets_same_bytes_in_place() {
        // The same two bytes view differently under Latin-1 vs CP437; switching
        // is a live re-decode that never touches the bytes or the cursor.
        let mut b = Buffer::from_bytes(vec![0xAE, 0xAF], latin1());
        assert_eq!(b.text(), "®¯"); // Latin-1
        b.move_doc_end(false);
        let cur = b.cursor();
        b.set_encoding(cp437());
        assert_eq!(b.text(), "«»"); // CP437, same bytes
        assert_eq!(b.bytes(), vec![0xAE, 0xAF]); // bytes untouched
        assert_eq!(b.cursor(), cur); // byte-offset cursor unmoved
        assert_eq!(b.encoding_name(), "cp437");
    }

    #[test]
    fn decoded_offset_maps_across_single_byte_view() {
        // CP437 byte 0xCB decodes to '╦' (3 UTF-8 bytes); rope and decoded offsets
        // diverge. The GUI must translate or it slices the decoded string mid-char.
        let b = Buffer::from_bytes(vec![0xCB, 0xCB], cp437());
        assert_eq!(b.text(), "╦╦");
        assert_eq!(b.decoded_offset(0), 0);
        assert_eq!(b.decoded_offset(1), 3);
        assert_eq!(b.decoded_offset(2), 6);
        // Inverse round-trips.
        assert_eq!(b.rope_offset(0), 0);
        assert_eq!(b.rope_offset(3), 1);
        assert_eq!(b.rope_offset(6), 2);
    }

    #[test]
    fn decoded_offset_is_identity_for_utf8() {
        let b = Buffer::from_str("héllo"); // 'é' is 2 UTF-8 bytes; cursor at 6
        assert_eq!(b.decoded_offset(6), 6);
        assert_eq!(b.rope_offset(6), 6);
        assert_eq!(b.decoded_offset(1), 1);
    }

    #[test]
    fn dec_index_bounds_offset_walks_exactly() {
        // A >1MB single-byte (CP437) buffer builds the sparse index; every mapping must still equal
        // the naive whole-prefix decode, from any starting checkpoint, in both directions.
        let n = 1_200_000usize;
        let b = Buffer::from_bytes(vec![0xCB; n], cp437()); // each 0xCB → '╦' (3 UTF-8 bytes)
        assert!(!b.dec_index.borrow().1.is_empty(), "index should be built for a >1MB non-UTF-8 buffer");
        // Spot-check across the whole range, including far cold jumps that used to walk from 0.
        for &rope in &[0usize, 1, 500_000, 999_999, n - 1, n] {
            assert_eq!(b.decoded_offset(rope), rope * 3, "decoded_offset({rope})");
        }
        for &dec in &[0usize, 3, 1_500_000, n * 3 - 3, n * 3] {
            assert_eq!(b.rope_offset(dec), (dec / 3).min(n), "rope_offset({dec})");
        }
        // Jump bottom→top→bottom (worst thrash for a single checkpoint) stays exact.
        assert_eq!(b.decoded_offset(n), n * 3);
        assert_eq!(b.decoded_offset(0), 0);
        assert_eq!(b.decoded_offset(n), n * 3);
    }

    #[test]
    fn anchored_offset_from_agrees_with_checkpoint_version() {
        // A run of CP437 '╦' (byte 0xCB → 3 UTF-8 bytes): rope and decoded offsets diverge by 3×.
        // The window-anchored translators must give the SAME answer as the checkpoint versions,
        // regardless of the anchor chosen — that's what lets the GUI anchor on a window start
        // instead of walking from a document-global checkpoint (O(file) at the bottom of a big doc).
        let b = Buffer::from_bytes(vec![0xCB; 20], cp437());
        // Anchor partway in (rope 5 ↔ decoded 15), then translate both directions.
        for rope in 0..=20 {
            assert_eq!(b.decoded_offset_from(rope, 5, 15), b.decoded_offset(rope), "rope={rope}");
        }
        for dec in (0..=60).step_by(3) {
            assert_eq!(b.rope_offset_from(dec, 5, 15), b.rope_offset(dec), "dec={dec}");
        }
        // UTF-8 identity: anchored variants are the O(1) pass-through too.
        let u = Buffer::from_str("héllo wörld");
        assert_eq!(u.decoded_offset_from(6, 0, 0), 6);
        assert_eq!(u.rope_offset_from(6, 0, 0), 6);
    }

    #[test]
    fn selection_via_extend_then_delete() {
        let mut b = Buffer::from_str("hello");
        b.move_doc_start(false);
        b.move_right(true); // select 'h'
        b.move_right(true); // select 'he'
        assert!(b.has_selection());
        assert_eq!(b.selection_text(), "he");
        b.delete_selection();
        assert_eq!(b.text(), "llo");
        assert_eq!(b.cursor(), 0);
        assert!(!b.has_selection());
    }

    #[test]
    fn insert_replaces_selection() {
        let mut b = Buffer::from_str("hello");
        b.move_doc_start(false);
        b.move_right(true);
        b.move_right(true); // select "he"
        b.insert("HE");
        assert_eq!(b.text(), "HEllo");
    }

    #[test]
    fn render_shows_cursor_and_selection() {
        let mut b = Buffer::from_str("abc");
        b.move_doc_start(false);
        assert_eq!(b.render(), "‸abc"); // cursor at start
        b.move_right(true);
        b.move_right(true); // select "ab"; cursor at the selection's trailing edge
        assert_eq!(b.render(), "[ab]‸c");
    }

    // ---- defined-text ("DF") selection — auto-track ------------------------

    #[test]
    fn define_extends_live_with_motion() {
        let mut b = Buffer::from_str("hello world");
        b.move_doc_start(false); // cursor at 0
        assert!(b.toggle_define()); // DF opens
        assert!(b.is_defining());
        assert!(b.selection_text().is_empty()); // empty until the cursor moves
        // Plain motion extends the selection live (no Shift, no DM).
        b.move_right(false);
        b.move_right(false);
        b.move_right(false);
        assert_eq!(b.selection_text(), "hel");
        b.move_right(false);
        b.move_right(false);
        assert_eq!(b.selection_text(), "hello");
        // Moving back shrinks it.
        b.move_left(false);
        b.move_left(false);
        assert_eq!(b.selection_text(), "hel");
    }

    #[test]
    fn dm_reopens_from_the_start_edge_too() {
        // Define rightward-anchored, extend LEFT so the caret lands on the START edge.
        let mut b = Buffer::from_str("hello world");
        b.move_doc_start(false);
        for _ in 0..5 {
            b.move_right(false); // caret at 5
        }
        b.toggle_define(); // anchor 5
        b.move_left(false);
        b.move_left(false);
        b.move_left(false); // caret 2; selection [2,5] = "llo"
        b.close_define(); // frozen; caret on the START edge (2)
        assert_eq!(b.selection_text(), "llo");
        assert_eq!(b.cursor(), 2);
        b.modify_define(); // DM reopens, boundaries unchanged
        assert!(b.is_defining());
        assert_eq!(b.selection_text(), "llo");
        // Extending leftward from the start edge keeps growing the selection.
        b.move_left(false);
        assert_eq!(b.selection_text(), "ello"); // [1,5]
    }

    #[test]
    fn dm_reopens_a_closed_selection_for_more_extending() {
        let mut b = Buffer::from_str("hello world");
        b.move_doc_start(false);
        b.toggle_define(); // DF
        b.move_right(false);
        b.move_right(false); // select "he", caret at 2 (an edge)
        b.close_define(); // DZ — frozen [0,2]
        assert!(!b.is_defining());
        assert_eq!(b.selection_text(), "he");
        // DM reopens for live extending without changing the boundaries.
        b.modify_define();
        assert!(b.is_defining());
        assert_eq!(b.selection_text(), "he"); // unchanged
        // Further motion now extends live again.
        b.move_right(false);
        b.move_right(false);
        b.move_right(false); // caret 5
        assert_eq!(b.selection_text(), "hello");
        b.close_define();
        assert_eq!(b.selection_text(), "hello");
    }

    #[test]
    fn dz_closes_and_freezes_the_selection() {
        let mut b = Buffer::from_str("hello");
        b.move_doc_start(false);
        b.toggle_define();
        b.move_right(false);
        b.move_right(false); // selection "he", cursor at 2
        b.close_define(); // DZ: freeze
        assert!(!b.is_defining());
        assert_eq!(b.selection_text(), "he");
        assert_eq!(b.cursor(), 2);
        // A closed block PERSISTS across plain motion (XyWrite defined-text): the
        // caret decouples and roams; the block stays put. Only XD/YD or an edit clears.
        b.move_right(false);
        assert_eq!(b.selection_text(), "he"); // still selected
        assert_eq!(b.cursor(), 3); // caret roamed off the block
        b.clear_selection();
        assert!(!b.has_selection());
    }

    #[test]
    fn df_toggle_with_no_motion_cancels() {
        let mut b = Buffer::from_str("hi");
        b.move_doc_start(false);
        b.toggle_define(); // open
        assert!(!b.toggle_define()); // DF again with no motion -> closes empty -> cancel
        assert!(!b.is_defining());
        assert!(!b.has_selection());
    }

    #[test]
    fn typing_while_defining_replaces_the_selection() {
        let mut b = Buffer::from_str("hello");
        b.move_doc_start(false);
        b.toggle_define();
        b.move_right(false);
        b.move_right(false); // selection "he"
        b.insert("HE"); // replaces the selection (and ends defining)
        assert_eq!(b.text(), "HEllo");
        assert!(!b.is_defining());
    }

    #[test]
    fn select_all_then_cancel() {
        let mut b = Buffer::from_str("abc");
        b.select_all();
        assert_eq!(b.selection_text(), "abc");
        b.clear_selection();
        assert!(!b.has_selection());
    }

    // ---- persistent selection ----------------------------------------------

    #[test]
    fn ps_plain_motion_keeps_block_and_caret_roams() {
        let mut b = Buffer::from_str("hello world");
        b.move_doc_start(false);
        b.set_persistent(true);
        b.toggle_define();
        for _ in 0..5 {
            b.move_right(false); // select "hello"
        }
        b.close_define(); // frozen [0,5]
        assert_eq!(b.selection_text(), "hello");
        // Under PS, a plain motion keeps the block; the caret roams off it.
        b.move_doc_end(false);
        assert_eq!(b.selection_text(), "hello"); // block unchanged
        assert_eq!(b.cursor(), 11); // caret roamed
        assert!(b.sel_head().is_some()); // decoupled
    }

    #[test]
    fn dm_reshapes_a_roamed_block_to_the_caret() {
        let mut b = Buffer::from_str("hello world");
        b.move_doc_start(false);
        b.set_persistent(true);
        b.toggle_define();
        for _ in 0..5 {
            b.move_right(false); // "hello"
        }
        b.close_define(); // [0,5]
        b.move_doc_end(false); // caret roams to 11; block stays [0,5]
        b.modify_define(); // DM: nearest edge (5) follows the caret -> [0,11]
        assert!(b.is_defining());
        assert_eq!(b.selection_text(), "hello world");
        // Live again: motion adjusts the span.
        b.move_left(false);
        assert_eq!(b.selection_text(), "hello worl"); // [0,10]
    }

    #[test]
    fn ps_typing_after_block_keeps_it() {
        let mut b = Buffer::from_str("hello world");
        b.move_doc_start(false);
        b.set_persistent(true);
        b.toggle_define();
        for _ in 0..5 {
            b.move_right(false); // "hello" [0,5]
        }
        b.close_define();
        b.move_doc_end(false); // caret roams to 11; block held
        b.insert("X"); // insert after the block — block unchanged
        assert_eq!(b.text(), "hello worldX");
        assert_eq!(b.selection_text(), "hello");
    }

    #[test]
    fn ps_insert_before_block_pushes_it_right() {
        let mut b = Buffer::from_str("hello world");
        b.move_doc_end(false);
        b.set_persistent(true);
        b.toggle_define();
        for _ in 0..5 {
            b.move_left(false); // block "world" [6,11]
        }
        b.close_define();
        b.move_doc_start(false); // caret roams to 0
        b.insert("XY"); // before the block -> rides right, same chars
        assert_eq!(b.text(), "XYhello world");
        assert_eq!(b.selection_text(), "world");
    }

    #[test]
    fn ps_delete_before_block_shifts_it_left() {
        let mut b = Buffer::from_str("hello world");
        b.move_doc_end(false);
        b.set_persistent(true);
        b.toggle_define();
        for _ in 0..5 {
            b.move_left(false); // block "world" [6,11]
        }
        b.close_define();
        b.move_doc_start(false); // caret 0
        b.delete_forward(); // delete 'h' before the block -> block shifts left
        assert_eq!(b.text(), "ello world");
        assert_eq!(b.selection_text(), "world");
    }

    #[test]
    fn click_under_ps_keeps_the_block() {
        let mut b = Buffer::from_str("hello world");
        b.move_doc_start(false);
        b.set_persistent(true);
        b.toggle_define();
        for _ in 0..5 {
            b.move_right(false); // "hello"
        }
        b.close_define();
        b.click_caret(9); // click outside the block — navigation, block kept
        assert_eq!(b.selection_text(), "hello");
        assert_eq!(b.cursor(), 9);
        b.click_caret(2); // and again (the reported "twice" case)
        assert_eq!(b.selection_text(), "hello");
    }

    #[test]
    fn word_before_cursor_keeps_a_contraction_together() {
        let mut b = Buffer::from_str("doesn't and 'quoted");
        // Caret right after the apostrophe (mid-typing): the word is still "doesn'", not "doesn" —
        // so a spelling squiggle on "doesn" stays deferred until a real separator finishes the word.
        b.set_caret(6, None); // after "doesn'"
        assert_eq!(b.word_before_cursor(), Some((0, 6)));
        b.set_caret(7, None); // the finished contraction "doesn't"
        assert_eq!(b.word_before_cursor(), Some((0, 7)));
        // A real separator (space) ends the word → nothing being typed.
        b.set_caret(8, None); // after "doesn't "
        assert_eq!(b.word_before_cursor(), None);
        // A leading apostrophe still needs a real word char; a lone quote is not a word.
        b.set_caret(13, None); // after the "'" of "'quoted"
        assert_eq!(b.word_before_cursor(), None);
        b.set_caret(19, None); // after "'quoted"
        assert_eq!(b.word_before_cursor(), Some((12, 19)));
    }

    #[test]
    fn click_keeps_the_block_navigation() {
        let mut b = Buffer::from_str("hello");
        b.move_doc_start(false);
        b.toggle_define();
        b.move_right(false);
        b.move_right(false); // "he"
        b.close_define();
        b.click_caret(4); // a click is navigation — keeps the defined block
        assert_eq!(b.selection_text(), "he");
        assert_eq!(b.cursor(), 4); // caret moved to the click
        // XD clears it.
        b.clear_selection();
        assert!(!b.has_selection());
    }

    #[test]
    fn selection_persists_across_motion_regardless_of_ps() {
        // Motion-persistence is unconditional now; PS only governs edits.
        let mut b = Buffer::from_str("hello world");
        b.move_doc_start(false);
        b.toggle_define();
        for _ in 0..5 {
            b.move_right(false); // "hello"
        }
        b.close_define(); // PS is OFF here
        b.move_doc_end(false); // roam -> decoupled, block held
        assert!(b.sel_head().is_some());
        assert_eq!(b.selection_text(), "hello");
        assert_eq!(b.cursor(), 11);
    }
}
