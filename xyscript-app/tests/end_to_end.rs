//! Stage 2 end-to-end: the shipped internal KBD driven through the real `Editor`
//! closed loop (`development-plan.md` Stage 2 exit criteria — type with a XPL-authored
//! KBD, see characters appear, run a command, with a broken load falling back without
//! stranding).

use xyscript_app::host::VerifyChoice;
use xyscript_app::Editor;
use xyscript_kbd::event::KeyEvent;
use xyscript_kbd::{load, Disposition, LoadOutcome};

const INTERNAL_KBD: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../xyscript-kbd/kbd/xyscript-internal.kbd"));

/// Normalize CRLF → LF so document assertions are line-ending-agnostic (new docs
/// default to the OS line ending, which is CRLF on the Windows CI host).
fn nl(s: &str) -> String {
    s.replace("\r\n", "\n")
}

fn sample_editor() -> (Editor, u32) {
    let km = match load(INTERNAL_KBD, Some(120)) {
        LoadOutcome::Installed(km) => km,
        LoadOutcome::Rejected(e) => panic!("sample kbd failed to load: {e:?}"),
    };
    let ctrl = km.bitmask_for_names(&["CTRL"]).expect("CTRL is a declared modifier");
    let editor = Editor::new(km);
    (editor, ctrl)
}

#[test]
fn typing_with_the_sample_kbd_produces_text() {
    let (mut editor, _ctrl) = sample_editor();
    // q(16), w(17), e is backspace? no — in the sample 18=insert "e".
    editor.feed_key(KeyEvent::down(16)); // q
    editor.feed_key(KeyEvent::down(17)); // w
    editor.feed_key(KeyEvent::down(18)); // e
    assert_eq!(editor.document().text(), "qwe");
    assert_eq!(editor.render(), "qwe‸");
}

#[test]
fn tab_key_inserts_a_tab_character() {
    // The sample KBD binds `15=TAB` (a command). The Tab key must insert a real `\t`, not be
    // swallowed (the old `15="\t"` typing cell fell through to egui's focus navigation in the GUI).
    let (mut editor, _ctrl) = sample_editor();
    editor.feed_key(KeyEvent::down(15)); // Tab
    assert_eq!(editor.document().text(), "\t");
    editor.run_command("INSERT \"x\""); // a following char lands after the tab
    assert_eq!(editor.document().text(), "\tx");
}

#[test]
fn shift_arrow_extends_the_selection() {
    let (mut editor, _ctrl) = sample_editor();
    editor.feed_key(KeyEvent::down(16)); // q
    editor.feed_key(KeyEvent::down(17)); // w
    editor.feed_key(KeyEvent::down(18)); // e
    assert_eq!(editor.document().text(), "qwe");

    // Shift+Left selects the last char, then the last two — the SHIFT layer maps
    // 101 (classic Left) to `LEFT SEL` (extend), reached via the modifier bitmask.
    editor.feed_key_mods(101, true, false, false);
    assert_eq!(editor.document().selection_text(), "e");
    editor.feed_key_mods(101, true, false, false);
    assert_eq!(editor.document().selection_text(), "we");

    // A plain (unshifted) Left collapses the selection back to a caret.
    editor.feed_key_mods(101, false, false, false);
    assert!(editor.document().selection_span().is_none());
    assert_eq!(editor.document().text(), "qwe"); // motion never edits
}

#[test]
fn defined_text_selection_via_cmline_verbs() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"hello world\"");
    editor.run_command("TOP"); // cursor to doc start
    editor.run_command("DF"); // open a defined-text selection (auto-track)
    for _ in 0..5 {
        editor.run_command("RIGHT"); // motion extends the selection live
    }
    assert_eq!(editor.document().selection_text(), "hello");
    editor.run_command("DM"); // DM re-opens (already open here) — boundaries unchanged
    assert_eq!(editor.document().selection_text(), "hello");
    editor.run_command("DZ"); // close: freeze the span
    assert_eq!(editor.document().selection_text(), "hello");
    editor.run_command("XD"); // cancel
    assert!(editor.document().selection_span().is_none());
    assert!(editor.last_error().is_none());
    // DFA selects the whole document.
    editor.run_command("DFA");
    assert_eq!(editor.document().selection_text(), "hello world");
}

#[test]
fn persistent_selection_roam_then_dm_via_cmline() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"hello world\"");
    editor.run_command("TOP");
    editor.run_command("SPS"); // persistent selection on
    editor.run_command("DF");
    for _ in 0..5 {
        editor.run_command("RIGHT"); // select "hello"
    }
    editor.run_command("DZ"); // freeze [0,5]
    assert_eq!(editor.document().selection_text(), "hello");
    // Roam the caret off the block — PS keeps it.
    editor.run_command("END");
    assert_eq!(editor.document().selection_text(), "hello"); // block held
    // DM reshapes the block out to the caret and resumes live extending.
    editor.run_command("DM");
    assert_eq!(editor.document().selection_text(), "hello world");
    editor.run_command("LEFT"); // live: shrink by one
    assert_eq!(editor.document().selection_text(), "hello worl");
    // The defined block persists across plain motion (XyWrite-style); XD clears it.
    editor.run_command("DZ");
    editor.run_command("LEFT"); // plain motion keeps the defined block
    assert_eq!(editor.document().selection_text(), "hello worl");
    editor.run_command("XD");
    assert!(editor.document().selection_span().is_none());
    assert!(editor.last_error().is_none());
}

#[test]
fn deletion_buffer_cut_move_paste_via_cmline() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"hello world\"");
    editor.run_command("TOP");
    editor.run_command("DF");
    for _ in 0..6 {
        editor.run_command("RIGHT"); // select "hello " (6 chars)
    }
    editor.run_command("DZ");
    assert_eq!(editor.document().selection_text(), "hello ");
    editor.run_command("RD"); // cut to deletion buffer
    assert_eq!(editor.document().text(), "world");
    // Move the caret to the end and paste — RD + move + UD = move text.
    editor.run_command("BOTTOM");
    editor.run_command("UD");
    assert_eq!(editor.document().text(), "worldhello ");
    assert!(editor.last_error().is_none());
}

