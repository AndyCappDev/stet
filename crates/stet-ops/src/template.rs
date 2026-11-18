// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PostScript name templates, for `filenameforall` and `resourceforall`.
//!
//! The PLRM (`filenameforall`) gives the convention: every character is
//! literal and case-sensitive except `*`, which matches zero or more
//! characters, `?`, which matches exactly one, and `\`, which makes the next
//! character literal even if it is `*`, `?` or `\`. A character is a byte, as
//! in every PostScript string.
//!
//! File templates are matched one path component at a time, so `*` never
//! matches a separator, and the names they produce are spelled as the
//! template spells them: the separators and literal components are the
//! template's own text, and only a component with a wildcard is replaced by
//! the name it matched. `(./d//*.ps)` gives `./d//a.ps`, as in Ghostscript.
//! On Windows `\` separates components as `/` does, as there and in
//! Ghostscript, and so is not an escape in a file template; a Windows file
//! name cannot hold `*` or `?`, which leaves the escape nothing to protect.

use std::path::PathBuf;

/// One element of a parsed template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    /// This byte and no other.
    Byte(u8),
    /// Any one byte (`?`).
    One,
    /// Any run of bytes, the empty one included (`*`).
    Run,
}

/// Parse `template`, resolving the `\` escape. A `\` at the very end has
/// nothing to quote and stands for itself.
fn tokens(template: &[u8]) -> Vec<Token> {
    let mut out = Vec::with_capacity(template.len());
    let mut bytes = template.iter().copied();
    while let Some(b) = bytes.next() {
        out.push(match b {
            b'\\' => Token::Byte(bytes.next().unwrap_or(b'\\')),
            b'?' => Token::One,
            b'*' => Token::Run,
            _ => Token::Byte(b),
        });
    }
    out
}

/// Whether `name` matches all of `template`.
pub(crate) fn matches(template: &[u8], name: &[u8]) -> bool {
    matches_tokens(&tokens(template), name)
}

/// Whether `name` matches all of `pattern`: the usual single-backtrack
/// wildcard match, linear in practice and never worse than their product.
fn matches_tokens(pattern: &[Token], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    // The last `*` seen and the name position it is now taken to end at.
    let mut retry: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some(Token::Run) => {
                retry = Some((p, n));
                p += 1;
            }
            Some(Token::One) => {
                p += 1;
                n += 1;
            }
            Some(Token::Byte(b)) if *b == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match retry {
                // Let the last `*` take one more byte and try again from there.
                Some((star, taken)) => {
                    retry = Some((star, taken + 1));
                    p = star + 1;
                    n = taken + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|t| *t == Token::Run)
}

/// Whether `b` separates the components of a file template.
fn is_separator(b: u8) -> bool {
    b == b'/' || (cfg!(windows) && b == b'\\')
}

/// A file template cut at its separators.
#[derive(Debug)]
enum Piece {
    /// A run of separators, kept as written.
    Separators(Vec<u8>),
    /// A component with no wildcard: the name it stands for, unescaped.
    Literal(Vec<u8>),
    /// A component with a wildcard, to match against a folder's entries.
    Wildcard(Vec<Token>),
}

/// Cut `template` into separators and components. A separator quoted with
/// `\` belongs to its component, where it matches nothing on disk.
fn pieces(template: &[u8]) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut component = Vec::new();
    let mut separators = Vec::new();
    let flush = |component: &mut Vec<u8>, out: &mut Vec<Piece>| {
        if component.is_empty() {
            return;
        }
        let parsed = tokens(component);
        out.push(if parsed.iter().all(|t| matches!(t, Token::Byte(_))) {
            Piece::Literal(
                parsed
                    .iter()
                    .map(|t| match t {
                        Token::Byte(b) => *b,
                        _ => unreachable!("checked just above"),
                    })
                    .collect(),
            )
        } else {
            Piece::Wildcard(parsed)
        });
        component.clear();
    };
    let mut bytes = template.iter().copied();
    while let Some(b) = bytes.next() {
        if is_separator(b) {
            flush(&mut component, &mut out);
            separators.push(b);
            continue;
        }
        if !separators.is_empty() {
            out.push(Piece::Separators(std::mem::take(&mut separators)));
        }
        component.push(b);
        if b == b'\\'
            && let Some(quoted) = bytes.next()
        {
            component.push(quoted);
        }
    }
    flush(&mut component, &mut out);
    if !separators.is_empty() {
        out.push(Piece::Separators(separators));
    }
    out
}

/// The names of the files and folders `template` matches, each spelled as
/// the template spells it, in byte order. Nothing is filtered for access:
/// the caller decides which of them a program may see.
pub(crate) fn matching_files(template: &[u8]) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    walk(&pieces(template), Vec::new(), &mut found);
    found.sort();
    found.dedup();
    found
}

/// Extend `prefix`, the name built so far, by the rest of the template.
fn walk(rest: &[Piece], mut prefix: Vec<u8>, found: &mut Vec<Vec<u8>>) {
    let Some((piece, rest)) = rest.split_first() else {
        // What is left names one file, if it is there; a trailing separator
        // asks for a folder.
        let path = to_path(&prefix);
        let there = match prefix.last() {
            None => false,
            Some(&b) if is_separator(b) => path.is_dir(),
            Some(_) => std::fs::symlink_metadata(&path).is_ok(),
        };
        if there {
            found.push(prefix);
        }
        return;
    };
    match piece {
        Piece::Separators(s) | Piece::Literal(s) => {
            prefix.extend_from_slice(s);
            walk(rest, prefix, found);
        }
        Piece::Wildcard(pattern) => {
            let folder = if prefix.is_empty() {
                PathBuf::from(".")
            } else {
                to_path(&prefix)
            };
            let Ok(entries) = std::fs::read_dir(folder) else {
                return;
            };
            for entry in entries.flatten() {
                let name = from_os(&entry.file_name());
                if !matches_tokens(pattern, &name) {
                    continue;
                }
                let mut path = prefix.clone();
                path.extend_from_slice(&name);
                if rest.is_empty() {
                    // The folder listed it, so it is there.
                    found.push(path);
                } else {
                    walk(rest, path, found);
                }
            }
        }
    }
}

