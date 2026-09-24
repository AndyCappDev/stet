// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end behaviour of `stet text`: what reaches stdout, and the exit
//! status, for PostScript and PDF input.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A file in a scratch directory that is removed afterwards.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, data: &[u8]) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "stet-text-{}-{:?}-{name}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&p, data).expect("write input");
        Self(p)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn stet_text(args: &[&str], input: &TempFile) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stet"))
        .arg("text")
        .args(args)
        .arg(&input.0)
        .output()
        .expect("run stet")
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).expect("UTF-8 output")
}

/// Two pages; the first sets its words apart by distance, as TeX does,
/// and prints to stdout, which must not reach the text.
const TWO_PAGES: &str = "%!PS
/Helvetica findfont 12 scalefont setfont
72 700 moveto (Paper) show 4 0 rmoveto (Title) show
72 680 moveto (second line) show
(noise) print
showpage
72 700 moveto (Page two) show
showpage
";

#[test]
fn plain_text_of_postscript() {
    let input = TempFile::new("two.ps", TWO_PAGES.as_bytes());
    assert_eq!(
        stdout(&stet_text(&[], &input)),
        "Paper Title\nsecond line\n\x0cPage two\n\x0c"
    );
    assert_eq!(
        stdout(&stet_text(&["--pages", "2"], &input)),
        "Page two\n\x0c"
    );
}

#[test]
fn json_with_positions() {
    let input = TempFile::new("two.ps", TWO_PAGES.as_bytes());
    let json = stdout(&stet_text(&["--json", "--pages", "1"], &input));
    assert!(json.starts_with(
        "{\"pages\": [\n  {\"page\": 1, \"width\": 612, \"height\": 792, \"lines\": ["
    ));
    assert!(json.contains("{\"text\": \"Paper Title\", \"bbox\": [72, "));
    assert!(json.contains("\"words\": [{\"text\": \"Paper\"}, {\"text\": \"Title\"}]"));
    assert!(json.trim_end().ends_with("]}"));
    // Word boxes only when asked for.
    let json = stdout(&stet_text(
        &["--json", "--word-boxes", "--pages", "1"],
        &input,
    ));
    assert!(json.contains("{\"text\": \"Paper\", \"bbox\": [72, "));
}

#[test]
fn text_of_a_pdf() {
    let content = "BT /F1 12 Tf 72 700 Td [(Paper) -333 (Title)] TJ ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objects.len() + 1).as_bytes());
    for off in offsets {
        pdf.extend(format!("{off:010} 00000 n\r\n").as_bytes());
    }
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    // Recognised by its header, whatever the name.
    let input = TempFile::new("doc.bin", &pdf);
    assert_eq!(stdout(&stet_text(&[], &input)), "Paper Title\n\x0c");
}

#[test]
fn output_goes_to_a_file() {
    let input = TempFile::new("two.ps", TWO_PAGES.as_bytes());
    let output = TempFile(input.0.with_extension("txt"));
    let path = output.0.to_str().expect("UTF-8 temp path");
    // Written whole, with nothing on stdout.
    assert_eq!(stdout(&stet_text(&["-o", path], &input)), "");
    assert_eq!(
        std::fs::read_to_string(&output.0).expect("output written"),
        "Paper Title\nsecond line\n\x0cPage two\n\x0c"
    );
    std::fs::remove_file(&output.0).expect("remove output");

    // Input that fails leaves no empty file behind.
    let broken = TempFile::new("broken.pdf", b"%PDF-1.7\nnot a pdf");
    let result = stet_text(&["--output", path], &broken);
    assert!(!result.status.success());
    assert!(!output.0.exists());

    // Nor does a file that cannot be created succeed.
    let result = stet_text(&["-o", "/nonexistent-dir/out.txt"], &input);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("/nonexistent-dir/out.txt"));
}

#[test]
fn bad_arguments_fail() {
    let input = TempFile::new("two.ps", TWO_PAGES.as_bytes());
    for args in [
        &["--word-boxes"][..],
        &["--pages", "0"],
        &["--bogus"],
        &["-o"],
    ] {
        let output = stet_text(args, &input);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}
