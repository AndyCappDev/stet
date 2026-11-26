// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `restore` must not free what is still in use (issue #46).
//!
//! PLRM 3e, `restore`: "If any of these stacks contains composite objects
//! whose values reside in local VM and are newer than the snapshot being
//! restored, an invalidrestore error occurs." A running procedure, a loop's
//! procedure and source, and the objects an operator holds while it calls
//! back into PostScript are all on Ghostscript's execution stack. stet keeps
//! several of them elsewhere, where the check did not look, and the freed
//! object's next use panicked.
//!
//! Each program leaves one name: the error a `restore` raised, or `/ok`.
//! Every expectation was checked against Ghostscript 10.05 except the two
//! scratch-string cases (see their comments).
//!
//! The error-path cases check that an error inside a callback leaves the
//! graphics state as it found it, which Ghostscript does.

use stet::Interpreter;
use stet_core::object::PsValue;

/// Run `setup`, then `/sv save def`, then `body`, and return the name left
/// on top of the operand stack.
///
/// The job runs on the null output device, installed as the CLI installs it
/// for `--device null`, so that `restore` and `showpage` have a page device
/// with an EndPage procedure to run, as on any real device.
fn outcome(setup: &str, body: &str) -> String {
    let mut interp = Interpreter::builder().suppress_output().build();
    let ctx = interp.context();
    let source = format!(
        "/null /OutputDevice findresource setpagedevice\n\
         /t_try {{ stopped {{ $error /errorname get }} {{ /ok }} ifelse }} bind def\n\
         {setup}\n/sv save def\n{body}\n"
    );
    stet_engine::eval::parse_and_exec(ctx, source.as_bytes())
        .unwrap_or_else(|e| panic!("job failed: {e:?}\nsource:\n{source}"));
    let top = ctx.o_stack.peek(0).expect("a result on the operand stack");
    match top.value {
        PsValue::Name(id) => String::from_utf8_lossy(ctx.names.get_bytes(id)).into_owned(),
        other => panic!("expected a name, got {other:?}"),
    }
}

/// A folder of three files for the `filenameforall` cases, removed when
/// dropped, including when the test panics.
struct ThreeFiles(std::path::PathBuf);

impl ThreeFiles {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("stet-restore-in-use-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        Self(dir)
    }

    /// A `filenameforall` template matching the three files, as the contents
    /// of a PostScript string. Written with `/`, since a Windows path's
    /// backslashes are escapes in a PostScript string.
    fn template(&self) -> String {
        format!(
            "{}/*",
            self.0
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        )
    }
}

