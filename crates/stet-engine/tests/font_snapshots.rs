// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The interpreter keeps a copy of each font text is shown with, for an
//! output device that reads fonts after the page is sent (PDF output).
//!
//! The copy has to be taken before any `restore` can change the font: a
//! restore reclaims fonts defined inside its save — often before the page is
//! sent, as when an EPS figure is placed with `save … restore` — and reverts
//! glyphs a page added to a font defined outside it (incremental definition,
//! PLRM 3e §5.9.2).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use stet_core::context::{CidGlyphMetrics, Context};
use stet_core::device::{
    ClipParams, FillParams, ImageParams, OutputDevice, StrokeParams, TextParams,
};
use stet_core::dict::DictKey;
use stet_core::display_list::{DisplayElement, DisplayList};
use stet_core::font_snapshot::{Frozen, FrozenDict};
use stet_core::geometry::PsPath;
use stet_core::object::{EntityId, PsValue};

/// A device that keeps the pages it is sent, and asks for font copies when
/// `keeps_fonts` is set.
struct Pages {
    keeps_fonts: bool,
    pages: Rc<RefCell<Vec<DisplayList>>>,
}

impl OutputDevice for Pages {
    fn fill_path(&mut self, _: &PsPath, _: &FillParams) {}
    fn stroke_path(&mut self, _: &PsPath, _: &StrokeParams) {}
    fn clip_path(&mut self, _: &PsPath, _: &ClipParams) {}
    fn init_clip(&mut self) {}
    fn erase_page(&mut self) {}
    fn show_page(&mut self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn draw_image(&mut self, _: &[u8], _: &ImageParams) {}
    fn page_size(&self) -> (u32, u32) {
        (612, 792)
    }
    fn keeps_text_fonts(&self) -> bool {
        self.keeps_fonts
    }
    fn replay_and_show(&mut self, list: DisplayList, _: &str) -> Result<(), String> {
        self.pages.borrow_mut().push(list);
        Ok(())
    }
}

/// A context with the bundled fonts and a device that keeps its pages.
fn ctx(keeps_fonts: bool) -> (Context, Rc<RefCell<Vec<DisplayList>>>) {
    let mut ctx = Context::new();
    stet_ops::build_system_dict(&mut ctx);
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stet/resources/Font");
    ctx.font_resource_path = Some(fonts.canonicalize().unwrap().to_string_lossy().into_owned());
    let pages = Rc::new(RefCell::new(Vec::new()));
    ctx.device = Some(Box::new(Pages {
        keeps_fonts,
        pages: pages.clone(),
    }));
    ctx.output_path = Some("page".to_string());
    (ctx, pages)
}

fn run(ctx: &mut Context, src: &str) {
    stet_engine::eval::parse_and_exec(ctx, src.as_bytes())
        .unwrap_or_else(|e| panic!("PS execution failed: {e:?}\nsource:\n{src}"));
}

/// Every `Text` element of every page sent, in order.
fn texts(pages: &Rc<RefCell<Vec<DisplayList>>>) -> Vec<TextParams> {
    fn collect(list: &DisplayList, out: &mut Vec<TextParams>) {
        for element in list.elements() {
            if let DisplayElement::Text { params } = element {
                out.push(params.clone());
            }
        }
    }
    let mut out = Vec::new();
    for page in pages.borrow().iter() {
        collect(page, &mut out);
    }
    out
}

/// `font`'s value under the name `key`.
fn get<'a>(ctx: &Context, font: &'a FrozenDict, key: &str) -> Option<&'a Frozen> {
    font.get_name(ctx.names.find(key.as_bytes())?)
}

/// Whether `font`'s `CharStrings` has a glyph named `glyph`.
fn has_glyph(ctx: &Context, font: &FrozenDict, glyph: &str) -> bool {
    get(ctx, font, "CharStrings")
        .and_then(Frozen::as_dict)
        .is_some_and(|cs| get(ctx, cs, glyph).is_some())
}

