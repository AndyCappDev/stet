// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared interpreter initialization logic.
//!
//! Extracts the common bootstrap sequence from stet-cli and stet-wasm into
//! a single reusable function.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_engine::eval::parse_and_exec;

use crate::StetError;
use crate::embedded_resources;

/// A Write implementation that discards all output.
struct NullWriter;

impl std::io::Write for NullWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Create a fully initialized PostScript interpreter context.
///
/// Steps:
///   1. Create Context with empty stores/stacks
///   2. Wire exec_sync for synchronous PS procedure execution
///   3. Register ~268 native operators into systemdict
///   4. Register embedded resource files (fonts, init scripts, encodings)
///   5. Configure ICC color management
///   6. Run init scripts (sysdict.ps → resourcecategories.ps → fontcategory.ps → fontmapping.ps)
///   7. Sync well-known dict EntityIds
///   8. Set systemdict read-only
pub fn create_initialized_context(
    use_icc: bool,
    suppress_output: bool,
) -> Result<Context, StetError> {
    let mut ctx = Context::new();

    // Wire exec_sync for operators that need synchronous PS procedure execution
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);

    // Register all native operators into systemdict
    stet_ops::build_system_dict(&mut ctx);

    // Register embedded resource files in virtual filesystem
    embedded_resources::register_all(&mut ctx.files);
    ctx.font_resource_path = Some("Font".to_string());

    // Configure ICC color management
    if use_icc {
        ctx.icc_cache
            .load_cmyk_profile_bytes(embedded_resources::DEFAULT_CMYK_ICC);
    }

    // Suppress stdout if requested
    if suppress_output {
        ctx.stdout = Box::new(NullWriter);
    }

    // Run init scripts
    run_init_scripts(&mut ctx)?;

    Ok(ctx)
}

/// Run embedded init scripts to bootstrap the PostScript resource system.
fn run_init_scripts(ctx: &mut Context) -> Result<(), StetError> {
    // sysdict.ps expects systemdict as the ONLY dict on the d_stack —
    // it creates and pushes globaldict + userdict itself.
    let saved_d_stack = ctx.d_stack.clone();
    ctx.d_stack.truncate(1);

    // Suppress stdout during init
    let old_stdout = std::mem::replace(&mut ctx.stdout, Box::new(NullWriter));

    ctx.initializing = true;
    ctx.vm_alloc_mode = true;

    let init_script = b"{(resources/Init/sysdict.ps) run} stopped { } if";
    let exec_ok = match parse_and_exec(ctx, init_script) {
        Ok(()) => true,
        Err(PsError::Quit) => true,
        Err(_) => false,
    };

    ctx.stdout = old_stdout;

    if exec_ok && ctx.d_stack.len() >= 3 {
        sync_context_after_init(ctx);
    } else {
        ctx.d_stack = saved_d_stack;
        ctx.o_stack.clear();
        ctx.e_stack.clear();
    }

    ctx.vm_alloc_mode = false;
    ctx.initializing = false;
    ctx.dicts.set_access(
        ctx.systemdict,
        stet_core::object::ObjFlags::ACCESS_READ_ONLY,
    );

    Ok(())
}

/// After init scripts run, update Context fields to match PS-created dicts.
fn sync_context_after_init(ctx: &mut Context) {
    use stet_core::dict::DictKey;
    use stet_core::object::PsValue;

    let sd = ctx.systemdict;
    let lookup = |ctx: &Context, name: &[u8]| -> Option<stet_core::object::EntityId> {
        let id = ctx.names.find(name)?;
        let obj = ctx.dicts.get(sd, &DictKey::Name(id))?;
        match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        }
    };

    if let Some(e) = lookup(ctx, b"$error") {
        ctx.dollar_error = e;
    }
    if let Some(e) = lookup(ctx, b"errordict") {
        ctx.errordict = e;
    }
    if let Some(e) = lookup(ctx, b"FontDirectory") {
        ctx.font_directory = e;
    }
    if let Some(e) = lookup(ctx, b"userdict") {
        ctx.userdict = e;
    }
    if let Some(e) = lookup(ctx, b"globaldict") {
        ctx.globaldict = e;
    }
}
