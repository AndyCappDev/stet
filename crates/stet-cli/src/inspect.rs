// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `stet inspect <file.pdf>` — print a human-readable summary of a PDF's
//! structural content (metadata, outline, annotations, forms, embedded
//! files, parse warnings).
//!
//! Exercises the `stet-pdf-reader` structural API; complements `--device
//! png/pdf/viewer` which exercise the rendering API.

use std::collections::BTreeMap;

use stet_graphics::icc::IccCache;
use stet_pdf_reader::{
    AnnotationKind, Destination, FieldKind, OutlineItem, ParsePhase, PdfDocument, PdfError,
    Severity, ViewSpec,
};

/// Run the inspect subcommand. Returns a process exit code.
pub fn run_inspect(path: &str, password: Option<&[u8]>) -> i32 {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: cannot read '{path}': {e}");
            return 1;
        }
    };

    let doc = match open_document(&data, password) {
        Ok(d) => d,
        Err(PdfError::PasswordRequired) => {
            eprintln!(
                "Error: '{path}' is encrypted with a non-empty password — use --password <pw>"
            );
            return 1;
        }
        Err(e) => {
            eprintln!("Error: failed to parse '{path}': {e}");
            return 1;
        }
    };

    println!("{}", path);
    println!();
    print_metadata(&doc);
    print_pages(&doc);
    print_outline(&doc);
    print_destinations(&doc);
    print_annotations(&doc);
    print_form(&doc);
    print_embedded_files(&doc);
    print_warnings(&doc);
    0
}

fn open_document<'a>(data: &'a [u8], password: Option<&[u8]>) -> Result<PdfDocument<'a>, PdfError> {
    let icc_cache = IccCache::new();
    match password {
        Some(pw) => PdfDocument::from_bytes_with_password(data, icc_cache, pw),
        None => PdfDocument::from_bytes_with_icc(data, icc_cache),
    }
}

fn print_metadata(doc: &PdfDocument) {
    let m = doc.metadata();
    let mut printed_any = false;
    let mut row = |label: &str, value: &str| {
        if !value.is_empty() {
            if !printed_any {
                println!("Metadata:");
                printed_any = true;
            }
            println!("  {label}: {value}");
        }
    };
    if let Some(t) = &m.title {
        row("Title", t);
    }
    if let Some(a) = &m.author {
        row("Author", a);
    }
    if let Some(s) = &m.subject {
        row("Subject", s);
    }
    if let Some(k) = &m.keywords {
        row("Keywords", k);
    }
    if let Some(c) = &m.creator {
        row("Creator", c);
    }
    if let Some(p) = &m.producer {
        row("Producer", p);
    }
    if let Some(d) = &m.creation_date {
        row("Created", &format_date(d));
    }
    if let Some(d) = &m.mod_date {
        row("Modified", &format_date(d));
    }
    if let Some(t) = &m.trapped {
        row("Trapped", &format!("{:?}", t));
    }
    if m.xmp_xml.is_some() {
        row("XMP", "(present)");
    }
    if printed_any {
        println!();
    }
}

fn format_date(d: &stet_pdf_reader::PdfDate) -> String {
    let mut s = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        d.year, d.month, d.day, d.hour, d.minute, d.second
    );
    if let Some(off) = d.tz_offset_minutes {
        let sign = if off >= 0 { '+' } else { '-' };
        let abs = off.unsigned_abs();
        s.push_str(&format!(" {sign}{:02}:{:02}", abs / 60, abs % 60));
    } else {
        s.push_str(" UTC");
    }
    s
}

fn print_pages(doc: &PdfDocument) {
    let count = doc.page_count();
    println!("Pages: {count}");
    if count > 0
        && let Ok(boxes) = doc.page_boxes(0)
    {
        let [llx, lly, urx, ury] = boxes.media_box;
        let w = urx - llx;
        let h = ury - lly;
        println!(
            "  Page 1 size: {:.1} × {:.1} pt ({:.2} × {:.2} in)",
            w,
            h,
            w / 72.0,
            h / 72.0
        );
        if boxes.rotate != 0 {
            println!("  Page 1 rotation: {}°", boxes.rotate);
        }
        if boxes.user_unit != 1.0 {
            println!("  Page 1 user unit: {}", boxes.user_unit);
        }
    }
    println!();
}

fn print_outline(doc: &PdfDocument) {
    let outline = doc.outline();
    if outline.is_empty() {
        return;
    }
    let total = count_items(outline);
    println!("Outline ({total} entries):");
    for item in outline {
        print_outline_node(item, 1);
    }
    println!();
}

fn count_items(items: &[OutlineItem]) -> usize {
    items.iter().map(|i| 1 + count_items(&i.children)).sum()
}

fn print_outline_node(item: &OutlineItem, depth: usize) {
    let indent = "  ".repeat(depth);
    let target = match &item.destination {
        Some(Destination::PageView {
            page: Some(p),
            view,
        }) => {
            format!(" → page {} {}", p + 1, format_view_brief(view))
        }
        Some(Destination::PageView { page: None, .. }) => " → (broken page ref)".to_string(),
        Some(Destination::NamedDest(name)) => format!(" → /{name}"),
        None => String::new(),
    };
    let title = if item.title.is_empty() {
        "(untitled)"
    } else {
        item.title.as_str()
    };
    println!("{indent}- {title}{target}");
    for child in &item.children {
        print_outline_node(child, depth + 1);
    }
}