/// A copy of Times-Roman named `name` whose `CharStrings` has only
/// `.notdef` and the glyphs `glyphs`, defined as `/name`.
fn subset_font(name: &str, glyphs: &str) -> String {
    format!(
        "/Base /Times-Roman findfont def\n\
         /{name} Base dup length dict begin\n\
           {{ 1 index /FID ne 2 index /CharStrings ne and {{ def }} {{ pop pop }} ifelse }} forall\n\
           /CharStrings 10 dict dup begin\n\
             /.notdef Base /CharStrings get /.notdef get def\n\
             [ {glyphs} ] {{ dup Base /CharStrings get exch get def }} forall\n\
           end def\n\
           /FontName /{name} def\n\
           currentdict end definefont pop\n"
    )
}

#[test]
fn no_copies_unless_the_device_asks() {
    let (mut c, pages) = ctx(false);
    run(
        &mut c,
        "/Times-Roman findfont 12 scalefont setfont 72 72 moveto (A) show showpage\n",
    );
    let texts = texts(&pages);
    assert_eq!(texts.len(), 1);
    assert_eq!(texts[0].font_snapshot, None);
    assert!(c.font_snapshots.is_empty());
}

/// The EPS idiom: the figure's font is reclaimed before the page is sent.
#[test]
fn a_font_shown_inside_a_save_is_copied_by_its_restore() {
    let (mut c, pages) = ctx(true);
    let entities_before = c.dicts.local.entities.len();
    run(
        &mut c,
        &format!(
            "save\n{}/Fig findfont 24 scalefont setfont 72 600 moveto (AB) show\nrestore\nshowpage\n",
            subset_font("Fig", "/A /B")
        ),
    );
    assert_eq!(
        c.dicts.local.entities.len(),
        entities_before,
        "the restore should have reclaimed the font"
    );
    let texts = texts(&pages);
    assert_eq!(texts.len(), 1);
    let id = texts[0].font_snapshot.expect("text shown with a kept font");
    let font = c.font_snapshots.frozen(id).expect("frozen by the restore");
    assert!(has_glyph(&c, font, "A") && has_glyph(&c, font, "B"));
    assert_eq!(
        get(&c, font, "FontName").and_then(Frozen::as_name),
        c.names.find(b"Fig")
    );
}

/// PLRM 3e §5.9.2: a glyph added inside the page's save is shown, then
/// removed by the restore. The copy must have it.
#[test]
fn a_glyph_added_inside_a_save_is_kept() {
    let (mut c, pages) = ctx(true);
    run(&mut c, &subset_font("Inc", "/A"));
    run(
        &mut c,
        "/IncS /Inc findfont 48 scalefont def\n\
         save\n\
           /Inc findfont /CharStrings get /B Base /CharStrings get /B get put\n\
           IncS setfont 72 600 moveto (AB) show showpage\n\
         restore\n",
    );
    let id = texts(&pages)[0].font_snapshot.unwrap();
    let font = c.font_snapshots.frozen(id).unwrap();
    assert!(has_glyph(&c, font, "B"), "the copy lost the added glyph");

    // The VM itself no longer has it: the copy is what PDF output must read.
    run(&mut c, "/Inc findfont /CharStrings get /B known\n");
    assert_eq!(c.o_stack.pop().unwrap().value, PsValue::Bool(false));
}

/// A font defined once and shown on pages bracketed by save/restore gets an
/// instance per page, all one font: one copy, shared.
#[test]
fn a_font_unchanged_across_restores_is_one_copy() {
    let (mut c, pages) = ctx(true);
    run(
        &mut c,
        "/F /Times-Roman findfont 12 scalefont def\n\
         3 { save F setfont 72 72 moveto (A) show showpage restore } repeat\n",
    );
    let ids: Vec<u32> = texts(&pages)
        .iter()
        .map(|t| t.font_snapshot.unwrap())
        .collect();
    assert_eq!(ids.len(), 3);
    assert!(
        ids[0] != ids[1] && ids[1] != ids[2],
        "one instance per page"
    );
    let resolved = c.font_snapshots.resolve(&c);
    let first = resolved.font(ids[0]).unwrap();
    for &id in &ids[1..] {
        assert!(first.same_font(resolved.font(id).unwrap()));
    }
}

