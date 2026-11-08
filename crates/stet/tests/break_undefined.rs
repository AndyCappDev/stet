// `break` is not a PostScript operator: neither the PLRM nor Ghostscript
// defines it, and Adobe's own prologs rely on that. Adobe_pattern_AI3 (the
// pattern procset Illustrator 3 to 5.5 writes into every EPS) raises the font
// cache limit in steps of 10,000 inside `{ ... } loop } stopped`, leaving the
// loop with `break` when the limit passes `bmax`; on a printer `break` raises
// `undefined` and `stopped` ends the loop. As a no-op, the loop never ended
// and every such EPS ran into the time limit.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use stet::Interpreter;

fn probe(source: &'static str) -> bool {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut interp = Interpreter::builder().suppress_output().build();
        let ctx = interp.context();
        let ok = stet_engine::eval::parse_and_exec(ctx, source.as_bytes()).is_ok()
            && matches!(
                ctx.o_stack.peek(0).map(|o| o.value),
                Ok(stet_core::object::PsValue::Bool(true))
            );
        let _ = tx.send(ok);
    });
    rx.recv_timeout(Duration::from_secs(30))
        .expect("the program finishes")
}

#[test]
fn break_is_not_defined() {
    assert!(probe("systemdict /break known not"));
}

#[test]
fn the_illustrator_pattern_prolog_leaves_its_cache_loop() {
    assert!(probe(
        "mark cachestatus 7 1 roll pop pop pop pop exch pop exch \
         { { 10000 add dup 2 index gt { break } if dup setcachelimit } loop } stopped \
         cleartomark true"
    ));
}