fn format_view_brief(view: &ViewSpec) -> &'static str {
    match view {
        ViewSpec::Xyz { .. } => "(xyz)",
        ViewSpec::Fit => "(fit)",
        ViewSpec::FitH { .. } => "(fith)",
        ViewSpec::FitV { .. } => "(fitv)",
        ViewSpec::FitR { .. } => "(fitr)",
        ViewSpec::FitB => "(fitb)",
        ViewSpec::FitBH { .. } => "(fitbh)",
        ViewSpec::FitBV { .. } => "(fitbv)",
    }
}

fn print_destinations(doc: &PdfDocument) {
    let dests = doc.destinations();
    if dests.is_empty() {
        return;
    }
    println!("Named destinations: {}", dests.len());
    println!();
}

fn print_annotations(doc: &PdfDocument) {
    let mut total = 0usize;
    let mut per_page: Vec<(usize, BTreeMap<String, u32>)> = Vec::new();
    for page in 0..doc.page_count() {
        let Ok(annots) = doc.page_annotations(page) else {
            continue;
        };
        if annots.is_empty() {
            continue;
        }
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        for annot in annots {
            *counts.entry(format_kind(&annot.kind)).or_insert(0) += 1;
        }
        total += annots.len();
        per_page.push((page, counts));
    }
    if per_page.is_empty() {
        return;
    }
    println!("Annotations: {total}");
    for (page, counts) in &per_page {
        let parts: Vec<String> = counts.iter().map(|(k, v)| format!("{v} {k}")).collect();
        println!("  Page {}: {}", page + 1, parts.join(", "));
    }
    println!();
}

fn format_kind(kind: &AnnotationKind) -> String {
    match kind {
        AnnotationKind::Other(s) => format!("Other({s})"),
        other => format!("{:?}", other),
    }
}

fn print_form(doc: &PdfDocument) {
    let Some(form) = doc.form() else {
        return;
    };
    let mut counts: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut total_terminals = 0u32;
    let mut total_widgets = 0u32;
    walk_form_for_summary(
        &form.fields,
        &mut counts,
        &mut total_terminals,
        &mut total_widgets,
    );
    println!(
        "Form: {} terminal field{} ({} widget{})",
        total_terminals,
        if total_terminals == 1 { "" } else { "s" },
        total_widgets,
        if total_widgets == 1 { "" } else { "s" }
    );
    if !counts.is_empty() {
        let parts: Vec<String> = counts.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        println!("  By kind: {}", parts.join(", "));
    }
    if form.sig_flags.signatures_exist {
        println!(
            "  Signatures: present{}",
            if form.sig_flags.append_only {
                " (append-only)"
            } else {
                ""
            }
        );
    }
    if form.has_xfa {
        println!("  XFA: present");
    }
    if form.need_appearances {
        println!("  NeedAppearances: true");
    }
    println!();
}

fn walk_form_for_summary(
    fields: &[stet_pdf_reader::FormField],
    counts: &mut BTreeMap<&'static str, u32>,
    total_terminals: &mut u32,
    total_widgets: &mut u32,
) {
    for f in fields {
        let label = match &f.kind {
            FieldKind::Button(_) => Some("Button"),
            FieldKind::Text(_) => Some("Text"),
            FieldKind::Choice(_) => Some("Choice"),
            FieldKind::Signature(_) => Some("Signature"),
            FieldKind::Other { .. } => Some("Other"),
            FieldKind::Container => None,
        };
        if let Some(name) = label {
            *counts.entry(name).or_insert(0) += 1;
            *total_terminals += 1;
            *total_widgets += f.widget_obj_nums.len() as u32;
        }
        walk_form_for_summary(&f.children, counts, total_terminals, total_widgets);
    }
}

fn print_embedded_files(doc: &PdfDocument) {
    let files = doc.embedded_files();
    if files.is_empty() {
        return;
    }
    println!("Embedded files: {}", files.len());
    let mut entries: Vec<_> = files.iter().collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));
    for (name, ef) in entries {
        let size = ef
            .size
            .map(format_byte_size)
            .unwrap_or_else(|| "size unknown".to_string());
        let mime = ef
            .mime_type
            .as_deref()
            .map(|m| format!(", {m}"))
            .unwrap_or_default();
        println!("  - {name} ({size}{mime})");
    }
    println!();
}

fn format_byte_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn print_warnings(doc: &PdfDocument) {
    let warnings = doc.parse_warnings();
    if warnings.is_empty() {
        return;
    }
    println!("Warnings: {}", warnings.len());
    let mut by_phase: BTreeMap<String, u32> = BTreeMap::new();
    for w in warnings.iter() {
        *by_phase.entry(format_phase(&w.phase)).or_insert(0) += 1;
    }
    for (phase, count) in &by_phase {
        println!("  {phase}: {count}");
    }
    // Show the first few in detail.
    let preview = warnings.iter().take(5);
    for w in preview {
        let sev = match w.severity {
            Severity::Info => "info",
            Severity::Warning => "warn",
            Severity::Error => "error",
        };
        println!("  [{sev}] {}: {}", format_phase(&w.phase), w.message);
    }
    if warnings.len() > 5 {
        println!("  ... and {} more", warnings.len() - 5);
    }
}

fn format_phase(phase: &ParsePhase) -> String {
    match phase {
        ParsePhase::Metadata => "metadata".to_string(),
        ParsePhase::ViewerPreferences => "viewer-prefs".to_string(),
        ParsePhase::Outline => "outline".to_string(),
        ParsePhase::Destinations => "destinations".to_string(),
        ParsePhase::Annotations { page } => format!("annotations(page {})", page + 1),
        ParsePhase::Form => "form".to_string(),
        ParsePhase::PageBoxes { page } => format!("page-boxes(page {})", page + 1),
        ParsePhase::EmbeddedFiles => "embedded-files".to_string(),
    }
}