#[test]
fn mv_via_feedkey_arrows_mirrors_the_gui() {
    // Mirror the GUI: arrows drive motion via feed_key (KBD), DF/DZ/MV via dispatch.
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"hello world\"");
    editor.run_command("TOP");
    editor.run_command("SPS");
    editor.run_command("DF"); // open define
    for _ in 0..5 {
        editor.feed_key(KeyEvent::down(103)); // RIGHT — extends while defining
    }
    editor.run_command("DZ"); // close -> [0,5]
    assert_eq!(editor.document().selection_text(), "hello");
    for _ in 0..6 {
        editor.feed_key(KeyEvent::down(103)); // RIGHT — roam (decouple under PS)
    }
    assert_eq!(editor.document().selection_text(), "hello"); // block held
    editor.run_command("MV");
    // "hello" (with the caret roamed to 11) moves; the text must change.
    assert_ne!(editor.document().text(), "hello world");
    assert!(editor.last_error().is_none());
}

#[test]
fn mv_via_submit_cmdline_mirrors_gui() {
    // Full GUI route: arrows via feed_key, MV typed into the CMline + submitted.
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"hello world\"");
    editor.run_command("TOP");
    editor.run_command("SPS");
    editor.run_command("DF");
    for _ in 0..5 {
        editor.feed_key(KeyEvent::down(103)); // RIGHT — extends while defining
    }
    editor.run_command("DZ");
    for _ in 0..6 {
        editor.feed_key(KeyEvent::down(103)); // RIGHT — roam under PS
    }
    editor.open_cmdline();
    editor.insert_str("MV"); // goes to the CMline (target = CmLine)
    editor.submit_cmdline(); // runs MV on the document
    assert_ne!(editor.document().text(), "hello world");
    assert!(editor.last_error().is_none());
}

#[test]
fn cp_and_mv_via_cmline_under_ps() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"hello world\"");
    editor.run_command("TOP");
    editor.run_command("SPS");
    editor.run_command("DF");
    for _ in 0..6 {
        editor.run_command("RIGHT"); // select "hello "
    }
    editor.run_command("DZ");
    editor.run_command("BOTTOM"); // caret to end; PS keeps the block
    editor.run_command("CP"); // copy "hello " to the caret
    assert_eq!(editor.document().text(), "hello worldhello ");
    assert!(editor.last_error().is_none());

    // Fresh doc for MV.
    let (mut e2, _c2) = sample_editor();
    e2.run_command("INSERT \"hello world\"");
    e2.run_command("TOP");
    e2.run_command("SPS");
    e2.run_command("DF");
    for _ in 0..6 {
        e2.run_command("RIGHT"); // select "hello "
    }
    e2.run_command("DZ");
    e2.run_command("BOTTOM");
    e2.run_command("MV"); // move "hello " to the end
    assert_eq!(e2.document().text(), "worldhello ");
    assert!(e2.last_error().is_none());
}

#[test]
fn dn_with_no_selection_is_a_noop_not_a_char_delete() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"abc\"");
    editor.run_command("TOP"); // cursor on 'a'
    editor.run_command("DN"); // no selection -> trappable error, NOT a char delete
    // The divergence from QN: the character under the cursor is left intact.
    assert_eq!(editor.document().text(), "abc");
}

#[test]
fn case_verbs_via_cmline() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"hello\"");
    editor.run_command("TOP");
    editor.run_command("DFA"); // select all
    editor.run_command("UC");
    assert_eq!(editor.document().text(), "HELLO");
    editor.run_command("LC");
    assert_eq!(editor.document().text(), "hello");
    assert!(editor.last_error().is_none());
}

#[test]
fn search_finds_selects_and_walks_forward() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"the cat sat on the mat\"");
    editor.run_command("TOP");
    editor.run_command("SE /at/"); // first "at" (in "cat")
    assert_eq!(editor.document().selection_text(), "at");
    assert_eq!(editor.document().cursor(), 7); // caret on the trailing edge
    editor.run_command("SE /at/"); // next "at" (in "sat") — walks from the match end
    assert_eq!(editor.document().selection_text(), "at");
    assert_eq!(editor.document().cursor(), 11);
    assert!(editor.last_error().is_none());
}

#[test]
fn search_does_not_wrap_at_end_of_file() {
    // XyWrite semantics: a forward search stops at the last occurrence. Once the
    // caret is past every match, the next SE must NOT wrap to the top — it must
    // report not-found ($ERROR) so loop-until-not-found scripts terminate.
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"alpha beta alpha\"");
    editor.run_command("TOP");
    editor.run_command("SE /alpha/"); // first "alpha"
    editor.run_command("PR $ERROR");
    assert_eq!(editor.output().last().map(String::as_str), Some("0"));
    editor.run_command("SE /alpha/"); // second "alpha" (last occurrence)
    editor.run_command("PR $ERROR");
    assert_eq!(editor.output().last().map(String::as_str), Some("0"));
    editor.run_command("SE /alpha/"); // nothing ahead → trappable error, NOT a wrap to the top
    editor.run_command("PR $ERROR");
    assert_eq!(
        editor.output().last().map(String::as_str),
        Some("4"), // ErrCode::SearchNotFound
        "forward search wrapped instead of stopping at EOF"
    );

    // And backward (SEB) must not wrap past the first occurrence.
    editor.run_command("TOP");
    editor.run_command("SEB /alpha/"); // nothing before the top → trappable error, no wrap to bottom
    editor.run_command("PR $ERROR");
    assert_eq!(
        editor.output().last().map(String::as_str),
        Some("4"), // ErrCode::SearchNotFound
        "backward search wrapped instead of stopping at BOF"
    );
}

#[test]
fn search_is_case_insensitive_by_default_and_absolute_with_a() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"Foo foo FOO\"");
    editor.run_command("TOP");
    editor.run_command("SE /foo/"); // insensitive: matches the leading "Foo"
    assert_eq!(editor.document().selection_text(), "Foo");
    editor.run_command("TOP");
    editor.run_command("SE/A /foo/"); // absolute (flags first): skips Foo/FOO, matches "foo"
    assert_eq!(editor.document().selection_text(), "foo");
    assert_eq!(editor.document().cursor(), 7);
    // SEA verb is equivalent to the /A flag.
    editor.run_command("TOP");
    editor.run_command("SEA /foo/");
    assert_eq!(editor.document().cursor(), 7);
}