/// Two pages each define a font inside their save; the second reuses the
/// first's reclaimed id. They are different fonts.
#[test]
fn a_reused_id_is_another_font() {
    let (mut c, pages) = ctx(true);
    run(
        &mut c,
        &format!(
            "save\n{}/One findfont 12 scalefont setfont 72 72 moveto (A) show showpage restore\n\
             save\n{}/Two findfont 12 scalefont setfont 72 72 moveto (B) show showpage restore\n",
            subset_font("One", "/A"),
            subset_font("Two", "/B")
        ),
    );
    let texts = texts(&pages);
    assert_eq!(
        texts[0].font_entity, texts[1].font_entity,
        "the second font should reuse the first one's id for this test to mean anything"
    );
    let resolved = c.font_snapshots.resolve(&c);
    let one = resolved.font(texts[0].font_snapshot.unwrap()).unwrap();
    let two = resolved.font(texts[1].font_snapshot.unwrap()).unwrap();
    assert!(!one.same_font(two));
    assert!(has_glyph(&c, &one.root, "A") && !has_glyph(&c, &one.root, "B"));
    assert!(has_glyph(&c, &two.root, "B") && !has_glyph(&c, &two.root, "A"));
}

/// Fonts still live at the end of the job are copied when the device reads
/// them.
#[test]
fn live_fonts_are_copied_when_resolved() {
    let (mut c, pages) = ctx(true);
    run(
        &mut c,
        "/Times-Roman findfont 12 scalefont setfont 72 72 moveto (A) show showpage\n",
    );
    let id = texts(&pages)[0].font_snapshot.unwrap();
    assert!(c.font_snapshots.frozen(id).is_none(), "no restore yet");
    let resolved = c.font_snapshots.resolve(&c);
    assert!(has_glyph(&c, &resolved.font(id).unwrap().root, "A"));
}

/// A Type 3 font's glyph procedures can reach anything; only its encoding
/// and geometry are copied. (A Type 3 font's own shows record no `Text` —
/// PDF output draws its glyphs — but one can be a composite font's
/// descendant, so the font is registered directly here.)
#[test]
fn a_type3_font_keeps_only_its_encoding_and_geometry() {
    let (mut c, _pages) = ctx(true);
    run(
        &mut c,
        "/T3 8 dict begin\n\
           /FontType 3 def /FontMatrix [0.001 0 0 0.001 0 0] def\n\
           /FontBBox [0 0 1000 1000] def\n\
           /Encoding 256 array dup 0 1 255 { /.notdef put dup } for pop dup 65 /a put def\n\
           /CharProcs 2 dict dup /a { 0 0 1000 1000 rectfill } put def\n\
           /CharStrings 2 dict dup /a (bitmap) put def\n\
           /BuildChar { pop pop 1000 0 0 0 1000 1000 setcachedevice } def\n\
         currentdict end definefont\n",
    );
    let PsValue::Dict(font) = c.o_stack.pop().unwrap().value else {
        panic!("definefont returns the font");
    };
    let id = c.font_snapshots.note_shown(font);
    let resolved = c.font_snapshots.resolve(&c);
    let root = &resolved.font(id).unwrap().root;
    let encoding = get(&c, root, "Encoding")
        .and_then(Frozen::as_array)
        .unwrap();
    assert_eq!(encoding[65].as_name(), c.names.find(b"a"));
    // Not even under a key other fonts' copies keep.
    assert!(matches!(
        get(&c, root, "CharStrings"),
        Some(Frozen::NotKept)
    ));
    assert!(matches!(get(&c, root, "CharProcs"), Some(Frozen::NotKept)));
    assert!(matches!(get(&c, root, "BuildChar"), Some(Frozen::NotKept)));
    assert_eq!(get(&c, root, "FontType").and_then(Frozen::as_i32), Some(3));
}

