// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `FileAccess::Safer` and the permits: a program reaches only stet's own
//! resources and what it is permitted, and a refusal never says whether a
//! file exists.

use std::path::{Path, PathBuf};

use stet::{FileAccess, Interpreter};

/// A scratch folder holding `inside/` and `outside/`, each with a file
/// `a.txt`, and on Unix `inside/out`, a link to `outside`.
fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("stet-fa-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    for dir in ["inside", "outside"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join("a.txt"), b"/ran true def\n").unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("outside"), root.join("inside/out")).unwrap();
    root
}

/// `path` as a PostScript string's contents.
fn ps(path: &Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// Run `proc` in `interp`. `None` expects it to run through, `Some(name)`
/// to stop with that error.
#[track_caller]
fn expect(interp: &mut Interpreter, proc: &str, error: Option<&str>) {
    let check = match error {
        None => format!("{{ {proc} }} stopped {{ 1 0 div }} if"),
        Some(name) => format!(
            "{{ {proc} }} stopped \
             {{ $error /errorname get /{name} ne {{ 1 0 div }} if }} {{ 1 0 div }} ifelse"
        ),
    };
    let result = interp.exec(format!("{check} clear $error /newerror false put").as_bytes());
    assert!(result.is_ok(), "{proc}: expected {error:?}");
}

fn safer() -> Interpreter {
    Interpreter::builder().suppress_output().safer().build()
}

/// Outside every permit, reading, running, writing, deleting and renaming
/// are each refused with `invalidfileaccess`, the same for a file that
/// exists and one that does not, and `status` and `filenameforall` see
/// neither. Nothing is changed on disk.
#[test]
fn safer_refuses_files_outside_the_permits_alike_whether_they_exist_or_not() {
    let root = scratch("refuse");
    let mut interp = safer();
    let there = ps(&root.join("outside/a.txt"));
    let missing = ps(&root.join("outside/missing.txt"));
    for path in [&there, &missing] {
        expect(
            &mut interp,
            &format!("({path}) (r) file"),
            Some("invalidfileaccess"),
        );
        expect(
            &mut interp,
            &format!("({path}) run"),
            Some("invalidfileaccess"),
        );
        expect(
            &mut interp,
            &format!("({path}) (w) file"),
            Some("invalidfileaccess"),
        );
        expect(
            &mut interp,
            &format!("({path}) (a) file"),
            Some("invalidfileaccess"),
        );
        expect(
            &mut interp,
            &format!("({path}) deletefile"),
            Some("invalidfileaccess"),
        );
        expect(
            &mut interp,
            &format!("({path}) ({path}.moved) renamefile"),
            Some("invalidfileaccess"),
        );
        expect(
            &mut interp,
            &format!("({path}) status {{ 1 0 div }} if"),
            None,
        );
    }
    let outside = ps(&root.join("outside"));
    expect(
        &mut interp,
        &format!("({outside}/*) {{ pop 1 0 div }} 256 string filenameforall"),
        None,
    );
    assert!(root.join("outside/a.txt").exists());
    assert!(!root.join("outside/missing.txt").exists());
    std::fs::remove_dir_all(&root).ok();
}

/// Under `Full`, the default, the same file reads and runs.
#[test]
fn full_access_reads_anywhere() {
    let root = scratch("full");
    let mut interp = Interpreter::builder().suppress_output().build();
    let path = ps(&root.join("outside/a.txt"));
    expect(&mut interp, &format!("({path}) (r) file closefile"), None);
    expect(
        &mut interp,
        &format!("({path}) run ran not {{ 1 0 div }} if"),
        None,
    );
    let mut interp = Interpreter::builder()
        .suppress_output()
        .file_access(FileAccess::Full)
        .build();
    expect(
        &mut interp,
        &format!("({path}) status not {{ 1 0 div }} if"),
        None,
    );
    std::fs::remove_dir_all(&root).ok();
}

/// stet's own resource folder is read under `Safer`, and writing to the
/// process's standard output still works.
#[test]
fn safer_reads_the_resource_folder_and_writes_standard_output() {
    let root = scratch("resources");
    std::fs::create_dir_all(root.join("inside/ProcSet")).unwrap();
    std::fs::write(root.join("inside/ProcSet/Legit.ps"), b"<< /legit true >>\n").unwrap();
    let mut interp = safer();
    interp.context().resource_base_path = Some(root.join("inside").to_string_lossy().into_owned());
    expect(
        &mut interp,
        "/Legit /ProcSet findresource /legit get not { 1 0 div } if",
        None,
    );
    let path = ps(&root.join("inside/a.txt"));
    expect(&mut interp, &format!("({path}) run"), None);
    expect(
        &mut interp,
        &format!("({path}) (w) file"),
        Some("invalidfileaccess"),
    );
    expect(
        &mut interp,
        "(%stdout) (w) file dup () writestring closefile",
        None,
    );
    std::fs::remove_dir_all(&root).ok();
}

/// A folder permitted for reading reads and runs what is inside it, but
/// not its sibling, a `..` out of it, or a link out of it; and the permit
/// alone restricts the rest.
#[test]
fn a_read_permit_covers_its_folder_only() {
    let root = scratch("read");
    let mut interp = Interpreter::builder().suppress_output().build();
    interp.permit_read(root.join("inside")).unwrap();
    let inside = ps(&root.join("inside/a.txt"));
    expect(&mut interp, &format!("({inside}) (r) file closefile"), None);
    expect(
        &mut interp,
        &format!("({inside}) run ran not {{ 1 0 div }} if"),
        None,
    );
    expect(
        &mut interp,
        &format!("({inside}) status not {{ 1 0 div }} if"),
        None,
    );
    expect(
        &mut interp,
        &format!("({inside}) (w) file"),
        Some("invalidfileaccess"),
    );
    for path in [
        root.join("outside/a.txt"),
        root.join("inside/../outside/a.txt"),
        #[cfg(unix)]
        root.join("inside/out/a.txt"),
    ] {
        let path = ps(&path);
        expect(
            &mut interp,
            &format!("({path}) (r) file"),
            Some("invalidfileaccess"),
        );
        expect(
            &mut interp,
            &format!("({path}) run"),
            Some("invalidfileaccess"),
        );
    }
    let listed = ps(&root.join("inside"));
    expect(
        &mut interp,
        &format!("0 ({listed}/*) {{ pop 1 add }} 256 string filenameforall 1 lt {{ 1 0 div }} if"),
        None,
    );
    std::fs::remove_dir_all(&root).ok();
}

/// A file permitted for reading reads; the rest of its folder does not.
#[test]
fn a_read_permit_on_a_file_covers_only_it() {
    let root = scratch("readfile");
    std::fs::write(root.join("inside/b.txt"), b"b").unwrap();
    let mut interp = Interpreter::builder().suppress_output().build();
    interp.permit_read(root.join("inside/a.txt")).unwrap();
    let a = ps(&root.join("inside/a.txt"));
    let b = ps(&root.join("inside/b.txt"));
    expect(&mut interp, &format!("({a}) (r) file closefile"), None);
    expect(
        &mut interp,
        &format!("({b}) (r) file"),
        Some("invalidfileaccess"),
    );
    std::fs::remove_dir_all(&root).ok();
}

/// A folder permitted for writing takes `(w)` and `(a)`, refuses `(r)`
/// without a read permit, and takes `(r+)` only with both. A permitted
/// file that does not exist yet can be created.
#[test]
fn a_write_permit_writes_and_needs_read_for_r_plus() {
    let root = scratch("write");
    let mut interp = Interpreter::builder().suppress_output().build();
    interp.permit_write(root.join("inside")).unwrap();
    interp.permit_write(root.join("outside/new.txt")).unwrap();
    let a = ps(&root.join("inside/a.txt"));
    expect(
        &mut interp,
        &format!("({a}) (w) file dup (w) writestring closefile"),
        None,
    );
    expect(
        &mut interp,
        &format!("({a}) (a) file dup (a) writestring closefile"),
        None,
    );
    assert_eq!(std::fs::read(root.join("inside/a.txt")).unwrap(), b"wa");
    expect(
        &mut interp,
        &format!("({a}) (r) file"),
        Some("invalidfileaccess"),
    );
    expect(
        &mut interp,
        &format!("({a}) (r+) file"),
        Some("invalidfileaccess"),
    );
    interp.permit_read(root.join("inside")).unwrap();
    expect(&mut interp, &format!("({a}) (r+) file closefile"), None);

    let new = ps(&root.join("outside/new.txt"));
    expect(
        &mut interp,
        &format!("({new}) (w) file dup (n) writestring closefile"),
        None,
    );
    assert_eq!(std::fs::read(root.join("outside/new.txt")).unwrap(), b"n");
    let other = ps(&root.join("outside/other.txt"));
    expect(
        &mut interp,
        &format!("({other}) (w) file"),
        Some("invalidfileaccess"),
    );
    std::fs::remove_dir_all(&root).ok();
}

/// `deletefile` needs a control permit, and `renamefile` control on both
/// names and write on the new one.
#[test]
fn deleting_and_renaming_need_a_control_permit() {
    let root = scratch("control");
    let mut interp = Interpreter::builder().suppress_output().build();
    interp.permit_read(root.join("inside")).unwrap();
    let a = ps(&root.join("inside/a.txt"));
    let moved = ps(&root.join("inside/moved.txt"));
    expect(
        &mut interp,
        &format!("({a}) deletefile"),
        Some("invalidfileaccess"),
    );

    interp.permit_control(root.join("inside")).unwrap();
    expect(
        &mut interp,
        &format!("({a}) ({moved}) renamefile"),
        Some("invalidfileaccess"),
    );
    assert!(root.join("inside/a.txt").exists(), "renamed without write");

    interp.permit_write(root.join("inside")).unwrap();
    expect(&mut interp, &format!("({a}) ({moved}) renamefile"), None);
    assert!(root.join("inside/moved.txt").exists());
    let out = ps(&root.join("outside/moved.txt"));
    expect(
        &mut interp,
        &format!("({moved}) ({out}) renamefile"),
        Some("invalidfileaccess"),
    );
    expect(&mut interp, &format!("({moved}) deletefile"), None);
    assert!(!root.join("inside/moved.txt").exists());

    let outside = ps(&root.join("outside/a.txt"));
    expect(
        &mut interp,
        &format!("({outside}) deletefile"),
        Some("invalidfileaccess"),
    );
    assert!(root.join("outside/a.txt").exists());
    std::fs::remove_dir_all(&root).ok();
}

/// `permit_all` is the three at once, and a permit naming a folder that
/// does not exist is an error.
#[test]
fn permit_all_and_a_missing_folder() {
    let root = scratch("all");
    let mut interp = Interpreter::builder().suppress_output().build();
    interp.permit_all(root.join("inside")).unwrap();
    let a = ps(&root.join("inside/a.txt"));
    let b = ps(&root.join("inside/b.txt"));
    expect(&mut interp, &format!("({a}) (r+) file closefile"), None);
    expect(
        &mut interp,
        &format!("({a}) ({b}) renamefile ({b}) deletefile"),
        None,
    );
    assert!(interp.permit_read(root.join("nowhere/x")).is_err());
    std::fs::remove_dir_all(&root).ok();
}

/// `.loadfont` reads a font from its folder on disk under `Safer` only
/// when that folder is permitted.
#[test]
fn loadfont_reads_from_disk_only_where_permitted() {
    let root = scratch("loadfont");
    let bundled =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/Font/NimbusSans-Regular.t1");
    std::fs::copy(bundled, root.join("outside/Planted.t1")).unwrap();
    let fonts = root.join("outside").to_string_lossy().into_owned();

    let mut interp = safer();
    interp.context().font_resource_path = Some(fonts.clone());
    expect(&mut interp, "/Planted .loadfont { 1 0 div } if", None);

    let mut interp = Interpreter::builder().suppress_output().build();
    interp.context().font_resource_path = Some(fonts);
    interp.permit_read(root.join("outside")).unwrap();
    expect(
        &mut interp,
        "/Planted .loadfont not { 1 0 div } if pop",
        None,
    );
    std::fs::remove_dir_all(&root).ok();
}

/// stet's own fonts and resources load under `Safer` as under `Full`: a
/// page of text renders the same.
#[test]
fn safer_renders_text_as_full_does() {
    let page = b"%!PS\n/Times-Roman findfont /FontName get /Times-Roman ne { 1 0 div } if\n\
        /Times-Roman findfont 24 scalefont setfont 10 10 moveto (Safer) show showpage\n";
    let full = Interpreter::builder()
        .suppress_output()
        .build()
        .render(page, 72.0)
        .unwrap();
    let mut interp = safer();
    let pages = interp.render(page, 72.0).unwrap();
    assert_eq!(pages.len(), 1);
    assert!(interp.warnings().is_empty(), "{:?}", interp.warnings());
    assert_eq!(pages[0].rgba, full[0].rgba);
}

/// A file from the embedding program's provider is embedded as a built-in
/// resource is: under `safer` it opens by its own name, and a spelling with
/// a leading `/` is a host path, refused for both alike.
#[test]
fn safer_opens_a_provided_file_by_its_exact_name_only() {
    let mut interp = safer();
    interp
        .context()
        .files
        .set_embedded_provider(Some(Box::new(|path: &str| {
            (path == "resources/ProcSet/Provided.ps")
                .then(|| std::sync::Arc::from(&b"/provided true def\n"[..]))
        })));
    expect(
        &mut interp,
        "(resources/ProcSet/Provided.ps) (r) file cvx exec provided not { 1 0 div } if",
        None,
    );
    expect(
        &mut interp,
        "(resources/Init/sysdict.ps) (r) file closefile",
        None,
    );
    for name in ["/resources/ProcSet/Provided.ps", "/resources/Init/sysdict.ps"] {
        expect(
            &mut interp,
            &format!("({name}) (r) file"),
            Some("invalidfileaccess"),
        );
    }
}