#[test]
fn search_backward_and_whole_word() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"cat category cat\"");
    editor.run_command("TOP");
    editor.run_command("SE/W /cat/"); // whole word (flags first): the leading "cat" (0..3)
    assert_eq!(editor.document().cursor(), 3);
    editor.run_command("SE/W /cat/"); // skip "category", match the trailing "cat"
    assert_eq!(editor.document().cursor(), 16);
    // SEB walks backward; caret lands on the match start.
    editor.run_command("BOTTOM");
    editor.run_command("SEB/W /cat/");
    assert_eq!(editor.document().cursor(), 13);
    assert!(editor.last_error().is_none());
}

#[test]
fn empty_search_repeats_the_last_pattern() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"x ab y ab z\"");
    editor.run_command("TOP");
    editor.run_command("SE /ab/"); // first "ab" (2..4)
    assert_eq!(editor.document().cursor(), 4);
    editor.run_command("SE"); // bare SE repeats /ab/ → next "ab" (7..9)
    assert_eq!(editor.document().cursor(), 9);
    assert_eq!(editor.document().selection_text(), "ab");
}

#[test]
fn search_not_found_trips_an_error() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"hello world\"");
    editor.run_command("SE /zzz/");
    // A not-found is a *trappable* error: it sets $ERROR and continues (it does
    // not surface as a fatal last_error). No match → no selection.
    assert!(editor.document().selection_span().is_none());
    // Both reads in one command — an action verb clears $ERROR on entry (after expanding args).
    editor.run_command("PR $ERROR $ERRORMSG[$ERROR]");
    assert_eq!(editor.output().last().map(String::as_str), Some("4 search text not found")); // ErrCode::SearchNotFound
}

#[test]
fn replace_all_changes_every_match_and_sets_cc() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"a-a-a-a\"");
    editor.run_command("TOP");
    editor.run_command("CH /a/X/"); // whole doc from top of an at-cursor run
    assert_eq!(editor.document().text(), "X-X-X-X");
    // $CC holds the replacement count.
    editor.run_command("PR $CC");
    assert_eq!(editor.output().last().map(String::as_str), Some("4"));
    // One undo reverts the entire replace-all.
    editor.run_command("UNDO");
    assert_eq!(editor.document().text(), "a-a-a-a");
    assert!(editor.last_error().is_none());
}

#[test]
fn replace_all_case_insensitive_and_from_cursor() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"Cat cat CAT\"");
    editor.run_command("TOP");
    editor.run_command("CH /cat/dog/"); // insensitive: all three
    assert_eq!(editor.document().text(), "dog dog dog");

    // From cursor (not /T): only matches at/after the caret are changed.
    let (mut e2, _c2) = sample_editor();
    e2.run_command("INSERT \"aa aa aa\"");
    e2.run_command("TOP");
    e2.run_command("SE /aa/"); // caret now at 2 (end of first match)
    e2.run_command("CH /aa/Z/"); // replaces the 2nd and 3rd, not the 1st
    assert_eq!(e2.document().text(), "aa Z Z");
    e2.run_command("PR $CC");
    assert_eq!(e2.output().last().map(String::as_str), Some("2"));
}

#[test]
fn replace_all_not_found_makes_no_edit() {
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"keep me\"");
    editor.run_command("CH /zzz/qqq/"); // no match → $ERROR, $CC = 0, no edit
    assert_eq!(editor.document().text(), "keep me");
    editor.run_command("PR $CC");
    assert_eq!(editor.output().last().map(String::as_str), Some("0"));
}

#[test]
fn ci_cia_are_synonyms_for_ch_cha() {
    // CI/CIA = XyWrite muscle-memory synonyms for CH/CHA (replace all).
    let (mut editor, _ctrl) = sample_editor();
    editor.run_command("INSERT \"Cat cat CAT\"");
    editor.run_command("TOP");
    editor.run_command("CI /cat/dog/"); // like CH: case-insensitive, all three
    assert_eq!(editor.document().text(), "dog dog dog");

    // CIA = CHA: case-absolute (only the exact-case match changes).
    let (mut e2, _c2) = sample_editor();
    e2.run_command("INSERT \"Cat cat CAT\"");
    e2.run_command("TOP");
    e2.run_command("CIA /cat/dog/"); // like CHA: only the lowercase "cat"
    assert_eq!(e2.document().text(), "Cat dog CAT");
    e2.run_command("PR $CC");
    assert_eq!(e2.output().last().map(String::as_str), Some("1"));
}

#[test]
fn search_flags_first_attached_spaced_and_dash() {
    // The flag-position-free forms all mean the same thing: case-absolute find.
    for cmd in ["SE/A \"foo\"", "SE /A \"foo\"", "SE -a \"foo\""] {
        let (mut e, _c) = sample_editor();
        e.run_command("INSERT \"Foo foo\"");
        e.run_command("TOP");
        e.run_command(cmd);
        assert_eq!(e.document().selection_text(), "foo", "cmd = {cmd}");
        assert!(e.last_error().is_none(), "cmd = {cmd}");
    }
}

#[test]
fn search_flags_first_whole_word_and_from_top() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"category cat\"");
    e.run_command("TOP");
    e.run_command("SE/W \"cat\""); // whole word → skip "category", match standalone "cat"
    assert_eq!(e.document().cursor(), 12);
    e.run_command("SE/T \"cat\""); // /T from top (no /W) → first "cat" (inside "category")
    assert_eq!(e.document().cursor(), 3);
    assert!(e.last_error().is_none());
}

#[test]
fn replace_flags_first_attached() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"Foo foo FOO\"");
    e.run_command("TOP");
    e.run_command("CH/A /foo/X/"); // absolute → only the lowercase "foo"
    assert_eq!(e.document().text(), "Foo X FOO");
    e.run_command("PR $CC");
    assert_eq!(e.output().last().map(String::as_str), Some("1"));
}