/// The host path a name stands for. On Unix a path is bytes, so any name
/// reaches its file; elsewhere it is read as UTF-8.
#[cfg(unix)]
fn to_path(name: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(name).into()
}

#[cfg(not(unix))]
fn to_path(name: &[u8]) -> PathBuf {
    String::from_utf8_lossy(name).into_owned().into()
}

/// A folder entry's name as PostScript string bytes.
#[cfg(unix)]
fn from_os(name: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn from_os(name: &std::ffi::OsStr) -> Vec<u8> {
    name.to_string_lossy().into_owned().into_bytes()
}

/// The host path a name from [`matching_files`] stands for, for the
/// caller's access check.
pub(crate) fn host_path(name: &[u8]) -> PathBuf {
    to_path(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_match_runs_and_single_bytes() {
        assert!(matches(b"*", b""));
        assert!(matches(b"*", b"anything"));
        assert!(matches(b"a*c", b"abbbc"));
        assert!(matches(b"a*c", b"ac"));
        assert!(!matches(b"a*c", b"acb"));
        assert!(matches(b"a?c", b"abc"));
        assert!(!matches(b"a?c", b"ac"));
        assert!(matches(b"*.ps", b"x.y.ps"));
        assert!(matches(b"*a*b", b"xaxaxb"));
        assert!(!matches(b"*a*b", b"xaxax"));
        assert!(matches(b"Times-*", b"Times-Roman"));
        assert!(!matches(b"times-*", b"Times-Roman"), "case-sensitive");
    }

    #[test]
    fn a_backslash_makes_the_next_byte_literal() {
        assert!(matches(b"x\\*y", b"x*y"));
        assert!(!matches(b"x\\*y", b"xzy"));
        assert!(matches(b"x\\?y", b"x?y"));
        assert!(!matches(b"x\\?y", b"xzy"));
        assert!(matches(b"x\\\\y", b"x\\y"));
        assert!(matches(b"\\a", b"a"));
        assert!(matches(b"end\\", b"end\\"), "a final backslash is itself");
    }

    #[test]
    fn brackets_are_literal() {
        assert!(matches(b"[ab]", b"[ab]"));
        assert!(!matches(b"[ab]", b"a"));
    }

    /// A fresh folder of files, named for the test.
    fn folder(test: &str, files: &[&str]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("stet-template-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for f in files {
            let path = root.join(f);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }
        root
    }

    /// The template `root` + `rel`, with `/` separators.
    fn template(root: &std::path::Path, rel: &str) -> String {
        format!("{}/{rel}", root.to_string_lossy())
    }

    /// The matches of `root` + `rel`, with `root`'s own spelling cut off.
    fn found(root: &std::path::Path, rel: &str) -> Vec<String> {
        let prefix = root.to_string_lossy().len();
        matching_files(template(root, rel).as_bytes())
            .into_iter()
            .map(|n| String::from_utf8(n[prefix..].to_vec()).unwrap())
            .collect()
    }

    #[test]
    fn names_keep_the_templates_spelling() {
        let root = folder("spelling", &["d/a.ps", "d/b.ps", "d/c.txt"]);
        assert_eq!(found(&root, "d/*.ps"), ["/d/a.ps", "/d/b.ps"]);
        assert_eq!(found(&root, "./d/*.ps"), ["/./d/a.ps", "/./d/b.ps"]);
        assert_eq!(found(&root, "d//?.ps"), ["/d//a.ps", "/d//b.ps"]);
        assert_eq!(found(&root, "d/a.ps"), ["/d/a.ps"]);
        assert!(found(&root, "d/missing.ps").is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn wildcards_stay_within_one_component() {
        let root = folder("components", &["d/sub/c.ps", "e/sub/c.ps", "d/x.ps"]);
        assert!(found(&root, "d*c.ps").is_empty());
        assert_eq!(found(&root, "*/sub/c.ps"), ["/d/sub/c.ps", "/e/sub/c.ps"]);
        assert_eq!(found(&root, "d/*"), ["/d/sub", "/d/x.ps"]);
        assert_eq!(found(&root, "d/*/"), ["/d/sub/"], "a folder only");
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(not(windows))]
    #[test]
    fn escapes_and_brackets_in_file_names() {
        let root = folder("escapes", &["d/x*y.ps", "d/[k].ps", "d/a.ps"]);
        assert_eq!(found(&root, "d/x\\*y.ps"), ["/d/x*y.ps"]);
        assert_eq!(found(&root, "d/[k].ps"), ["/d/[k].ps"]);
        assert_eq!(found(&root, "d/\\[k\\].*"), ["/d/[k].ps"]);
        assert!(found(&root, "d/[a].ps").is_empty(), "not a class");
        std::fs::remove_dir_all(&root).ok();
    }

    /// On Windows `\` separates, and the name keeps whichever the template
    /// used: what a program asks for with `/` comes back with `/`.
    #[cfg(windows)]
    #[test]
    fn backslash_separates_on_windows() {
        let root = folder("windows", &["d/a.ps"]);
        assert_eq!(found(&root, "d/*.ps"), ["/d/a.ps"]);
        assert_eq!(found(&root, "d\\*.ps"), ["/d\\a.ps"]);
        std::fs::remove_dir_all(&root).ok();
    }
}
