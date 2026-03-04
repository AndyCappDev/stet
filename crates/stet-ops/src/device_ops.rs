// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Page device operators: setpagedevice, currentpagedevice, nulldevice,
//! and internal continuation operators for showpage/copypage protocol.

use stet_core::context::Context;
use stet_core::device::NullDevice;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::graphics_state::{GraphicsState, Matrix, PathSegment};
use stet_core::object::{EntityId, ObjFlags, PsObject, PsValue};

// ---------- Page device dict helpers ----------

/// Read a `[x y]` numeric pair from the page device dict.
pub fn get_pd_f64_pair(ctx: &Context, key: &[u8]) -> Result<(f64, f64), PsError> {
    let pd = ctx.gstate.page_device.ok_or(PsError::Undefined)?;
    let name_id = ctx.names.find(key).ok_or(PsError::Undefined)?;
    let obj = ctx
        .dicts
        .get(pd, &DictKey::Name(name_id))
        .ok_or(PsError::Undefined)?;
    match obj.value {
        PsValue::Array { entity, start, len } if len >= 2 => {
            let elems = ctx.arrays.get(entity, start, len);
            let x = elems[0].as_f64().ok_or(PsError::TypeCheck)?;
            let y = elems[1].as_f64().ok_or(PsError::TypeCheck)?;
            Ok((x, y))
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Store a `[v1 v2]` array into the page device dict.
pub fn set_pd_array(ctx: &mut Context, key: &[u8], values: &[f64]) {
    if let Some(pd) = ctx.gstate.page_device {
        let name_id = ctx.names.intern(key);
        let items: Vec<PsObject> = values.iter().map(|&v| PsObject::real(v)).collect();
        let entity = crate::vm_ops::alloc_array_from(ctx, &items);
        let arr = crate::vm_ops::make_array_obj(ctx, entity, items.len() as u32);
        ctx.cow_check_dict(pd);
        ctx.dicts.put(pd, DictKey::Name(name_id), arr);
    }
}

/// Check if a key exists in the page device dict.
pub fn has_pd_key(ctx: &Context, key: &[u8]) -> bool {
    if let Some(pd) = ctx.gstate.page_device
        && let Some(name_id) = ctx.names.find(key)
    {
        return ctx.dicts.known(pd, &DictKey::Name(name_id));
    }
    false
}

/// Check if the current page device is a null device.
pub fn is_null_device(ctx: &Context) -> bool {
    if let Some(pd) = ctx.gstate.page_device
        && let Some(name_id) = ctx.names.find(b".NullDevice")
        && let Some(obj) = ctx.dicts.get(pd, &DictKey::Name(name_id))
    {
        return matches!(obj.value, PsValue::Bool(true));
    }
    false
}

/// Read an integer from the page device dict.
pub fn get_pd_int(ctx: &Context, key: &[u8]) -> Result<i32, PsError> {
    let pd = ctx.gstate.page_device.ok_or(PsError::Undefined)?;
    let name_id = ctx.names.find(key).ok_or(PsError::Undefined)?;
    let obj = ctx
        .dicts
        .get(pd, &DictKey::Name(name_id))
        .ok_or(PsError::Undefined)?;
    match obj.value {
        PsValue::Int(v) => Ok(v),
        _ => Err(PsError::TypeCheck),
    }
}

/// Set an integer in the page device dict.
pub fn set_pd_int(ctx: &mut Context, key: &[u8], val: i32) {
    if let Some(pd) = ctx.gstate.page_device {
        let name_id = ctx.names.intern(key);
        ctx.cow_check_dict(pd);
        ctx.dicts
            .put(pd, DictKey::Name(name_id), PsObject::int(val));
    }
}

/// Read a procedure/value from the page device dict.
pub fn get_pd_value(ctx: &Context, key: &[u8]) -> Option<PsObject> {
    let pd = ctx.gstate.page_device?;
    let name_id = ctx.names.find(key)?;
    ctx.dicts.get(pd, &DictKey::Name(name_id))
}

// ---------- setpagedevice ----------

/// `setpagedevice`: dict → —
///
/// Merges the request dictionary into the current page device dictionary.
/// If this is the first call or the OutputDevice changes, loads the device
/// definition from `resources/OutputDevice/{name}.ps`.
pub fn op_setpagedevice(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let req_obj = ctx.o_stack.peek(0)?;
    let req_entity = match req_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;

    // If the request dict itself has .IsPageDevice, it's a full device dict
    // (loaded by findresource before calling setpagedevice). Use it directly.
    // Otherwise, COW copy the existing page_device and merge request entries.
    let is_page_name = ctx.names.find(b".IsPageDevice");
    let req_is_full = is_page_name
        .and_then(|id| ctx.dicts.get(req_entity, &DictKey::Name(id)))
        .is_some();

    let base_pd = if req_is_full {
        // Full device dict from findresource — use directly
        req_entity
    } else if let Some(old_pd) = ctx.gstate.page_device {
        // COW copy of existing page device, then merge request
        let new_pd = crate::vm_ops::alloc_dict(ctx, 50, b"pagedevice");
        copy_dict(ctx, old_pd, new_pd);
        merge_request_dict(ctx, req_entity, new_pd);
        new_pd
    } else {
        // No existing page device and request isn't a full dict — use request as-is
        req_entity
    };

    // Store as current page device.
    // Must be in global VM so save/restore COW doesn't revert PageCount etc.
    // (PostForge's page device dict is a Python dict not subject to PS VM save/restore.)
    // If the dict is local, copy it to a new global dict.
    let base_pd = if !base_pd.is_global() {
        let new_pd = ctx.dicts.allocate_with(
            ctx.dicts.max_length(base_pd),
            b"pagedevice",
            0,
            true,
            0,
        );
        copy_dict(ctx, base_pd, new_pd);
        new_pd
    } else {
        base_pd
    };
    ctx.gstate.page_device = Some(base_pd);

    // Compute MediaSize from PageSize and HWResolution (with sensible defaults)
    let (pw, ph) = get_pd_f64_pair(ctx, b"PageSize").unwrap_or((612.0, 792.0));
    let (dpi_x, dpi_y) = get_pd_f64_pair(ctx, b"HWResolution").unwrap_or((72.0, 72.0));
    let media_w = (pw * dpi_x / 72.0).round() as u32;
    let media_h = (ph * dpi_y / 72.0).round() as u32;
    set_pd_array(ctx, b"MediaSize", &[media_w as f64, media_h as f64]);

    // Update page_width/page_height for fallback code
    ctx.page_width = pw as u32;
    ctx.page_height = ph as u32;

    // Create/recreate the device via factory
    if let Some(factory) = ctx.device_factory.take() {
        let device = factory(media_w, media_h);
        ctx.device = Some(device);
        ctx.device_factory = Some(factory);
    }

    // Compute CTM from HWResolution
    let scale_x = dpi_x / 72.0;
    let scale_y = dpi_y / 72.0;
    let ctm = Matrix::new(scale_x, 0.0, 0.0, -scale_y, 0.0, media_h as f64);
    ctx.gstate.ctm = ctm;
    ctx.gstate.default_ctm = ctm;

    // Reset graphics state (preserve page_device and CTM)
    let page_device = ctx.gstate.page_device;
    let default_ctm = ctx.gstate.default_ctm;
    let saved_ctm = ctx.gstate.ctm;
    ctx.gstate = GraphicsState::new();
    ctx.gstate.page_device = page_device;
    ctx.gstate.ctm = saved_ctm;
    ctx.gstate.default_ctm = default_ctm;

    // Init clip on device
    if let Some(ref mut device) = ctx.device {
        device.init_clip();
        device.erase_page();
    }

    // Push Install and BeginPage procs on e_stack for execution.
    // e_stack is LIFO: last pushed runs first.
    // We want execution order: Install first, then BeginPage (with PageCount on o_stack).
    // So push order is: BeginPage setup (bottom), Install (top).
    let install_obj = get_pd_value(ctx, b"Install").filter(is_nonempty_proc);
    let begin_obj = get_pd_value(ctx, b"BeginPage").filter(is_nonempty_proc);

    // e_stack is LIFO: last pushed runs first.
    // Desired execution order: Install, then push PageCount, then BeginPage.
    // So push order (bottom→top): BeginPage, PageCount literal, Install.
    if let Some(begin_obj) = begin_obj {
        let page_count = get_pd_int(ctx, b"PageCount").unwrap_or(0);
        ctx.e_stack.push(begin_obj)?;
        // A literal int on e_stack gets pushed to o_stack by the eval loop
        ctx.e_stack.push(PsObject::int(page_count))?;
    }

    if let Some(install_obj) = install_obj {
        ctx.e_stack.push(install_obj)?;
    }

    Ok(())
}

/// `currentpagedevice`: — → dict
///
/// Returns a read-only copy of the current page device dictionary.
pub fn op_currentpagedevice(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(pd) = ctx.gstate.page_device {
        // Create a read-only copy
        let copy = crate::vm_ops::alloc_dict(ctx, 50, b"pagedevice");
        copy_dict(ctx, pd, copy);
        ctx.dicts.set_access(copy, ObjFlags::ACCESS_READ_ONLY);
        let mut obj = PsObject::dict(copy);
        obj.flags = ObjFlags::new(ObjFlags::ACCESS_READ_ONLY, false, false, false);
        ctx.o_stack.push(obj)?;
    } else {
        // No page device: push empty dict
        let entity = crate::vm_ops::alloc_dict(ctx, 0, b"pagedevice");
        ctx.o_stack.push(PsObject::dict(entity))?;
    }
    Ok(())
}

/// `nulldevice`: — → — (install a null rendering device)
pub fn op_nulldevice(ctx: &mut Context) -> Result<(), PsError> {
    // Save current OutputDevice name for recovery
    let prev_device_name = get_pd_value(ctx, b"OutputDevice");

    // Create new page_device dict with .NullDevice true
    let pd = crate::vm_ops::alloc_dict(ctx, 10, b"nulldevice");
    let null_dev_name = ctx.names.intern(b".NullDevice");
    ctx.dicts
        .put(pd, DictKey::Name(null_dev_name), PsObject::bool(true));

    if let Some(prev) = prev_device_name {
        let prev_name = ctx.names.intern(b".PrevOutputDevice");
        ctx.dicts.put(pd, DictKey::Name(prev_name), prev);
    }

    // Store dummy PageSize for clippath fallback
    let ps_name = ctx.names.intern(b"PageSize");
    let items = [PsObject::real(0.0), PsObject::real(0.0)];
    let arr_entity = crate::vm_ops::alloc_array_from(ctx, &items);
    let arr_obj = crate::vm_ops::make_array_obj(ctx, arr_entity, 2);
    ctx.dicts.put(pd, DictKey::Name(ps_name), arr_obj);

    ctx.gstate.page_device = Some(pd);

    // Replace device with NullDevice
    let (w, h) = if let Some(ref dev) = ctx.device {
        dev.page_size()
    } else {
        (ctx.page_width, ctx.page_height)
    };
    ctx.device = Some(Box::new(NullDevice::new(w, h)));

    // Set CTM and default CTM to identity matrix
    ctx.gstate.ctm = Matrix::identity();
    ctx.gstate.default_ctm = Matrix::identity();

    // Set clipping to degenerate path (single MoveTo)
    ctx.gstate.clip_path = Some({
        let mut p = stet_core::graphics_state::PsPath::new();
        p.segments.push(PathSegment::MoveTo(0.0, 0.0));
        p
    });

    // Clear current path and current point
    ctx.gstate.path.clear();
    ctx.gstate.current_point = None;

    Ok(())
}

// ---------- copypage ----------

/// `copypage`: — → — (copy current page without erasing)
///
/// Uses the EndPage/BeginPage protocol with reason code 1 if a page device
/// with EndPage is active. Does NOT call erasepage or initgraphics afterward.
pub fn op_copypage(ctx: &mut Context) -> Result<(), PsError> {
    if crate::device_ops::is_null_device(ctx) {
        return Ok(());
    }

    if ctx.gstate.page_device.is_some()
        && let Some(end_page) = get_pd_value(ctx, b"EndPage")
        && is_nonempty_proc(&end_page)
    {
        let page_count = get_pd_int(ctx, b"PageCount").unwrap_or(0);
        ctx.o_stack.push(PsObject::int(page_count))?;
        ctx.o_stack.push(PsObject::int(1))?; // reason 1 = copypage

        let continue_name = ctx.names.intern(b".copypage_continue");
        if let Some(continue_op) = ctx.dict_load(&stet_core::dict::DictKey::Name(continue_name)) {
            ctx.e_stack.push(continue_op)?;
        }
        ctx.e_stack.push(end_page)?;
        return Ok(());
    }

    // Fallback: direct copy (replay but don't clear)
    if ctx.device.is_some() {
        if ctx.output_path.is_some() {
            let list = ctx.take_display_list();
            let device = ctx.device.as_mut().unwrap();
            let path = ctx.output_path.as_ref().unwrap();
            if let Err(e) = device.replay_and_show(list, path) {
                eprintln!("copypage error: {}", e);
            }
        } else {
            let device = ctx.device.as_mut().unwrap();
            stet_core::display_list::replay_to_device(&ctx.display_list, device.as_mut());
        }
    }
    Ok(())
}

// ---------- showpage continuation ----------

/// `.showpage_continue`: internal operator called after EndPage proc completes.
///
/// Pops the bool result from EndPage. If true, renders the page and generates
/// the output file. Then calls erasepage, initgraphics, and pushes BeginPage.
pub fn op_showpage_continue(ctx: &mut Context) -> Result<(), PsError> {
    // Pop EndPage result (bool)
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let result = ctx.o_stack.pop()?;
    let should_render = match result.value {
        PsValue::Bool(b) => b,
        _ => true, // default to rendering if EndPage returned non-bool
    };

    if should_render {
        // Increment PageCount
        let page_count = get_pd_int(ctx, b"PageCount").unwrap_or(0) + 1;
        set_pd_int(ctx, b"PageCount", page_count);

        // Replay display list and render
        let list = ctx.take_display_list();
        if let Some(ref mut device) = ctx.device {
            let output_path = generate_output_path(ctx.output_path.as_deref(), page_count);
            if let Err(e) = device.replay_and_show(list, &output_path) {
                eprintln!("showpage error: {}", e);
            }
        }
    } else {
        ctx.display_list.clear();
    }

    // Erase page (direct device call — post-showpage cleanup)
    if let Some(ref mut device) = ctx.device {
        device.erase_page();
    }

    // Reset graphics state (preserves page_device and current_font per PLRM)
    let page_device = ctx.gstate.page_device;
    let default_ctm = ctx.gstate.default_ctm;
    let current_font = ctx.gstate.current_font;
    ctx.gstate = GraphicsState::new();
    ctx.gstate.page_device = page_device;
    ctx.gstate.current_font = current_font;

    // initmatrix from page_device
    if page_device.is_some() && !is_null_device(ctx) {
        if let Ok((_pw, ph)) = get_pd_f64_pair(ctx, b"PageSize")
            && let Ok((dpi_x, dpi_y)) = get_pd_f64_pair(ctx, b"HWResolution")
        {
            let scale_x = dpi_x / 72.0;
            let scale_y = dpi_y / 72.0;
            let media_h = (ph * scale_y).round() as u32;
            let ctm = Matrix::new(scale_x, 0.0, 0.0, -scale_y, 0.0, media_h as f64);
            ctx.gstate.ctm = ctm;
            ctx.gstate.default_ctm = ctm;
        }
    } else {
        ctx.gstate.ctm = default_ctm;
        ctx.gstate.default_ctm = default_ctm;
    }

    // Init clip
    if let Some(ref mut device) = ctx.device {
        device.init_clip();
    }

    // Note: gstate_stack is NOT cleared by showpage (per PLRM / PostForge).
    // Programs like dvi_ps rely on gsave/grestore around showpage to preserve
    // coordinate system setup across page boundaries.

    // Push BeginPage for execution
    if let Some(begin_obj) = get_pd_value(ctx, b"BeginPage")
        && is_nonempty_proc(&begin_obj)
    {
        let page_count = get_pd_int(ctx, b"PageCount").unwrap_or(0);
        ctx.o_stack.push(PsObject::int(page_count))?;
        ctx.e_stack.push(begin_obj)?;
    }

    Ok(())
}

/// `.copypage_continue`: internal operator called after EndPage for copypage.
///
/// Same as showpage_continue but does NOT erase page or call initgraphics.
pub fn op_copypage_continue(ctx: &mut Context) -> Result<(), PsError> {
    // Pop EndPage result (bool)
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let result = ctx.o_stack.pop()?;
    let should_render = match result.value {
        PsValue::Bool(b) => b,
        _ => true,
    };
    if should_render {
        let page_count = get_pd_int(ctx, b"PageCount").unwrap_or(0) + 1;
        set_pd_int(ctx, b"PageCount", page_count);

        // Replay display list (copypage preserves page content, but we must
        // transfer ownership for pipelined rendering)
        let list = ctx.take_display_list();
        if let Some(ref mut device) = ctx.device {
            let output_path = generate_output_path(ctx.output_path.as_deref(), page_count);
            if let Err(e) = device.replay_and_show(list, &output_path) {
                eprintln!("copypage error: {}", e);
            }
        }
    }

    // copypage does NOT erase or call initgraphics
    Ok(())
}

// ---------- Internal helpers ----------

/// Copy all entries from one dict to another.
fn copy_dict(ctx: &mut Context, src: EntityId, dst: EntityId) {
    let entries: Vec<(DictKey, PsObject)> = ctx
        .dicts
        .entry(src)
        .entries
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    for (key, value) in entries {
        ctx.dicts.put(dst, key, value);
    }
}

/// Merge request dict entries into page device dict.
/// Skips HWResolution unless `allow_ps_resolution` is set (WASM mode).
fn merge_request_dict(ctx: &mut Context, req: EntityId, pd: EntityId) {
    let hw_res_name = if !ctx.allow_ps_resolution {
        ctx.names.find(b"HWResolution")
    } else {
        None
    };
    let entries: Vec<(DictKey, PsObject)> = ctx
        .dicts
        .entry(req)
        .entries
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    for (key, value) in entries {
        // Skip HWResolution in CLI mode (user-controlled)
        if let DictKey::Name(id) = &key
            && hw_res_name == Some(*id)
        {
            continue;
        }
        ctx.cow_check_dict(pd);
        ctx.dicts.put(pd, key, value);
    }
}

/// Check if an object is a non-empty executable array (procedure).
fn is_nonempty_proc(obj: &PsObject) -> bool {
    match obj.value {
        PsValue::Array { len, .. } | PsValue::ExecArray { len, .. } => {
            len > 0 && obj.flags.is_executable()
        }
        _ => false,
    }
}

/// Generate the output file path for a given page number.
///
/// Pattern: `{basename}-{pagenum:04d}.png`
fn generate_output_path(base_path: Option<&str>, page_count: i32) -> String {
    match base_path {
        Some(path) => {
            // Strip extension, add page number
            let base = if let Some(pos) = path.rfind('.') {
                &path[..pos]
            } else {
                path
            };
            format!("{}-{:04}.png", base, page_count)
        }
        None => format!("output-{:04}.png", page_count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;

    fn setup() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx
    }

    #[test]
    fn test_generate_output_path() {
        assert_eq!(generate_output_path(Some("test.png"), 1), "test-0001.png");
        assert_eq!(
            generate_output_path(Some("/tmp/foo.ps.png"), 3),
            "/tmp/foo.ps-0003.png"
        );
        assert_eq!(generate_output_path(None, 1), "output-0001.png");
    }

    #[test]
    fn test_nulldevice_sets_identity_ctm() {
        let mut ctx = setup();
        ctx.gstate.ctm = Matrix::new(2.0, 0.0, 0.0, -2.0, 0.0, 100.0);
        op_nulldevice(&mut ctx).unwrap();
        assert!((ctx.gstate.ctm.a - 1.0).abs() < 1e-10);
        assert!((ctx.gstate.ctm.d - 1.0).abs() < 1e-10);
        assert!(is_null_device(&ctx));
    }

    #[test]
    fn test_nulldevice_degenerate_clip() {
        let mut ctx = setup();
        op_nulldevice(&mut ctx).unwrap();
        assert!(ctx.gstate.clip_path.is_some());
        let clip = ctx.gstate.clip_path.as_ref().unwrap();
        assert_eq!(clip.segments.len(), 1);
        assert!(matches!(clip.segments[0], PathSegment::MoveTo(0.0, 0.0)));
    }

    #[test]
    fn test_currentpagedevice_no_device() {
        let mut ctx = setup();
        op_currentpagedevice(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 1);
        let obj = ctx.o_stack.pop().unwrap();
        assert!(matches!(obj.value, PsValue::Dict(_)));
    }

    #[test]
    fn test_currentpagedevice_with_device() {
        let mut ctx = setup();
        // Set up a page device dict
        let pd = crate::vm_ops::alloc_dict(&mut ctx, 10, b"pd");
        let name_id = ctx.names.intern(b"PageSize");
        let items = [PsObject::real(612.0), PsObject::real(792.0)];
        let arr = ctx.arrays.allocate_from(&items);
        ctx.dicts
            .put(pd, DictKey::Name(name_id), PsObject::array(arr, 2));
        ctx.gstate.page_device = Some(pd);

        op_currentpagedevice(&mut ctx).unwrap();
        let obj = ctx.o_stack.pop().unwrap();
        match obj.value {
            PsValue::Dict(e) => {
                // Should be a copy (different entity) and read-only
                assert_ne!(e, pd);
                assert!(obj.flags.access() == ObjFlags::ACCESS_READ_ONLY);
            }
            _ => panic!("expected dict"),
        }
    }

    #[test]
    fn test_is_null_device() {
        let mut ctx = setup();
        assert!(!is_null_device(&ctx));
        op_nulldevice(&mut ctx).unwrap();
        assert!(is_null_device(&ctx));
    }

    #[test]
    fn test_setpagedevice_typecheck() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::int(42)).unwrap();
        assert_eq!(op_setpagedevice(&mut ctx), Err(PsError::TypeCheck));
    }
}