#[test]
fn regex_search_dot_and_character_classes() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"cat cot 42 dog\"");
    e.run_command("TOP");
    e.run_command("SE/R /c.t/"); // c<any>t → "cat"
    assert_eq!(e.document().selection_text(), "cat");
    e.run_command("SE/R /c.t/"); // walks → "cot"
    assert_eq!(e.document().selection_text(), "cot");
    e.run_command("TOP");
    e.run_command(r"SE/R /\d+/"); // a digit run → "42" (escapes survive to the regex)
    assert_eq!(e.document().selection_text(), "42");
    assert!(e.last_error().is_none());
}

#[test]
fn regex_search_case_insensitive_default_and_absolute() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"Foo foo\"");
    e.run_command("TOP");
    e.run_command("SE/R /fo+/"); // insensitive by default → "Foo"
    assert_eq!(e.document().selection_text(), "Foo");
    e.run_command("TOP");
    e.run_command("SE/RA /fo+/"); // /R + /A absolute → skip "Foo", match "foo"
    assert_eq!(e.document().selection_text(), "foo");
}

#[test]
fn regex_replace_all_with_count() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"a1b22c333\"");
    e.run_command("TOP");
    e.run_command(r"CH/R /\d+/#/"); // each digit run → "#"
    assert_eq!(e.document().text(), "a#b#c#");
    e.run_command("PR $CC");
    assert_eq!(e.output().last().map(String::as_str), Some("3"));
    e.run_command("UNDO"); // one undo reverts the whole regex replace
    assert_eq!(e.document().text(), "a1b22c333");
}

#[test]
fn regex_replace_with_capture_backreferences() {
    // $1/$2 capture references in a /R replacement (the find still gets $-subst,
    // the regex replacement does not — so $1 reaches the regex engine).
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"foo=bar\"");
    e.run_command("TOP");
    e.run_command(r"CH/R /(\w+)=(\w+)/$2=$1/"); // swap around the '='
    assert_eq!(e.document().text(), "bar=foo");
    e.run_command("UNDO");
    assert_eq!(e.document().text(), "foo=bar");
}

#[test]
fn regex_replace_literal_dollar_with_double() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"5\"");
    e.run_command("TOP");
    e.run_command(r"CH/R /(\d)/$$$1/"); // $$ → literal $, then $1 → "5"
    assert_eq!(e.document().text(), "$5");
}

#[test]
fn regex_invalid_pattern_trips_error() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"abc\"");
    e.run_command("SE/R /(/"); // unbalanced group → compile error
    assert!(e.document().selection_span().is_none());
    e.run_command("PR $ERROR");
    assert_eq!(e.output().last().map(String::as_str), Some("1"));
}

#[test]
fn search_newline_escape_matches_lf_and_crlf() {
    // `\n` in a pattern matches a whole line ending — bare LF *or* CRLF.
    let (mut e1, _c1) = sample_editor();
    e1.run_command(r#"INSERT "ab\ncd""#); // a real LF in the middle
    e1.run_command("TOP");
    e1.run_command(r"SE /b\n/");
    assert_eq!(e1.document().selection_text(), "b\n");

    let (mut e2, _c2) = sample_editor();
    e2.run_command(r#"INSERT "ab\r\ncd""#); // CRLF
    e2.run_command("TOP");
    e2.run_command(r"SE /b\n/"); // pattern has no CR, but matches the CRLF break
    assert_eq!(e2.document().selection_text(), "b\r\n");
    assert!(e2.last_error().is_none());
}

#[test]
fn search_tab_escape_and_semicolon_in_pattern() {
    let (mut e, _c) = sample_editor();
    e.run_command(r#"INSERT "a\tb;c""#); // a, TAB, b, ;, c
    e.run_command("TOP");
    e.run_command(r"SE /\t/"); // find the tab
    assert_eq!(e.document().cursor(), 2);
    // A `;` inside the pattern must not split the command (search verbs are exempt).
    e.run_command("TOP");
    e.run_command("SE /b;c/");
    assert_eq!(e.document().selection_text(), "b;c");
    assert!(e.last_error().is_none());
}

#[test]
fn change_verify_yes_no_walk() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"a a a a\""); // four "a" at 0,2,4,6
    e.run_command("TOP");
    e.run_command("CV /a/X/"); // arm the walk; first match selected
    assert!(e.verify_active());
    assert_eq!(e.document().selection_text(), "a");
    e.verify_respond(VerifyChoice::Yes); // replace #1, advance
    assert!(e.verify_active());
    e.verify_respond(VerifyChoice::No); // skip #2, advance
    e.verify_respond(VerifyChoice::Yes); // replace #3, advance
    e.verify_respond(VerifyChoice::Yes); // replace #4 (last) → walk ends
    assert!(!e.verify_active());
    assert_eq!(e.document().text(), "X a X X");
}

#[test]
fn change_verify_one_more_replaces_then_stops() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"a a a\"");
    e.run_command("TOP");
    e.run_command("CV /a/X/");
    e.verify_respond(VerifyChoice::Yes); // #1
    e.verify_respond(VerifyChoice::OneMore); // #2 then stop; #3 untouched
    assert!(!e.verify_active());
    assert_eq!(e.document().text(), "X X a");
}

#[test]
fn change_verify_quit_keeps_caret_esc_returns_to_start() {
    // Quit: stop, don't replace current, caret stays at the current match.
    let (mut q, _c1) = sample_editor();
    q.run_command("INSERT \"a a a\"");
    q.run_command("TOP");
    q.run_command("CV /a/X/");
    q.verify_respond(VerifyChoice::Yes); // replace #1 → caret advances to #2 (at 2)
    q.verify_respond(VerifyChoice::Quit);
    assert!(!q.verify_active());
    assert_eq!(q.document().text(), "X a a");
    assert_eq!(q.document().cursor(), 2); // left at the current match

    // Esc: stop, don't replace current, caret back to where CV started; edits kept.
    let (mut s, _c2) = sample_editor();
    s.run_command("INSERT \"a a a\"");
    s.run_command("TOP"); // origin = 0
    s.run_command("CV /a/X/");
    s.verify_respond(VerifyChoice::Yes); // replace #1
    s.verify_respond(VerifyChoice::Esc);
    assert!(!s.verify_active());
    assert_eq!(s.document().text(), "X a a"); // #1 kept
    assert_eq!(s.document().cursor(), 0); // caret back to origin
}

