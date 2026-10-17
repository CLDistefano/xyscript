//! Minimal line-level diff for the `DIFF` command: a hand-rolled Myers O(ND) edit script
//! (Eugene Myers, "An O(ND) Difference Algorithm and Its Variations", 1986) rendered as a
//! unified diff. No external crates — see host.rs (`EditorOp::Diff`), which reads the current
//! buffer and its on-disk file and paints the `-`/`+`/`@@` lines via `set_color_spans_raw`.
//!
//! Lines are compared with `str::lines()` semantics: split on `\n`, a trailing `\r` stripped.
//! So a pure CRLF-vs-LF or trailing-newline change reads as "identical" — this is a *content*
//! diff, not a byte diff.

/// What a rendered unified-diff line is, so the caller can colour it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tag {
    /// `---`/`+++` file header.
    Header,
    /// `@@ -a,b +c,d @@` hunk header.
    Hunk,
    /// A context line (unchanged, present in both), rendered with a leading space.
    Context,
    /// A removed line (present only in the old/on-disk side), leading `-`.
    Del,
    /// An inserted line (present only in the new/in-memory side), leading `+`.
    Ins,
}

/// One rendered line of the unified diff (text already carries its ` `/`-`/`+` prefix).
#[derive(Clone, Debug)]
pub struct DiffLine {
    pub tag: Tag,
    pub text: String,
}

/// An element of the edit script, in document order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    /// `a[ai] == b[bi]`.
    Equal(usize, usize),
    /// `a[ai]` is only on the old side.
    Delete(usize),
    /// `b[bi]` is only on the new side.
    Insert(usize),
}

/// The Myers edit script between two line sequences, in order.
fn diff_ops(a: &[&str], b: &[&str]) -> Vec<Op> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    if n == 0 {
        return (0..b.len()).map(Op::Insert).collect();
    }
    if m == 0 {
        return (0..a.len()).map(Op::Delete).collect();
    }
    let max = (n + m) as usize;
    let off = max as isize; // shift so k in [-max, max] indexes 0..=2*max
    let mut v = vec![0isize; 2 * max + 1];
    let mut trace: Vec<Vec<isize>> = Vec::new();

    let mut d_final = 0isize;
    'outer: for d in 0..=max as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            // Choose to extend from the neighbour with the larger x: move down (insertion,
            // take k+1) or right (deletion, take k-1 then +1 in x).
            let down = k == -d || (k != d && v[(k - 1 + off) as usize] < v[(k + 1 + off) as usize]);
            let mut x = if down { v[(k + 1 + off) as usize] } else { v[(k - 1 + off) as usize] + 1 };
            let mut y = x - k;
            // Slide along the diagonal of equal lines (a free "snake").
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(k + off) as usize] = x;
            if x >= n && y >= m {
                d_final = d;
                break 'outer;
            }
            k += 2;
        }
    }

    // Backtrack through the saved per-d frontiers to recover the ops.
    let mut ops: Vec<Op> = Vec::new();
    let mut x = n;
    let mut y = m;
    for d in (0..=d_final).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let down = k == -d || (k != d && v[(k - 1 + off) as usize] < v[(k + 1 + off) as usize]);
        let prev_k = if down { k + 1 } else { k - 1 };
        let prev_x = v[(prev_k + off) as usize];
        let prev_y = prev_x - prev_k;
        // Walk back down the snake (equal lines) to (prev_x, prev_y)'s diagonal end.
        while x > prev_x && y > prev_y {
            ops.push(Op::Equal((x - 1) as usize, (y - 1) as usize));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                ops.push(Op::Insert((y - 1) as usize));
            } else {
                ops.push(Op::Delete((x - 1) as usize));
            }
        }
        x = prev_x;
        y = prev_y;
    }
    ops.reverse();
    ops
}