/// An array that contains itself is copied without looping.
#[test]
fn a_cycle_is_cut() {
    let (mut c, pages) = ctx(true);
    run(
        &mut c,
        "/Base /Times-Roman findfont def\n\
         /Cyc Base dup length dict begin\n\
           { 1 index /FID ne { def } { pop pop } ifelse } forall\n\
           /Encoding Base /Encoding get 256 array copy def\n\
           Encoding 0 Encoding put\n\
           /FontName /Cyc def\n\
         currentdict end definefont pop\n\
         /Cyc findfont 12 scalefont setfont 72 72 moveto (A) show showpage\n",
    );
    let id = texts(&pages)[0].font_snapshot.unwrap();
    let resolved = c.font_snapshots.resolve(&c);
    let encoding = get(&c, &resolved.font(id).unwrap().root, "Encoding")
        .and_then(Frozen::as_array)
        .unwrap();
    assert!(matches!(encoding[0], Frozen::NotKept));
    assert_eq!(encoding[65].as_name(), c.names.find(b"A"));
}

/// A Type 3 glyph that shows another font is cached with its text; a cache
/// hit on a later page replays that text, whose font the page's `restore`
/// has reclaimed. It keeps the instance it was shown with.
#[test]
fn a_cached_type3_glyph_keeps_its_instance() {
    let (mut c, pages) = ctx(true);
    run(
        &mut c,
        "/Base /Times-Roman findfont def\n\
         /Wrap 8 dict begin\n\
           /FontType 3 def /FontMatrix [0.001 0 0 0.001 0 0] def\n\
           /FontBBox [0 0 1000 1000] def\n\
           /Encoding 256 array dup 0 1 255 { /.notdef put dup } for pop dup 65 /A put def\n\
           /BuildChar { pop pop 1000 0 0 0 1000 1000 setcachedevice\n\
             /Inner findfont 1000 scalefont setfont 0 0 moveto (A) show } def\n\
         currentdict end definefont pop\n\
         /WrapS /Wrap findfont 12 scalefont def\n",
    );
    run(
        &mut c,
        &format!(
            "save\n{}WrapS setfont 72 72 moveto (A) show showpage restore\n\
             save WrapS setfont 72 72 moveto (A) show showpage restore\n",
            subset_font("Inner", "/A")
        ),
    );
    let texts = texts(&pages);
    assert_eq!(texts.len(), 2);
    let id = texts[0].font_snapshot.unwrap();
    assert_eq!(
        texts[1].font_snapshot,
        Some(id),
        "the second page should replay the cached glyph, not run BuildChar"
    );
    let font = c.font_snapshots.frozen(id).unwrap();
    assert!(has_glyph(&c, font, "A"));
    assert_eq!(
        get(&c, font, "FontName").and_then(Frozen::as_name),
        c.names.find(b"Inner")
    );
}

/// CIDFont metrics from `Metrics2`/`CDevProc` travel with the copy, and
/// `restore` drops the interpreter's entries for fonts it reclaims.
#[test]
fn glyph_metrics_are_copied_then_pruned() {
    let (mut c, pages) = ctx(true);
    let metrics = CidGlyphMetrics {
        w0: [0.5, 0.0],
        vertical: Some(([0.0, -1.0], [0.25, 0.75])),
    };
    run(
        &mut c,
        &format!(
            "save\n{}/Fig findfont 12 scalefont setfont 72 72 moveto (A) show showpage\n",
            subset_font("Fig", "/A")
        ),
    );
    let font = EntityId(texts(&pages)[0].font_entity);
    c.cid_glyph_metrics.insert((font, 7), metrics);
    run(&mut c, "restore\n");
    assert!(
        !c.cid_glyph_metrics.contains_key(&(font, 7)),
        "the restore reclaimed the font, and should drop its metrics"
    );
    let id = texts(&pages)[0].font_snapshot.unwrap();
    let resolved = c.font_snapshots.resolve(&c);
    let root: &Arc<FrozenDict> = &resolved.font(id).unwrap().root;
    assert_eq!(resolved.cid_metrics(root, 7), Some(metrics));
    assert_eq!(resolved.cid_metrics(root, 8), None);

    // A font still live at the end of the job: copied, metrics and all,
    // when the device reads it.
    run(
        &mut c,
        "/Times-Roman findfont 12 scalefont setfont 72 72 moveto (A) show showpage\n",
    );
    let text = texts(&pages)[1].clone();
    c.cid_glyph_metrics
        .insert((EntityId(text.font_entity), 9), metrics);
    let resolved = c.font_snapshots.resolve(&c);
    let root = &resolved.font(text.font_snapshot.unwrap()).unwrap().root;
    assert_eq!(resolved.cid_metrics(root, 9), Some(metrics));
}