#[test]
fn change_verify_regex_with_captures() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"a1 b2 c3\"");
    e.run_command("TOP");
    e.run_command(r"CV/R /(\w)(\d)/$2$1/"); // swap letter/digit at each stop
    e.verify_respond(VerifyChoice::Yes);
    e.verify_respond(VerifyChoice::Yes);
    e.verify_respond(VerifyChoice::OneMore);
    assert!(!e.verify_active());
    assert_eq!(e.document().text(), "1a 2b 3c");
}

#[test]
fn word_motion_walks_words() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"foo bar baz\""); // f0..o2 sp3 b4..r6 sp7 b8..z10
    e.run_command("TOP");
    e.run_command("NW");
    assert_eq!(e.document().cursor(), 4); // start of "bar"
    e.run_command("NW");
    assert_eq!(e.document().cursor(), 8); // start of "baz"
    e.run_command("PW");
    assert_eq!(e.document().cursor(), 4); // back to "bar"
    e.run_command("PW");
    assert_eq!(e.document().cursor(), 0); // back to "foo"
    // NW SEL extends the selection over the word + trailing space.
    e.run_command("NW SEL");
    assert_eq!(e.document().selection_text(), "foo ");
    // PC/NC are char synonyms for LEFT/RIGHT.
    e.run_command("TOP");
    e.run_command("NC");
    assert_eq!(e.document().cursor(), 1);
}

#[test]
fn sentence_motion_ns_ps() {
    let (mut e, _c) = sample_editor();
    e.run_command(r#"INSERT "One. Two. Three.""#);
    e.run_command("TOP");
    e.run_command("NS");
    assert_eq!(e.document().cursor(), 5); // start of "Two."
    e.run_command("NS");
    assert_eq!(e.document().cursor(), 10); // start of "Three."
    e.run_command("PS");
    assert_eq!(e.document().cursor(), 5); // back to "Two."
    e.run_command("PS");
    assert_eq!(e.document().cursor(), 0); // back to "One."
    assert!(e.last_error().is_none());
}

#[test]
fn ctrl_shift_arrow_selects_by_word() {
    // Ctrl+Shift+Right resolves through the new CTRL+SHIFT KBD table to `NW SEL`.
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"alpha beta gamma\"");
    e.run_command("TOP");
    e.feed_key_mods(103, true, true, false); // Ctrl+Shift+Right
    assert_eq!(e.document().selection_text(), "alpha ");
    e.feed_key_mods(103, true, true, false); // extend by another word
    assert_eq!(e.document().selection_text(), "alpha beta ");
    // Ctrl+Shift+Left shrinks the selection back by a word.
    e.feed_key_mods(101, true, true, false);
    assert_eq!(e.document().selection_text(), "alpha ");
}

