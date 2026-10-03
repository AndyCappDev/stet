// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A job leaves nothing behind in VM, so one `Interpreter` can run any
//! number of them.
//!
//! Two things made each render call grow VM for good. The library installed
//! its device before the job's `save`, so the job's `restore` never reclaimed
//! what that `setpagedevice` allocated. And `setpagedevice` copied every page
//! device into global VM, which `restore` never reclaims and stet has no
//! garbage collector for — about 8 KB per call, from the CLI as well.
//!
//! Files too: a job's files are closed when read to their end, and the
//! job's `restore` closes the rest and reclaims their slots (PLRM 3e `file`).

use stet::Interpreter;

/// Entities in each of the six VM stores — local then global dicts, arrays
/// and strings — and files.
fn vm(interp: &mut Interpreter) -> [usize; 7] {
    let c = interp.context();
    [
        c.dicts.local.entities.len(),
        c.arrays.local.entities.len(),
        c.strings.local.entities.len(),
        c.dicts.global.entities.len(),
        c.arrays.global.entities.len(),
        c.strings.global.entities.len(),
        c.files.len(),
    ]
}

const JOBS: [&str; 4] = [
    "%!PS\n",
    "%!PS\n<< /PageSize [300 300] >> setpagedevice 0 0 10 10 rectfill showpage\n",
    "%!PS\nsave << /PageSize [200 200] >> setpagedevice showpage restore \
     << >> setpagedevice currentpagedevice pop showpage\n",
    // Files left open, which only the job's restore closes.
    "%!PS\n(616263>) /ASCIIHexDecode filter (abc) 0 () /SubFileDecode filter pop pop\n",
];

/// Runs `call` once to load what a first call loads, then checks that
/// further calls leave VM as they found it.
fn leaves_nothing(what: &str, mut call: impl FnMut(&mut Interpreter, &[u8])) {
    for job in JOBS {
        let mut interp = Interpreter::new();
        call(&mut interp, job.as_bytes());
        let before = vm(&mut interp);
        for _ in 0..3 {
            call(&mut interp, job.as_bytes());
        }
        assert_eq!(vm(&mut interp), before, "{what}: {job:?}");
    }
}

#[test]
fn exec_leaves_nothing() {
    leaves_nothing("exec", |i, job| i.exec(job).unwrap());
}

#[test]
fn render_to_display_list_leaves_nothing() {
    leaves_nothing("render_to_display_list", |i, job| {
        i.render_to_display_list(job, 72.0).unwrap();
    });
}

#[cfg(feature = "render")]
#[test]
fn render_leaves_nothing() {
    leaves_nothing("render", |i, job| {
        i.render(job, 72.0).unwrap();
    });
}

#[cfg(feature = "pdf-output")]
#[test]
fn render_to_pdf_leaves_nothing() {
    leaves_nothing("render_to_pdf", |i, job| {
        i.render_to_pdf(job, 72.0).unwrap();
    });
}

/// An EPS goes through its own path, which sizes the device from its
/// bounding box.
#[test]
fn an_eps_leaves_nothing() {
    let eps = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 50 50\n0 0 10 10 rectfill\n";
    let mut interp = Interpreter::new();
    interp.render_to_display_list(eps, 72.0).unwrap();
    let before = vm(&mut interp);
    for _ in 0..3 {
        interp.render_to_display_list(eps, 72.0).unwrap();
    }
    assert_eq!(vm(&mut interp), before);
}

/// Every file a job reads to its end is closed, as PLRM 3e `file` has it,
/// which is what releases it: the job's own source, which the library copies
/// into memory, among them. Nothing closed a file but `closefile`, so each
/// call kept a copy of its job for the life of the `Interpreter`.
#[test]
fn a_job_leaves_no_file_open() {
    use stet_core::object::EntityId;
    let jobs = [
        "%!PS\n0 0 10 10 rectfill showpage\n",
        "%!PS\n(616263>) /ASCIIHexDecode filter 10 string readstring pop pop\n",
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 50 50\n0 0 10 10 rectfill\n",
    ];
    let open = |interp: &mut Interpreter| -> Vec<u32> {
        let files = &interp.context().files;
        // 0, 1 and 2 are %stdin, %stdout and %stderr.
        (3..files.len() as u32)
            .filter(|&i| files.is_open(EntityId::local(i)))
            .collect()
    };
    let mut interp = Interpreter::new();
    for job in jobs {
        interp.exec(job.as_bytes()).unwrap();
        assert_eq!(open(&mut interp), [0u32; 0], "exec {job:?}");
        interp.render_to_display_list(job.as_bytes(), 72.0).unwrap();
        assert_eq!(open(&mut interp), [0u32; 0], "render {job:?}");
        #[cfg(feature = "pdf-output")]
        {
            interp.render_to_pdf(job.as_bytes(), 72.0).unwrap();
            assert_eq!(open(&mut interp), [0u32; 0], "pdf {job:?}");
        }
    }
}
