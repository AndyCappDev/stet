// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A resource's name never names a folder. `findresource` and `findfont`
//! look for a resource not yet loaded in `resources/<Category>/<key>`, and
//! a key holding `/` or `\` would otherwise reach a file outside it.
//! Each test plants a resource folder of its own, with a file beside it,
//! not in it, that records being run.

use std::path::PathBuf;

use stet::Interpreter;

/// A resource folder holding `ProcSet/Legit.ps` and `Font/LegitFont.t1`,
/// with `outside/planted.ps` and `outside/planted.t1` beside it. Each
/// planted file records that it ran in `userdict`.
fn resource_folder(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("stet-resnames-{tag}-{}", std::process::id()));
    for dir in ["ProcSet", "Font", "outside"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("ProcSet/Legit.ps"), b"<< /legit true >>\n").unwrap();
    std::fs::write(
        root.join("Font/LegitFont.t1"),
        b"/LegitFont /Helvetica findfont definefont pop\n",
    )
    .unwrap();
    std::fs::write(
        root.join("outside/planted.ps"),
        b"userdict /planted_ran true put << /planted true >>\n",
    )
    .unwrap();
    std::fs::write(
        root.join("outside/planted.t1"),
        b"userdict /planted_ran true put\n",
    )
    .unwrap();
    root
}

/// Run `body` against the resource folder `root`; true when it runs
/// through without an error.
fn passes(root: &std::path::Path, body: &str) -> bool {
    let mut interp = Interpreter::builder().suppress_output().build();
    interp.context().resource_base_path = Some(root.to_string_lossy().into_owned());
    interp.exec(body.as_bytes()).is_ok()
}

/// No file outside the category's folder is run, by a `/` or a `\` key, and
/// the lookup ends as for a resource that does not exist.
#[test]
fn a_resource_key_does_not_reach_outside_its_folder() {
    let root = resource_folder("procset");
    assert!(
        passes(
            &root,
            "/Legit /ProcSet findresource /legit get not { 1 0 div } if"
        ),
        "a resource in the folder is found"
    );
    for key in ["../outside/planted", "..\\\\outside\\\\planted"] {
        assert!(
            passes(
                &root,
                &format!(
                    "{{ ({key}) cvn /ProcSet findresource }} stopped \
                     {{ $error /errorname get /undefinedresource ne {{ 1 0 div }} if }} \
                     {{ 1 0 div }} ifelse \
                     userdict /planted_ran known {{ 1 0 div }} if"
                )
            ),
            "{key}: the planted file ran, or the error was not undefinedresource"
        );
    }
    std::fs::remove_dir_all(&root).ok();
}

/// A font name with a `/` or `\` is not looked for on disk: it is
/// substituted like any font not found.
#[test]
fn a_font_name_does_not_reach_outside_the_font_folder() {
    let root = resource_folder("font");
    assert!(
        passes(
            &root,
            "/LegitFont findfont pop FontDirectory /LegitFont known not { 1 0 div } if"
        ),
        "a font in the folder is found"
    );
    for key in ["../outside/planted", "..\\\\outside\\\\planted"] {
        assert!(
            passes(
                &root,
                &format!(
                    "({key}) cvn findfont /FontName get \
                     /Helvetica findfont /FontName get ne {{ 1 0 div }} if \
                     userdict /planted_ran known {{ 1 0 div }} if"
                )
            ),
            "{key}: the planted file ran, or the font was not substituted"
        );
    }
    std::fs::remove_dir_all(&root).ok();
}