#[test]
fn wrap_aware_char_motion_cl_cr_ll_lr() {
    let (mut e, _c) = sample_editor();
    e.run_command(r#"INSERT "ab\ncd""#); // a0 b1 \n2 c3 d4
    e.run_command("JMP 3"); // start of line 1 ("cd")
    e.run_command("LL"); // no-wrap left at line start → stays
    assert_eq!(e.document().cursor(), 3);
    e.run_command("CL"); // wrap left → end of previous line
    assert_eq!(e.document().cursor(), 2);
    e.run_command("LR"); // no-wrap right at line end → stays
    assert_eq!(e.document().cursor(), 2);
    e.run_command("CR"); // wrap right → start of next line
    assert_eq!(e.document().cursor(), 3);
}

#[test]
fn viewport_page_and_screen_motion() {
    let (mut e, _c) = sample_editor();
    e.run_command(r#"INSERT "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9""#);
    e.run_command("TOP");
    // 4 visible rows → a page is 3 lines.
    e.host_mut().set_viewport(0, 4);
    e.run_command("PD");
    assert_eq!(e.document().line(), 4); // line index 3 (1-based)
    e.run_command("PU");
    assert_eq!(e.document().line(), 1);
    // Screen top/bottom from a scrolled viewport (top line 2, 4 rows → bottom 5).
    e.host_mut().set_viewport(2, 4);
    e.run_command("HM");
    assert_eq!(e.document().line(), 3); // top visible line (index 2)
    e.run_command("BS");
    assert_eq!(e.document().line(), 6); // bottom visible line (index 5)
    assert!(e.last_error().is_none());
}

#[test]
fn paragraph_and_line_and_jmp_motion() {
    let (mut e, _c) = sample_editor();
    e.run_command(r#"INSERT "para one\n\npara two\n\npara three""#);
    e.run_command("TOP");
    e.run_command("NP");
    assert_eq!(e.document().cursor(), 10); // start of "para two"
    e.run_command("NP");
    assert_eq!(e.document().cursor(), 20); // start of "para three"
    e.run_command("PP");
    assert_eq!(e.document().cursor(), 10); // back to "para two"
    // JMP to an absolute character offset.
    e.run_command("JMP 3");
    assert_eq!(e.document().cursor(), 3);
    // Relative JMP: + forward, - back, from the current position.
    e.run_command("JMP +5");
    assert_eq!(e.document().cursor(), 8);
    e.run_command("JMP -2");
    assert_eq!(e.document().cursor(), 6);
    // Out of range clamps silently to the document edges (no error).
    e.run_command("JMP -9999");
    assert_eq!(e.document().cursor(), 0);
    e.run_command("JMP +999999");
    assert_eq!(e.document().cursor(), e.document().text().len());
    assert!(e.last_error().is_none());
    // NL/PL move to the start of the next/previous line.
    e.run_command("TOP");
    e.run_command("NL");
    assert_eq!(e.document().cursor(), 9); // start of the blank line 1
    assert!(e.last_error().is_none());
}

#[test]
fn ctrl_arrow_word_motion_via_kbd() {
    // Ctrl+Right (77 in the CTRL layer) → NW; the sample KBD wires the modifier.
    let (mut editor, ctrl) = sample_editor();
    editor.run_command("INSERT \"alpha beta\"");
    editor.run_command("TOP");
    editor.feed_key(KeyEvent::inject(103, ctrl)); // Ctrl+Right
    assert_eq!(editor.document().cursor(), 6); // start of "beta"
    editor.feed_key(KeyEvent::inject(101, ctrl)); // Ctrl+Left
    assert_eq!(editor.document().cursor(), 0); // back to "alpha"
}

#[test]
fn delete_char_aliases_bd_rc() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"abcd\"");
    e.run_command("TOP");
    e.run_command("RC"); // rub-out (forward) 'a'
    assert_eq!(e.document().text(), "bcd");
    e.run_command("END");
    e.run_command("BD"); // backspace 'd'
    assert_eq!(e.document().text(), "bc");
    assert!(e.last_error().is_none());
}

#[test]
fn whole_unit_deletes_word_line_sentence_paragraph() {
    // RW — the word the cursor is in (+ a trailing space).
    let (mut w, _c1) = sample_editor();
    w.run_command("INSERT \"foo bar baz\"");
    w.run_command("JMP 5"); // inside "bar"
    w.run_command("RW");
    assert_eq!(w.document().text(), "foo baz");

    // RL — the whole line, including its break.
    let (mut l, _c2) = sample_editor();
    l.run_command(r#"INSERT "one\ntwo\nthree""#);
    l.run_command("JMP 5"); // inside "two"
    l.run_command("RL");
    assert_eq!(l.document().text(), "one\nthree");

    // RS — the whole sentence (+ trailing space).
    let (mut s, _c3) = sample_editor();
    s.run_command(r#"INSERT "One. Two. Three.""#);
    s.run_command("JMP 6"); // inside "Two."
    s.run_command("RS");
    assert_eq!(s.document().text(), "One. Three.");

    // RP — the whole paragraph (+ following blank line).
    let (mut p, _c4) = sample_editor();
    p.run_command(r#"INSERT "p1a\np1b\n\np2""#);
    p.run_command("TOP"); // in paragraph 1
    p.run_command("RP");
    assert_eq!(p.document().text(), "p2");
}

#[test]
fn delete_to_end_of_line_re() {
    let (mut e, _c) = sample_editor();
    e.run_command(r#"INSERT "hello world\nsecond""#);
    e.run_command("JMP 5"); // just after "hello"
    e.run_command("RE"); // delete " world" → end of line
    assert_eq!(e.document().text(), "hello\nsecond");
    e.run_command("RE"); // at EOL: eat the break, joining the next line up
    assert_eq!(e.document().text(), "hellosecond");
    e.run_command("UNDO");
    assert_eq!(e.document().text(), "hello\nsecond");
    assert!(e.last_error().is_none());
}

#[test]
fn move_active_to_offset_with_extend() {
    // The engine hook behind the GUI's visual (wrapped-line) Up/Down.
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"hello\"");
    e.run_command("TOP");
    e.host_mut().move_active_to(3, false); // plain move → caret at 3, no selection
    assert_eq!(e.document().cursor(), 3);
    assert!(e.document().selection_span().is_none());
    e.host_mut().move_active_to(5, true); // extend → selects [3,5)
    assert_eq!(e.document().selection_text(), "lo");
    assert_eq!(e.document().cursor(), 5);
}

#[test]
fn define_unit_verbs_select_word_line_sentence_paragraph() {
    // DW — word content only (no trailing space).
    let (mut w, _c1) = sample_editor();
    w.run_command("INSERT \"foo bar baz\"");
    w.run_command("JMP 5"); // inside "bar"
    w.run_command("DW");
    assert_eq!(w.document().selection_text(), "bar");

    // DL — the whole line including its break (a movable line unit).
    let (mut l, _c2) = sample_editor();
    l.run_command(r#"INSERT "one\ntwo\nthree""#);
    l.run_command("JMP 5"); // inside "two"
    l.run_command("DL");
    assert_eq!(l.document().selection_text(), "two\n");

    // DS — the sentence (no trailing space, keeps the terminator).
    let (mut s, _c3) = sample_editor();
    s.run_command(r#"INSERT "One. Two. Three.""#);
    s.run_command("JMP 6"); // inside "Two."
    s.run_command("DS");
    assert_eq!(s.document().selection_text(), "Two.");

    // DP — the paragraph's lines (no trailing blank line).
    let (mut p, _c4) = sample_editor();
    p.run_command(r#"INSERT "p1a\np1b\n\np2""#);
    p.run_command("TOP");
    p.run_command("DP");
    assert_eq!(p.document().selection_text(), "p1a\np1b");
}

#[test]
fn define_unit_plus_variants_include_trailing_separator() {
    // DW+ — word plus its trailing space.
    let (mut w, _c1) = sample_editor();
    w.run_command("INSERT \"foo bar baz\"");
    w.run_command("JMP 5"); // inside "bar"
    w.run_command("DW+");
    assert_eq!(w.document().selection_text(), "bar ");

    // DS+ — sentence plus its trailing space.
    let (mut s, _c2) = sample_editor();
    s.run_command(r#"INSERT "One. Two. Three.""#);
    s.run_command("JMP 6"); // inside "Two."
    s.run_command("DS+");
    assert_eq!(s.document().selection_text(), "Two. ");

    // DP+ — paragraph plus the separating blank line.
    let (mut p, _c3) = sample_editor();
    p.run_command(r#"INSERT "p1a\np1b\n\np2""#);
    p.run_command("TOP");
    p.run_command("DP+");
    assert_eq!(p.document().selection_text(), "p1a\np1b\n\n");
    assert!(p.last_error().is_none());
}

#[test]
fn define_word_is_a_block_then_act() {
    // A defined word persists across plain motion (it's a defined block) and can be
    // acted on — e.g. uppercased in place.
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"foo bar baz\"");
    e.run_command("JMP 5"); // inside "bar"
    e.run_command("DW");
    e.run_command("UC"); // uppercase the defined word
    assert_eq!(e.document().text(), "foo BAR baz");
    assert!(e.last_error().is_none());
}

#[test]
fn ctrl_c_copies_selection_to_clipboard() {
    // Ctrl+C (key 46 in the CTRL layer) dispatches the COPY verb → `$CB`. Uses a SELF-CONTAINED
    // keymap rather than the live sample (whose bindings the user customizes), so this tests the
    // resolve→COPY→$CB mechanism, not a particular .kbd file.
    const KBD: &str = "KEYS=104\nCTRL=29\nTABLE=\n1=KF\nTABLE=CTRL\n46=COPY\n";
    let km = match load(KBD, Some(120)) {
        LoadOutcome::Installed(km) => km,
        LoadOutcome::Rejected(e) => panic!("test kbd failed to load: {e:?}"),
    };
    let ctrl = km.bitmask_for_names(&["CTRL"]).expect("CTRL is a declared modifier");
    let mut editor = Editor::new(km);
    editor.run_command("INSERT \"hello\"");
    editor.run_command("DFA"); // select all
    match editor.feed_key(KeyEvent::inject(46, ctrl)) {
        Disposition::Dispatch(_) => {}
        other => panic!("expected Dispatch for Ctrl+C, got {other:?}"),
    }
    editor.run_command("PR $CB");
    assert_eq!(editor.output().last().map(String::as_str), Some("hello"));
}

#[test]
fn clip_quoted_arg_protects_semicolons_and_decodes_escaped_quotes() {
    let (mut e, _c) = sample_editor();
    // A `;` inside CLIP's quoted argument is part of the text, not a command separator —
    // and chaining resumes after the closing quote.
    e.run_command(r#"CLIP "a;b"; PR $CB"#);
    assert_eq!(e.output().last().map(String::as_str), Some("a;b"));
    // Internal quotes escaped as \" are decoded.
    e.run_command(r#"CLIP "she said \"hi\""; PR $CB"#);
    assert_eq!(e.output().last().map(String::as_str), Some(r#"she said "hi""#));
    // A bare (unquoted) argument is still taken verbatim; the trailing `; PR` now chains.
    e.run_command("CLIP plain; PR $CB");
    assert_eq!(e.output().last().map(String::as_str), Some("plain"));
}

#[test]
fn clipboard_cut_copy_paste_append_prepend() {
    let (mut e, _c) = sample_editor();
    // CLIP sets $CB literally.
    e.run_command("CLIP hello");
    e.run_command("PR $CB");
    assert_eq!(e.output().last().map(String::as_str), Some("hello"));

    // COPY the selection, then PASTE it elsewhere.
    e.run_command("INSERT \"world\"");
    e.run_command("DFA");
    e.run_command("COPY"); // $CB = "world"
    e.run_command("XD"); // clear selection
    e.run_command("BOTTOM");
    e.run_command("PASTE"); // insert at end
    assert_eq!(e.document().text(), "worldworld");

    // CUT the whole document to the clipboard.
    e.run_command("DFA");
    e.run_command("CUT");
    assert_eq!(e.document().text(), "");
    e.run_command("PR $CB");
    assert_eq!(e.output().last().map(String::as_str), Some("worldworld"));

    // APPEND / PREPEND extend the clipboard with the selection.
    let (mut a, _c2) = sample_editor();
    a.run_command("CLIP X");
    a.run_command("INSERT \"ab\"");
    a.run_command("DFA");
    a.run_command("APPEND"); // "X" + "ab"
    a.run_command("PR $CB");
    assert_eq!(a.output().last().map(String::as_str), Some("Xab"));
    a.run_command("PREPEND"); // "ab" + "Xab"
    a.run_command("PR $CB");
    assert_eq!(a.output().last().map(String::as_str), Some("abXab"));
    assert!(a.last_error().is_none());
}

#[test]
fn kf_key_is_released_to_the_os() {
    let (mut editor, _ctrl) = sample_editor();
    // Esc (1) is a KF cell in every layer.
    assert_eq!(editor.feed_key(KeyEvent::down(1)), Disposition::Flip);
}

#[test]
fn loadkbd_command_cell_reloads_the_layout() {
    // The reload-via-keymap MECHANISM, independent of whatever key the shipped layout happens to bind
    // to `loadkbd` (it need not bind one at all). A throwaway keymap binds scan 38 to `loadkbd <file>`;
    // pressing it swaps the layout live, read offline from a virtual file.
    let reloader = "KEYS=104\nTABLE=\n38=loadkbd xyscript-reloaded.kbd\n";
    let km = match load(reloader, Some(120)) {
        LoadOutcome::Installed(km) => km,
        LoadOutcome::Rejected(e) => panic!("reloader kbd failed to load: {e:?}"),
    };
    let mut editor = Editor::new(km);
    // The layout the reload will install: key 16 inserts "Z" (a virtual file, so it's deterministic).
    editor
        .host_mut()
        .files
        .insert("xyscript-reloaded.kbd".into(), "KEYS=104\nTABLE=\n16=insert \"Z\"\n".into());
    // Before the swap, key 16 is unbound in the reloader map → nothing lands.
    editor.feed_key(KeyEvent::down(16));
    assert_eq!(editor.document().text(), "", "key 16 is unbound before the reload");
    // Press the loadkbd key (scan 38) → it must Dispatch, and reload without error.
    match editor.feed_key(KeyEvent::down(38)) {
        Disposition::Dispatch(_) => {}
        other => panic!("expected Dispatch for the loadkbd key, got {other:?}"),
    }
    assert!(editor.last_error().is_none(), "reload failed: {:?}", editor.last_error());
    // After the swap, key 16 now inserts "Z" — proving the layout changed live.
    editor.feed_key(KeyEvent::down(16));
    assert_eq!(editor.document().text(), "Z");
}

// ---- CALC calculator command -------------------------------------------------

#[test]
fn calc_expr_arg_reports_and_copies() {
    // Mode 1: `CALC expr` evaluates the argument, reports on the status line, copies
    // the result to the clipboard ($CB), and records it in $CALC. The document is
    // untouched.
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"keep me\"");
    e.run_command("CALC 6 * 7");
    assert_eq!(e.document().text(), "keep me", "Mode 1 must not edit the document");
    assert_eq!(e.special_str("CB"), "42", "result is copied to the clipboard");
    assert_eq!(e.special_str("CALC"), "42", "$CALC reads back the last result");
    assert!(e.last_error().is_none());
}

#[test]
fn calc_no_arg_on_a_line_appends_result_below() {
    // Mode 2: `CALC` with the caret on a text line evaluates the line and drops the
    // result on a fresh line right below it. Mode 2 does NOT touch the clipboard.
    let (mut e, _c) = sample_editor();
    e.run_command("CLIP untouched"); // seed $CB so we can prove Mode 2 leaves it alone
    e.run_command("INSERT \"3 + 4 * 2\""); // caret rests at end of the only line
    e.run_command("CALC");
    assert_eq!(nl(&e.document().text()), "3 + 4 * 2\n11");
    assert_eq!(e.special_str("CALC"), "11");
    assert_eq!(e.special_str("CB"), "untouched", "Mode 2 must not copy to the clipboard");
    assert!(e.last_error().is_none());
}

#[test]
fn calc_no_arg_over_a_selection_sums_the_lines() {
    // Mode 3: with lines selected, evaluate each and put the SUM on a new line just
    // below the selection. Blank lines are skipped.
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"10\"; ENTER; INSERT \"20\"; ENTER; INSERT \"12\""); // three numeric lines
    e.run_command("DFA"); // select the whole document
    e.run_command("CALC");
    assert_eq!(nl(&e.document().text()), "10\n20\n12\n42");
    assert_eq!(e.special_str("CALC"), "42");
    assert!(e.last_error().is_none());
}

#[test]
fn calc_sum_evaluates_each_line_as_an_expression() {
    // Mode 3 sums the *evaluated* value of every selected line, not the raw text.
    let (mut e, _c) = sample_editor();
    // a blank line in the middle is skipped
    e.run_command("INSERT \"2 * 3\"; ENTER; INSERT \"4 + 1\"; ENTER; ENTER; INSERT \"10 / 2\"");
    e.run_command("DFA");
    e.run_command("CALC");
    // 6 + 5 + 5 = 16
    assert!(nl(&e.document().text()).ends_with("\n16"), "got {:?}", e.document().text());
    assert!(e.last_error().is_none());
}

#[test]
fn calc_bad_expression_trips_an_error_and_leaves_the_doc_alone() {
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"not math here\"");
    e.run_command("CALC");
    assert_eq!(e.document().text(), "not math here", "a failed CALC makes no edit");
    assert!(e.status_message().map(|s| s.starts_with("CALC:")).unwrap_or(false));
}

#[test]
fn calc_on_the_command_line_with_no_selection_prompts() {
    // `CALC` typed on the command line (focus there, nothing selected) can't guess a
    // line to evaluate — it emits guidance instead of editing the document.
    let (mut e, _c) = sample_editor();
    e.run_command("INSERT \"123\"");
    e.run_command("BC CALC"); // put "CALC" on the cmline and focus it
    e.submit_cmdline(); // run it as a genuine command-line submission
    assert_eq!(nl(&e.document().text()), "123", "guidance path must not edit the document");
    assert!(
        e.status_message().map(|s| s.contains("put the cursor on a line")).unwrap_or(false),
        "expected guidance, got {:?}",
        e.status_message()
    );
}

// ---- Cursor addition: CLRSUM / ADDSUM / PUTSUM + $RUNSUM -------------------------

#[test]
fn sum_clrsum_addsum_putsum_flow() {
    // CLRSUM resets, ADDSUM adds the number under the caret, $RUNSUM reads the running total,
    // and PUTSUM writes it into the document.
    let (mut e, _c) = sample_editor();
    e.run_command("CLRSUM");
    assert_eq!(e.special_str("RUNSUM"), "0");
    e.insert_literal("10"); // caret lands at the end of the number
    e.run_command("ADDSUM");
    assert_eq!(e.special_str("RUNSUM"), "10");
    e.run_command("ENTER");
    e.insert_literal("20");
    e.run_command("ADDSUM");
    assert_eq!(e.special_str("RUNSUM"), "30");
    e.run_command("ENTER");
    e.run_command("PUTSUM");
    assert!(nl(&e.document().text()).ends_with("\n30"), "PUTSUM writes the total: {:?}", e.document().text());
    assert!(e.last_error().is_none());
    // CLRSUM starts over.
    e.run_command("CLRSUM");
    assert_eq!(e.special_str("RUNSUM"), "0");
}

#[test]
fn sum_strips_currency_and_thousands_and_sums_floats() {
    // A leading currency sign and thousands-separators are stripped; a leading `-` is kept; floats and
    // integers mix. DECIMALS 2 keeps the float display exact.
    let (mut e, _c) = sample_editor();
    e.run_command("DECIMALS 2");
    e.run_command("CLRSUM");
    e.insert_literal("$1,234.56");
    e.run_command("ADDSUM");
    assert_eq!(e.special_str("RUNSUM"), "1234.56");
    e.run_command("ENTER");
    e.insert_literal("-42");
    e.run_command("ADDSUM");
    assert_eq!(e.special_str("RUNSUM"), "1192.56");
    e.run_command("ENTER");
    e.insert_literal("3,000");
    e.run_command("ADDSUM");
    assert_eq!(e.special_str("RUNSUM"), "4192.56");
}

#[test]
fn sum_addsum_reads_the_number_the_caret_is_within() {
    // The caret need only be ON the number (here in the middle of it), not at its start.
    let (mut e, _c) = sample_editor();
    e.run_command("CLRSUM");
    e.insert_literal("500 apples");
    e.run_command("TOP"); // caret to the "5"
    e.run_command("RIGHT");
    e.run_command("RIGHT"); // caret now between the second "0" and the space — still within "500"
    e.run_command("ADDSUM");
    assert_eq!(e.special_str("RUNSUM"), "500");
}

#[test]
fn sum_addsum_errors_off_a_number_and_leaves_the_total() {
    let (mut e, _c) = sample_editor();
    e.run_command("CLRSUM");
    e.insert_literal("hello");
    e.run_command("ADDSUM");
    assert!(
        e.status_message().map(|s| s.starts_with("ADDSUM:")).unwrap_or(false),
        "expected an ADDSUM error, got {:?}",
        e.status_message()
    );
    assert_eq!(e.special_str("RUNSUM"), "0", "a failed ADDSUM leaves the total untouched");
}