impl Drop for ThreeFiles {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

const HELVETICA: &str = "/Helvetica findfont 10 scalefont setfont 0 0 moveto";

/// A Type 3 font whose BuildChar runs `T3BC`, set at 10 points.
const TYPE3: &str = r#"/T3 8 dict dup begin
  /FontType 3 def /FontMatrix [0.001 0 0 0.001 0 0] def /FontBBox [0 0 1000 1000] def
  /Encoding 256 array def 0 1 255 { Encoding exch /.notdef put } for
  Encoding 97 /a put Encoding 98 /b put
  /BuildChar { T3BC } def
end definefont pop
/T3 findfont 10 scalefont setfont 0 0 moveto"#;

/// `restore` in the middle of a procedure made after the save: the procedure is
/// still running.
#[test]
fn inline_restore() {
    assert_eq!(outcome("", "{ sv restore 1 pop } t_try"), "invalidrestore");
}

/// `restore` as a procedure's last element: the procedure has finished, so the
/// restore is legal.
#[test]
fn tail_restore_is_legal() {
    assert_eq!(outcome("", "{ sv restore } t_try"), "ok");
}

/// An outer procedure made after the save is still running when an inner one
/// restores.
#[test]
fn nested_cursor() {
    assert_eq!(
        outcome("", "{ { sv restore } exec 1 pop } t_try"),
        "invalidrestore"
    );
}

/// As `inline_restore`, with the procedure reached through a name.
#[test]
fn name_called_proc() {
    assert_eq!(
        outcome("", "/p { sv restore 1 pop } def { p } t_try"),
        "invalidrestore"
    );
}

/// `repeat` keeps its procedure for the next iteration.
#[test]
fn repeat_proc() {
    assert_eq!(
        outcome("", "{ { sv restore } 2 exch repeat } t_try"),
        "invalidrestore"
    );
}

/// `for` keeps its procedure for the next iteration.
#[test]
fn for_proc() {
    assert_eq!(
        outcome("", "{ 1 1 2 { pop sv restore } for } t_try"),
        "invalidrestore"
    );
}

/// `loop` keeps its procedure for the next iteration.
#[test]
fn loop_proc() {
    assert_eq!(
        outcome("", "{ { sv restore } loop } t_try"),
        "invalidrestore"
    );
}

/// `forall` keeps the array it is walking.
#[test]
fn forall_array_source() {
    assert_eq!(
        outcome(
            "/p { pop sv restore } def",
            "{ [1 2 3] /p load forall } t_try"
        ),
        "invalidrestore"
    );
}

/// `forall` keeps the string it is walking.
#[test]
fn forall_string_source() {
    assert_eq!(
        outcome(
            "/p { pop sv restore } def",
            "{ (abc) /p load forall } t_try"
        ),
        "invalidrestore"
    );
}

/// `forall` keeps the dictionary it is walking.
#[test]
fn forall_dict_source() {
    assert_eq!(
        outcome(
            "/p { pop pop sv restore } def",
            "{ 3 dict dup /a 1 put dup /b 2 put /p load forall } t_try"
        ),
        "invalidrestore"
    );
}

/// `forall` over objects older than the save: nothing newer is in use, so the
/// restore is legal.
#[test]
fn forall_old_source_is_legal() {
    assert_eq!(
        outcome(
            "/a [1] def /p { pop sv restore } def",
            "{ a /p load forall } t_try"
        ),
        "ok"
    );
}

/// `pathforall` keeps its four procedures.
#[test]
fn pathforall_procs() {
    assert_eq!(
        outcome(
            "",
            "{ newpath 0 0 moveto 1 1 lineto 2 2 lineto {pop pop} {pop pop sv restore} {} {} pathforall } t_try"
        ),
        "invalidrestore"
    );
}

/// `image` calls its data procedure again for the next row.
#[test]
fn image_data_proc() {
    assert_eq!(
        outcome(
            "/s1 1 string def",
            "{ 2 1 8 [2 0 0 1 0 0] { s1 sv restore } image } t_try"
        ),
        "invalidrestore"
    );
}

/// `colorimage` with one procedure per component keeps the others while one
/// runs.
#[test]
fn colorimage_other_procs() {
    assert_eq!(
        outcome(
            "/s1 1 string def",
            "{ 1 1 8 [1 0 0 1 0 0] {s1} {s1 sv restore} {s1} true 3 colorimage } t_try"
        ),
        "invalidrestore"
    );
}

/// `kshow` keeps its procedure and the string it is showing.
#[test]
fn kshow_proc_and_string() {
    assert_eq!(
        outcome(HELVETICA, "{ { pop pop sv restore } (abc) kshow } t_try"),
        "invalidrestore"
    );
}

/// `cshow` keeps its procedure and the string it is showing.
#[test]
fn cshow_proc_and_string() {
    assert_eq!(
        outcome(
            HELVETICA,
            "{ { pop pop pop sv restore } (abc) cshow } t_try"
        ),
        "invalidrestore"
    );
}

/// `show` keeps the string while a Type 3 BuildChar runs.
#[test]
fn type3_string() {
    assert_eq!(
        outcome(
            &["/T3BC { pop pop 500 0 setcharwidth sv restore } def", TYPE3].join("\n"),
            "{ (ab) show } t_try"
        ),
        "invalidrestore"
    );
}

/// `settransfer` samples its procedure 256 times.
#[test]
fn settransfer_proc() {
    assert_eq!(
        outcome("", "{ { pop 0 sv restore } settransfer } t_try"),
        "invalidrestore"
    );
}

/// `setscreen` samples its spot function many times.
#[test]
fn setscreen_spot() {
    assert_eq!(
        outcome("", "{ 72 45 { pop pop 0 sv restore } setscreen } t_try"),
        "invalidrestore"
    );
}

/// `setcolorspace` samples a Separation tint transform many times.
#[test]
fn separation_tint() {
    assert_eq!(
        outcome(
            "",
            "{ [/Separation /Foo /DeviceGray { pop 0 sv restore }] setcolorspace 0.5 setcolor } t_try"
        ),
        "invalidrestore"
    );
}

/// `resourceforall` keeps its procedure and scratch string.
#[test]
fn resourceforall_proc() {
    assert_eq!(
        outcome(
            "",
            "{ (*) { pop sv restore } 100 string /Encoding resourceforall } t_try"
        ),
        "invalidrestore"
    );
}

/// A filter keeps reading from its procedure data source, through a filter made
/// after the save.
#[test]
fn drain_proc_source() {
    assert_eq!(
        outcome(
            "/d (41>) def",
            "{ { d sv restore } /ASCIIHexDecode filter token } t_try"
        ),
        "invalidrestore"
    );
}

/// `execform` keeps the form dictionary while PaintProc runs.
#[test]
fn execform_dict() {
    assert_eq!(
        outcome(
            "",
            "{ << /FormType 1 /BBox [0 0 1 1] /Matrix [1 0 0 1 0 0] /PaintProc { pop sv restore } >> execform } t_try"
        ),
        "invalidrestore"
    );
}

/// A pattern's PaintProc runs while the pattern dictionary is in use.
#[test]
fn makepattern_dict() {
    assert_eq!(
        outcome(
            "",
            "{ << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1 /PaintProc { pop sv restore } >> matrix makepattern setpattern 0 0 10 10 rectfill } t_try"
        ),
        "invalidrestore"
    );
}

/// What EndPage leaves on the operand stack when `restore` runs it is
/// discarded, as Ghostscript does, rather than freed in place.
#[test]
fn endpage_leaves_array() {
    assert_eq!(
        outcome(
            "",
            "<< /EndPage { pop pop 1 array true } >> setpagedevice { sv restore } t_try count 1 eq not { /stackleft } if"
        ),
        "ok"
    );
}

/// A dictionary EndPage leaves on the dictionary stack is still in use when
/// `restore` runs it.
#[test]
fn endpage_leaves_dict() {
    assert_eq!(
        outcome(
            "",
            "<< /EndPage { pop pop 1 dict begin true } >> setpagedevice { sv restore } t_try"
        ),
        "invalidrestore"
    );
}

/// EndPage run by a restore may leave objects older than the save: they are
/// discarded with the rest, as in Ghostscript.
#[test]
fn endpage_leaves_older_objects() {
    assert_eq!(
        outcome(
            "/old 1 array def",
            "<< /EndPage { pop pop old 5 true } >> setpagedevice { sv restore } t_try \
             count 1 eq not { /stackleft } if"
        ),
        "ok"
    );
}

/// The program's own operands under EndPage's stay.
#[test]
fn endpage_keeps_operands_below() {
    assert_eq!(
        outcome(
            "",
            "<< /EndPage { pop pop 9 true } >> setpagedevice 7 sv { restore } t_try \
             exch 7 eq { count 1 eq { } { pop /stackleft } ifelse } { pop /lost } ifelse"
        ),
        "ok"
    );
}

/// `cvs`, `cvrs` and `join` (a stet extension) write into the program's
/// string, which `restore` must revert like any other change to a string
/// older than the save (PLRM 3.7.3; stet's `putinterval` does). They wrote
/// without a backup, so the restore kept what they wrote. Ghostscript reverts
/// no string contents on `restore`, so the PLRM decides these.
#[test]
fn string_writes_revert() {
    for write in [
        "12 s cvs pop",
        "255 16 s cvrs pop",
        "(-) [(a) (b)] s join pop",
    ] {
        assert_eq!(
            outcome(
                "/s (XXXXXX) def",
                &format!("{write} sv restore s (XXXXXX) eq {{ /ok }} {{ /notreverted }} ifelse")
            ),
            "ok",
            "{write}"
        );
    }
}

/// `filenameforall` keeps its procedure and scratch string.
#[test]
fn filenameforall_proc() {
    let dir = ThreeFiles::new("filenameforall_proc");
    assert_eq!(
        outcome(
            "",
            &format!(
                "{{ ({}) {{ pop sv restore }} 512 string filenameforall }} t_try",
                dir.template()
            )
        ),
        "invalidrestore"
    );
}

/// A refused `restore` leaves its save object on the operand stack (PLRM: the
/// stacks are not altered on an error).
#[test]
fn inline_restore_keeps_operand() {
    assert_eq!(
        outcome(
            "",
            "{ sv restore 1 pop } stopped pop dup type /savetype eq exch pop { /ok } { /lost } ifelse"
        ),
        "ok"
    );
}

/// `resourceforall`'s writes into a scratch string made before the save are
/// undone by `restore`, as `putinterval`'s are. (Ghostscript reverts no string
/// contents at all, so it is no oracle here; this is PLRM 3.7.3.)
#[test]
fn resourceforall_scratch_reverts() {
    assert_eq!(
        outcome(
            "/scr (XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX) def",
            "(*) {pop} scr /Encoding resourceforall sv restore scr (XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX) eq { /ok } { /notreverted } ifelse"
        ),
        "ok"
    );
}

/// `filenameforall`'s writes into a scratch string made before the save are
/// undone by `restore`. (See `resourceforall_scratch_reverts`.)
#[test]
fn filenameforall_scratch_reverts() {
    let dir = ThreeFiles::new("filenameforall_scratch_reverts");
    let template = dir.template();
    // Longer than any name the template matches, however long the temp
    // folder's path is (on macOS it is over 100 bytes).
    let scratch = "X".repeat(template.len() + 32);
    assert_eq!(
        outcome(
            &format!("/scr ({scratch}) def"),
            &format!(
                "({template}) {{pop}} scr filenameforall sv restore scr 0 1 getinterval (X) eq {{ /ok }} {{ /notreverted }} ifelse"
            )
        ),
        "ok"
    );
}

/// An error in a Type 3 BuildChar leaves the graphics state as it was before
/// `show`.
#[test]
fn type3_error_keeps_gstate() {
    assert_eq!(
        outcome(
            &[
                "/T3BC { pop pop nosuchname } def",
                TYPE3,
                "/m0 matrix currentmatrix def"
            ]
            .join("\n"),
            "{ (a) show } stopped pop true matrix currentmatrix 0 1 5 { 2 copy get m0 3 -1 roll get sub abs 1e-6 lt 3 -1 roll and exch } for pop { /ok } { /gstateleft } ifelse"
        ),
        "ok"
    );
}

/// An error in a form's PaintProc leaves the graphics state as it was before
/// `execform`.
#[test]
fn execform_error_keeps_gstate() {
    assert_eq!(
        outcome(
            "/m0 matrix currentmatrix def",
            "{ << /FormType 1 /BBox [0 0 1 1] /Matrix [2 0 0 2 0 0] /PaintProc { pop nosuchname } >> execform } stopped pop true matrix currentmatrix 0 1 5 { 2 copy get m0 3 -1 roll get sub abs 1e-6 lt 3 -1 roll and exch } for pop { /ok } { /gstateleft } ifelse"
        ),
        "ok"
    );
}

/// An error in a pattern's PaintProc leaves the graphics state as it was.
#[test]
fn makepattern_error_keeps_gstate() {
    assert_eq!(
        outcome(
            "/m0 matrix currentmatrix def",
            "{ << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1 /PaintProc { pop nosuchname } >> [2 0 0 2 0 0] makepattern setpattern 0 0 10 10 rectfill } stopped pop true matrix currentmatrix 0 1 5 { 2 copy get m0 3 -1 roll get sub abs 1e-6 lt 3 -1 roll and exch } for pop { /ok } { /gstateleft } ifelse"
        ),
        "ok"
    );
}

/// An error in a Type 3 BuildChar under `xshow` leaves the graphics state as it
/// was.
#[test]
fn type3_xshow_error_keeps_gstate() {
    assert_eq!(
        outcome(
            &[
                "/T3BC { pop pop nosuchname } def",
                TYPE3,
                "/m0 matrix currentmatrix def"
            ]
            .join("\n"),
            "{ (a) [10] xshow } stopped pop true matrix currentmatrix 0 1 5 { 2 copy get m0 3 -1 roll get sub abs 1e-6 lt 3 -1 roll and exch } for pop { /ok } { /gstateleft } ifelse"
        ),
        "ok"
    );
}

// --- Everything the program passed in is older than the save ---
//
// A restore inside a callback is still `invalidrestore` on Ghostscript 10.05
// when the procedure, the string, the font and the dictionaries all predate
// the save: each operator keeps working state of its own, allocated when it
// starts, on the execution stack. stet refuses any restore that crosses a
// callback (`stet_core::sync_frames`), which gives the same answer. One
// case per group of call sites.

/// The show family: string and procedure older than the save.
#[test]
fn kshow_older_operands() {
    assert_eq!(
        outcome(
            &format!("{HELVETICA}\n/s (ab) def /p {{ pop pop sv restore }} def"),
            "{ /p load s kshow } t_try"
        ),
        "invalidrestore"
    );
}

/// A Type 3 `BuildChar`, with font and string older than the save.
#[test]
fn type3_older_font_and_string() {
    assert_eq!(
        outcome(
            &format!("/T3BC {{ pop pop 500 0 setcharwidth sv restore }} def\n{TYPE3}\n/s (a) def"),
            "{ s show } t_try"
        ),
        "invalidrestore"
    );
}

/// A transfer procedure older than the save.
#[test]
fn settransfer_older_proc() {
    assert_eq!(
        outcome(
            "/p { pop 0 sv restore } def",
            "{ /p load settransfer } t_try"
        ),
        "invalidrestore"
    );
}

/// A halftone dictionary and its spot function older than the save.
#[test]
fn sethalftone_older_dict() {
    assert_eq!(
        outcome(
            "/p { pop pop 0 sv restore } def \
             /h << /HalftoneType 1 /Frequency 72 /Angle 45 /SpotFunction /p load >> def",
            "{ h sethalftone } t_try"
        ),
        "invalidrestore"
    );
}

/// A Separation colour space, with its tint transform, older than the save.
#[test]
fn separation_older_space() {
    assert_eq!(
        outcome(
            "/cs [/Separation /Foo /DeviceGray { pop 0 sv restore }] def",
            "{ cs setcolorspace 0.5 setcolor 0 0 1 1 rectfill } t_try"
        ),
        "invalidrestore"
    );
}

/// An Indexed colour space's lookup procedure older than the save.
#[test]
fn indexed_older_proc() {
    assert_eq!(
        outcome(
            "/cs [/Indexed /DeviceGray 1 { pop 0 sv restore }] def",
            "{ cs setcolorspace 1 setcolor 0 0 1 1 rectfill } t_try"
        ),
        "invalidrestore"
    );
}

/// An image data procedure and its data older than the save.
#[test]
fn image_older_proc() {
    assert_eq!(
        outcome(
            r"/s1 (\377\377) def /p { s1 sv restore } def /m [2 0 0 1 0 0] def",
            "{ 2 1 8 m /p load image } t_try"
        ),
        "invalidrestore"
    );
}

/// `resourceforall` with procedure and scratch string older than the save.
#[test]
fn resourceforall_older_operands() {
    assert_eq!(
        outcome(
            "/scr 100 string def /p { pop sv restore } def",
            "{ (StandardEncoding) /p load scr /Encoding resourceforall } t_try"
        ),
        "invalidrestore"
    );
}

/// A procedure data source older than the save.
#[test]
fn data_source_older_proc() {
    assert_eq!(
        outcome(
            "/d (41>) def /p { d sv restore } def",
            "{ /p load /ASCIIHexDecode filter read pop pop } t_try"
        ),
        "invalidrestore"
    );
}

/// `execform` with a form dictionary older than the save: the one case
/// where stet is stricter than Ghostscript. Ghostscript lets the restore
/// through, but then `execform`'s closing `grestore` pops past the graphics
/// state the restore reinstated, so the program carries on in a state none of
/// its own operators set (with no `gsave` before the save, the initial
/// default line width). stet refuses the restore, as at every other callback.
#[test]
fn execform_older_dict() {
    assert_eq!(
        outcome(
            "/f << /FormType 1 /BBox [0 0 1 1] /Matrix [1 0 0 1 0 0] \
             /PaintProc { pop sv restore } >> def",
            "{ f execform } t_try"
        ),
        "invalidrestore"
    );
}

// --- A procedure's graphics state stack, checked against Ghostscript 10.05 ---
//
// Each leaves the line width as a name: 5 is set before the operator, the
// procedure sets 6, makes a `gsave` of its own and sets 7.

const LINE_WIDTH: &str = "currentlinewidth cvi 10 string cvs cvn";

/// A Type 3 font whose glyph procedure sets 6, `gsave`s, sets 7, then runs
/// `tail`.
fn type3_unmatched_gsave(tail: &str) -> String {
    format!(
        "/T3BC {{ pop pop 500 0 setcharwidth 6 setlinewidth gsave 7 setlinewidth {tail} }} def\n{TYPE3}"
    )
}

/// A glyph procedure's unmatched `gsave` is undone with the operator's own,
/// whether it succeeds or fails, for every operator that runs one.
#[test]
fn type3_unmatched_gsave_is_undone() {
    for tail in ["", "nosuchname"] {
        for op in [
            "(a) show",
            "(a) [10] xshow",
            "(a) false charpath",
            "(a) stringwidth pop pop",
        ] {
            assert_eq!(
                outcome(
                    &type3_unmatched_gsave(tail),
                    &format!("5 setlinewidth {{ {op} }} t_try pop {LINE_WIDTH}")
                ),
                "5",
                "{op} with `{tail}`"
            );
        }
    }
}

/// A glyph procedure that pops a level too many: on success the operator's
/// closing `grestore` pops the program's level too; on error nothing more
/// is undone. Ghostscript does both.
#[test]
fn type3_extra_grestore() {
    for (tail, expected) in [("", "3"), ("nosuchname", "7")] {
        let setup = format!(
            "/T3BC {{ pop pop 500 0 setcharwidth grestore 7 setlinewidth {tail} }} def\n{TYPE3}"
        );
        assert_eq!(
            outcome(
                &setup,
                &format!(
                    "3 setlinewidth gsave 5 setlinewidth {{ (a) show }} t_try pop {LINE_WIDTH}"
                )
            ),
            expected,
            "`{tail}`"
        );
    }
}

/// `execform` does one `grestore` whether PaintProc succeeds or fails, so a
/// `gsave` PaintProc leaves unmatched stays, as in Ghostscript.
#[test]
fn execform_grestores_once() {
    for tail in ["", "nosuchname"] {
        assert_eq!(
            outcome(
                &format!(
                    "/f << /FormType 1 /BBox [0 0 1 1] /Matrix [1 0 0 1 0 0] \
                     /PaintProc {{ pop 6 setlinewidth gsave 7 setlinewidth {tail} }} >> def"
                ),
                &format!("5 setlinewidth {{ f execform }} t_try pop {LINE_WIDTH}")
            ),
            "6",
            "`{tail}`"
        );
    }
}

/// A `gsave` a pattern's PaintProc leaves unmatched does not surface at the
/// program's next `grestore`: it popped into the PaintProc's state.
#[test]
fn makepattern_unmatched_gsave_is_dropped() {
    assert_eq!(
        outcome(
            "/d << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1 \
             /PaintProc { pop gsave 7 setlinewidth } >> def /m matrix def",
            &format!(
                "3 setlinewidth gsave 5 setlinewidth d m makepattern pop grestore {LINE_WIDTH}"
            )
        ),
        "3"
    );
}