/// PLRM 3e §5.9.2 lets a page fill a `.notdef` slot of a font's encoding,
/// which its `restore` then reverts. The copy has the entry.
#[test]
fn an_encoding_entry_added_inside_a_save_is_kept() {
    let (mut c, pages) = ctx(true);
    run(
        &mut c,
        "/Base /Times-Roman findfont def\n\
         /Enc Base dup length dict begin\n\
           { 1 index /FID ne { def } { pop pop } ifelse } forall\n\
           /Encoding Base /Encoding get 256 array copy dup 66 /.notdef put def\n\
           /FontName /Enc def\n\
         currentdict end definefont pop\n\
         /EncS /Enc findfont 12 scalefont def\n\
         save EncS setfont 72 72 moveto (A) show showpage restore\n\
         save /Enc findfont /Encoding get 66 /B put\n\
           EncS setfont 72 72 moveto (B) show showpage restore\n",
    );
    let texts = texts(&pages);
    let resolved = c.font_snapshots.resolve(&c);
    let encoding = |i: usize| {
        let root = &resolved.font(texts[i].font_snapshot.unwrap()).unwrap().root;
        get(&c, root, "Encoding")
            .and_then(Frozen::as_array)
            .unwrap()[66]
            .as_name()
    };
    assert_eq!(encoding(0), c.names.find(b".notdef"));
    assert_eq!(
        encoding(1),
        c.names.find(b"B"),
        "the copy lost the added entry"
    );
}

/// Keys that are composite identities are not copied; names are.
#[test]
fn a_copy_has_the_font_dictionary_entries() {
    let (mut c, pages) = ctx(true);
    run(
        &mut c,
        "/Times-Roman findfont 12 scalefont setfont 72 72 moveto (A) show showpage\n",
    );
    let id = texts(&pages)[0].font_snapshot.unwrap();
    let resolved = c.font_snapshots.resolve(&c);
    let root = &resolved.font(id).unwrap().root;
    let private = get(&c, root, "Private").and_then(Frozen::as_dict).unwrap();
    assert!(
        get(&c, private, "Subrs")
            .and_then(Frozen::as_array)
            .is_some()
    );
    assert!(
        get(&c, root, "FontMatrix")
            .and_then(Frozen::as_array)
            .is_some()
    );
    assert!(
        root.iter()
            .all(|(k, _)| !matches!(k, DictKey::Identity(..)))
    );
}

/// A font that changes between restores — a glyph added outside any save —
/// is a new copy, sharing every part that did not change.
#[test]
fn a_changed_font_shares_its_unchanged_parts() {
    let (mut c, pages) = ctx(true);
    run(&mut c, &subset_font("Grow", "/A"));
    run(
        &mut c,
        "/GrowS /Grow findfont 12 scalefont def\n\
         save GrowS setfont 72 72 moveto (A) show showpage restore\n\
         /Grow findfont /CharStrings get /B Base /CharStrings get /B get put\n\
         save GrowS setfont 72 72 moveto (AB) show showpage restore\n",
    );
    let texts = texts(&pages);
    let resolved = c.font_snapshots.resolve(&c);
    let before = &resolved.font(texts[0].font_snapshot.unwrap()).unwrap().root;
    let after = &resolved.font(texts[1].font_snapshot.unwrap()).unwrap().root;
    assert!(!Arc::ptr_eq(before, after), "the font changed");
    assert!(!has_glyph(&c, before, "B") && has_glyph(&c, after, "B"));
    for key in ["Private", "Encoding"] {
        match (get(&c, before, key), get(&c, after, key)) {
            (Some(Frozen::Dict(x)), Some(Frozen::Dict(y))) => assert!(Arc::ptr_eq(x, y), "{key}"),
            (Some(Frozen::Array(x)), Some(Frozen::Array(y))) => assert!(Arc::ptr_eq(x, y), "{key}"),
            other => panic!("{key}: {other:?}"),
        }
    }
}
