// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A byte read ahead and put back is read again, from `%stdin` and from a
//! pipe as from a disk file.
//!
//! The scanner reads one byte past a token to find its end, and `readline`
//! one past a CR to see whether an LF follows; a filter may look past its
//! data for its end-of-data marker. Each puts the byte back. That worked
//! for a disk file by seeking back over it, but `%stdin` cannot seek and
//! seeking a pipe fails, so the byte was dropped: `abc/def` read from
//! standard input scanned as `abc` and `def`, losing the `/`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A file in a scratch directory that is removed afterwards.
struct TempPath(PathBuf);

impl TempPath {
    fn new(name: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "stet-pushback-{}-{:?}-{name}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        Self(p)
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `path` as the body of a PostScript string literal. A backslash in a
/// literal starts an escape, so a Windows path's separators must be
/// doubled, or `C:\Users` reaches `file` as `C:Users`.
fn ps_string(path: &std::path::Path) -> String {
    let mut out = String::new();
    for c in path.display().to_string().chars() {
        if matches!(c, '\\' | '(' | ')') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Run `program` with `input` on standard input; what it prints.
fn run(program: &str, input: &[u8]) -> String {
    let file = TempPath::new("job.ps");
    std::fs::write(&file.0, program).expect("write program");
    let mut child = Command::new(env!("CARGO_BIN_EXE_stet"))
        .args(["--device", "null"])
        .arg(&file.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run stet");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .expect("write stdin");
    let output = child.wait_with_output().expect("stet finishes");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The lines `program` prints that start with `(` or `/`, or a digit.
fn printed(out: &str) -> Vec<&str> {
    out.lines()
        .map(str::trim)
        .filter(|l| l.starts_with(['(', '/']) || l.starts_with(|c: char| c.is_ascii_digit()))
        .collect()
}

#[test]
fn a_token_read_from_stdin_keeps_the_byte_after_it() {
    let out = run(
        "%!PS\n/f (%stdin) (r) file def f token pop == f token pop ==\n",
        b"abc/def (x)",
    );
    assert_eq!(printed(&out), ["/def"], "{out}");
    assert!(out.contains("abc"), "{out}");
}

#[test]
fn readline_from_stdin_keeps_the_byte_after_a_cr() {
    let out = run(
        "%!PS\n/f (%stdin) (r) file def /s 20 string def \
         f s readline pop == f s readline pop ==\n",
        b"ab\rcd\n",
    );
    assert_eq!(printed(&out), ["(ab)", "(cd)"], "{out}");
}

/// The look-ahead a filter would make for its end-of-data marker: data
/// read from `%stdin` past a full buffer comes through whole.
#[test]
fn ascii85_data_from_stdin_comes_through_whole() {
    // 4100 bytes, ASCII85 with no whitespace, so the bytes after the
    // first 4096 begin with a data character.
    let data: Vec<u8> = (0..4100u32).map(|i| (i * 7 + 3) as u8).collect();
    let mut encoded = Vec::new();
    for chunk in data.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        let mut v = u32::from_be_bytes(word);
        let mut digits = [0u8; 5];
        for d in digits.iter_mut().rev() {
            *d = (v % 85) as u8 + b'!';
            v /= 85;
        }
        encoded.extend_from_slice(&digits[..chunk.len() + 1]);
    }
    encoded.extend_from_slice(b"~>");
    let out = run(
        "%!PS\n(%stdin) (r) file /ASCII85Decode filter 5000 string readstring pop \
         dup length = 4096 4 getinterval { = } forall\n",
        &encoded,
    );
    let want: Vec<String> = std::iter::once("4100".to_string())
        .chain(data[4096..].iter().map(|b| b.to_string()))
        .collect();
    assert_eq!(printed(&out), want, "{out}");
}

#[cfg(unix)]
#[test]
fn a_token_read_from_a_pipe_keeps_the_byte_after_it() {
    let fifo = TempPath::new("fifo");
    let made = Command::new("mkfifo")
        .arg(&fifo.0)
        .status()
        .expect("mkfifo runs");
    assert!(made.success());
    let path = fifo.0.clone();
    let writer = std::thread::spawn(move || {
        std::fs::write(path, b"abc/def (x)").expect("write the pipe");
    });
    let out = run(
        &format!(
            "%!PS\n/f ({}) (r) file def f token pop == f token pop ==\n",
            ps_string(&fifo.0)
        ),
        b"",
    );
    writer.join().unwrap();
    assert_eq!(printed(&out), ["/def"], "{out}");
}

/// A disk file still reports its position as the bytes not yet read.
#[test]
fn a_disk_file_reports_its_position_after_a_byte_is_put_back() {
    let data = TempPath::new("data.txt");
    std::fs::write(&data.0, b"abc/def (x)").expect("write data");
    let out = run(
        &format!(
            "%!PS\n/f ({}) (r) file def f token pop pop f fileposition = \
             f bytesavailable = f token pop == f fileposition =\n",
            ps_string(&data.0)
        ),
        b"",
    );
    assert_eq!(printed(&out), ["3", "8", "/def", "8"], "{out}");
}