/// Compute the line-level diff of `a` (old) vs `b` (new) and render a unified diff with
/// `context` lines of surrounding context. Returns an empty vector when the two are identical
/// (by `lines()` semantics), so the caller can show a dedicated "identical" message.
pub fn unified(a: &str, b: &str, a_name: &str, b_name: &str, context: usize) -> Vec<DiffLine> {
    let al: Vec<&str> = a.lines().collect();
    let bl: Vec<&str> = b.lines().collect();
    let ops = diff_ops(&al, &bl);
    if ops.iter().all(|o| matches!(o, Op::Equal(..))) {
        return Vec::new();
    }

    // Flatten to records carrying 1-based line numbers on each side.
    #[derive(Clone)]
    struct Rec {
        tag: Tag,
        a_no: usize,
        b_no: usize,
        text: String,
    }
    let mut recs: Vec<Rec> = Vec::with_capacity(ops.len());
    for op in &ops {
        match *op {
            Op::Equal(ai, bi) => recs.push(Rec { tag: Tag::Context, a_no: ai + 1, b_no: bi + 1, text: al[ai].to_string() }),
            Op::Delete(ai) => recs.push(Rec { tag: Tag::Del, a_no: ai + 1, b_no: 0, text: al[ai].to_string() }),
            Op::Insert(bi) => recs.push(Rec { tag: Tag::Ins, a_no: 0, b_no: bi + 1, text: bl[bi].to_string() }),
        }
    }

    // Group changes into hunks: each hunk spans from `context` lines before the first change to
    // `context` lines after the last, merging neighbours whose context windows touch.
    let change_idx: Vec<usize> = recs.iter().enumerate().filter(|(_, r)| r.tag != Tag::Context).map(|(i, _)| i).collect();
    let mut out = vec![
        DiffLine { tag: Tag::Header, text: format!("--- {a_name}") },
        DiffLine { tag: Tag::Header, text: format!("+++ {b_name}") },
    ];

    let mut ci = 0;
    while ci < change_idx.len() {
        let start = change_idx[ci].saturating_sub(context);
        let mut end = (change_idx[ci] + context).min(recs.len() - 1);
        // Absorb following changes whose leading context overlaps this hunk's trailing context.
        let mut cj = ci + 1;
        while cj < change_idx.len() && change_idx[cj].saturating_sub(context) <= end + 1 {
            end = (change_idx[cj] + context).min(recs.len() - 1);
            cj += 1;
        }

        // Hunk header line numbers/counts (1-based; a zero count prints as `start,0`).
        let slice = &recs[start..=end];
        let a_lines: Vec<&Rec> = slice.iter().filter(|r| r.tag != Tag::Ins).collect();
        let b_lines: Vec<&Rec> = slice.iter().filter(|r| r.tag != Tag::Del).collect();
        let a_start = a_lines.first().map(|r| r.a_no).unwrap_or(0);
        let b_start = b_lines.first().map(|r| r.b_no).unwrap_or(0);
        let a_count = a_lines.len();
        let b_count = b_lines.len();
        out.push(DiffLine {
            tag: Tag::Hunk,
            text: format!("@@ -{a_start},{a_count} +{b_start},{b_count} @@"),
        });
        for r in slice {
            let (prefix, tag) = match r.tag {
                Tag::Del => ('-', Tag::Del),
                Tag::Ins => ('+', Tag::Ins),
                _ => (' ', Tag::Context),
            };
            out.push(DiffLine { tag, text: format!("{prefix}{}", r.text) });
        }
        ci = cj;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(d: &[DiffLine]) -> Vec<Tag> {
        d.iter().map(|l| l.tag).collect()
    }
    fn body(d: &[DiffLine]) -> Vec<&str> {
        d.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn identical_inputs_produce_no_diff() {
        assert!(unified("a\nb\nc\n", "a\nb\nc\n", "x", "y", 3).is_empty());
        // Trailing-newline and CRLF-only differences are content-identical.
        assert!(unified("a\nb", "a\nb\n", "x", "y", 3).is_empty());
        assert!(unified("a\r\nb\r\n", "a\nb\n", "x", "y", 3).is_empty());
    }

    #[test]
    fn single_line_change_is_one_delete_one_insert() {
        let d = unified("one\ntwo\nthree\n", "one\nTWO\nthree\n", "a", "b", 3);
        let b = body(&d);
        assert_eq!(b[0], "--- a");
        assert_eq!(b[1], "+++ b");
        assert!(b.iter().any(|l| *l == "-two"), "{b:?}");
        assert!(b.iter().any(|l| *l == "+TWO"), "{b:?}");
        assert!(b.iter().any(|l| *l == " one") && b.iter().any(|l| *l == " three"), "context kept: {b:?}");
        assert_eq!(d.iter().filter(|l| l.tag == Tag::Del).count(), 1);
        assert_eq!(d.iter().filter(|l| l.tag == Tag::Ins).count(), 1);
    }

    #[test]
    fn pure_insertions_and_deletions() {
        let ins = unified("a\nb\n", "a\nx\ny\nb\n", "a", "b", 3);
        assert_eq!(ins.iter().filter(|l| l.tag == Tag::Ins).count(), 2);
        assert_eq!(ins.iter().filter(|l| l.tag == Tag::Del).count(), 0);

        let del = unified("a\nx\ny\nb\n", "a\nb\n", "a", "b", 3);
        assert_eq!(del.iter().filter(|l| l.tag == Tag::Del).count(), 2);
        assert_eq!(del.iter().filter(|l| l.tag == Tag::Ins).count(), 0);
    }

    #[test]
    fn empty_sides() {
        let add = unified("", "a\nb\n", "a", "b", 3);
        assert_eq!(add.iter().filter(|l| l.tag == Tag::Ins).count(), 2);
        let wipe = unified("a\nb\n", "", "a", "b", 3);
        assert_eq!(wipe.iter().filter(|l| l.tag == Tag::Del).count(), 2);
    }

    #[test]
    fn distant_changes_make_separate_hunks() {
        let a = (1..=30).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let mut bv: Vec<String> = (1..=30).map(|n| n.to_string()).collect();
        bv[1] = "TWO".into(); // line 2
        bv[27] = "TWENTYEIGHT".into(); // line 28 — far from line 2
        let b = bv.join("\n");
        let d = unified(&a, &b, "a", "b", 3);
        assert_eq!(d.iter().filter(|l| l.tag == Tag::Hunk).count(), 2, "two separate hunks: {:?}", body(&d));
    }

    #[test]
    fn adjacent_changes_merge_into_one_hunk() {
        let a = (1..=30).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let mut bv: Vec<String> = (1..=30).map(|n| n.to_string()).collect();
        bv[10] = "ELEVEN".into();
        bv[12] = "THIRTEEN".into(); // within 2*context of the first change
        let b = bv.join("\n");
        let d = unified(&a, &b, "a", "b", 3);
        assert_eq!(d.iter().filter(|l| l.tag == Tag::Hunk).count(), 1, "one merged hunk: {:?}", body(&d));
    }

    #[test]
    fn hunk_header_line_numbers_are_correct() {
        let d = unified("one\ntwo\nthree\n", "one\nTWO\nthree\n", "a", "b", 1);
        let hunk = d.iter().find(|l| l.tag == Tag::Hunk).unwrap();
        // 1 line of context either side of the change at line 2 → spans lines 1..=3 on both sides.
        assert_eq!(hunk.text, "@@ -1,3 +1,3 @@", "{}", hunk.text);
        assert_eq!(tags(&d)[0], Tag::Header);
    }
}
