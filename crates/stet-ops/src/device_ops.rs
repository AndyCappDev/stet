// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Page device operators: setpagedevice, currentpagedevice, nulldevice,
//! and internal continuation operators for showpage/copypage protocol.

use stet_core::context::Context;
use stet_core::device::NullDevice;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{EntityId, ObjFlags, PsObject, PsValue};
use stet_core::output_template::{ExpandError, OutputTemplate};
use stet_fonts::geometry::{Matrix, PathSegment};

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
        // Allocate in the page device's own VM, not the ambient `currentglobal`:
        // a local array stored into a global page device would be a PLRM 3.7.2
        // violation, pointing at storage a later `restore` releases.
        let pd_global = pd.is_global();
        let entity = crate::vm_ops::alloc_array_from_in(ctx, &items, pd_global);
        let mut arr = PsObject::array(entity, items.len() as u32);
        if pd_global {
            arr.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, false, true, true);
        }
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
        PsValue::Int(v) => i32::try_from(v).map_err(|_| PsError::RangeCheck),
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

/// Largest page dimension a *file* may declare, in points.
///
/// 14400 pt is 200 inches — the maximum `/MediaBox` extent in Adobe's PDF 1.7
/// implementation limits (Table C.1). PostScript sets no limit of its own, but
/// the same number is applied here so both input paths agree.
///
/// This bounds the untrusted half of the page-size calculation only. Render
/// resolution is deliberately **not** capped: pixel dimensions are
/// `points * dpi / 72`, and while the points come from the file, the DPI is
/// the caller's explicit request. A 1200 dpi prepress proof of a large-format
/// page is a legitimate gigapixel render, and refusing it would be worse than
/// the denial of service it prevents. Bounding the points alone still rejects
/// `<< /PageSize [2000000 2000000] >> setpagedevice` — 27,777 inches is not a
/// page at any resolution.
const MAX_PAGE_SIZE_POINTS: f64 = 14_400.0;

/// Clamp a file-declared page size to something physically meaningful.
///
/// Non-finite and non-positive values fall back to US Letter rather than
/// propagating: a NaN width would otherwise reach the renderer as a zero or
/// garbage pixel count.
fn clamp_page_size(width: f64, height: f64) -> (f64, f64) {
    fn clamp_one(v: f64, fallback: f64) -> f64 {
        if !v.is_finite() || v <= 0.0 {
            fallback
        } else {
            v.min(MAX_PAGE_SIZE_POINTS)
        }
    }
    let w = clamp_one(width, 612.0);
    let h = clamp_one(height, 792.0);
    if w != width || h != height {
        eprintln!(
            "Warning: PageSize [{width} {height}] is outside the supported range; \
             using [{w} {h}] (limit {MAX_PAGE_SIZE_POINTS} pt per side)"
        );
    }
    (w, h)
}

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

    // The outgoing device's EndPage, with reason 2 (PLRM 3e §6.2.6). Every
    // setpagedevice deactivates the current device, as in Ghostscript,
    // including one that only changes a parameter.
    deactivate_page_device(ctx)?;

    // PLRM: when current device is not a page device (e.g. after nulldevice),
    // or when switching to a different output device, create a new device
    // dictionary from scratch before merging request params.
    let is_page_name = ctx.names.intern(b".IsPageDevice");
    let od_name = ctx.names.intern(b"OutputDevice");

    // Check if current page device has .IsPageDevice
    let cur_has_is_page = ctx
        .gstate
        .page_device
        .is_some_and(|pd| ctx.dicts.get(pd, &DictKey::Name(is_page_name)).is_some());

    let mut need_full_reload = !cur_has_is_page;

    // Also reload when switching to a different output device
    if !need_full_reload {
        let req_od = ctx.dicts.get(req_entity, &DictKey::Name(od_name));
        let cur_od = ctx
            .gstate
            .page_device
            .and_then(|pd| ctx.dicts.get(pd, &DictKey::Name(od_name)));
        if let (Some(req), Some(cur)) = (req_od, cur_od)
            && req.value != cur.value
        {
            need_full_reload = true;
        }
    }

    let base_pd = if need_full_reload {
        // Determine device: request dict /OutputDevice > .PrevOutputDevice > fallback
        let prev_od_name = ctx.names.intern(b".PrevOutputDevice");
        let device_name_obj = ctx
            .dicts
            .get(req_entity, &DictKey::Name(od_name))
            .or_else(|| {
                ctx.gstate
                    .page_device
                    .and_then(|pd| ctx.dicts.get(pd, &DictKey::Name(prev_od_name)))
            })
            .or_else(|| {
                // Fallback: use "viewer" if available, else "png"
                let fallback = ctx.names.intern(b"viewer");
                Some(PsObject::name_lit(fallback))
            });
        if let Some(name_obj) = device_name_obj {
            // Load full device dict via findresource on OutputDevice category
            let fr_name = ctx.names.intern(b"findresource");
            let cat_name_id = ctx.names.intern(b"OutputDevice");
            let proc_elements = [
                name_obj,
                PsObject::name_lit(cat_name_id),
                PsObject::name_exec(fr_name),
            ];
            let proc_entity = crate::vm_ops::alloc_array_from(ctx, &proc_elements);
            let proc_obj = PsObject::procedure(proc_entity, proc_elements.len() as u32);
            let dev_entity = if ctx.exec_sync(proc_obj).is_ok() {
                ctx.o_stack.pop().ok().and_then(|obj| {
                    if let PsValue::Dict(e) = obj.value {
                        Some(e)
                    } else {
                        None
                    }
                })
            } else {
                None
            };
            if let Some(dev_entity) = dev_entity {
                // Copy device resource dict, merge request entries on top
                let new_pd = alloc_page_device(ctx);
                copy_dict(ctx, dev_entity, new_pd);
                merge_request_dict(ctx, req_entity, new_pd);
                new_pd
            } else {
                req_entity
            }
        } else {
            req_entity
        }
    } else if let Some(old_pd) = ctx.gstate.page_device {
        // Incremental merge: COW copy of existing page device, then merge request
        let new_pd = alloc_page_device(ctx);
        copy_dict(ctx, old_pd, new_pd);
        merge_request_dict(ctx, req_entity, new_pd);
        new_pd
    } else {
        // No existing page device and request isn't a full dict — use request as-is
        req_entity
    };

    ctx.gstate.page_device = Some(base_pd);
    // A new device starts counting showpages and pages afresh; a parameter
    // change keeps both.
    if need_full_reload {
        ctx.showpage_count = 0;
        ctx.page_count = 0;
    }

    // Compute MediaSize from PageSize and HWResolution (with sensible defaults)
    let (pw, ph) = get_pd_f64_pair(ctx, b"PageSize").unwrap_or((612.0, 792.0));
    let (pw, ph) = clamp_page_size(pw, ph);
    let (dpi_x, dpi_y) = get_pd_f64_pair(ctx, b"HWResolution").unwrap_or((72.0, 72.0));
    let media_w = (pw * dpi_x / 72.0).round() as u32;
    let media_h = (ph * dpi_y / 72.0).round() as u32;
    set_pd_array(ctx, b"MediaSize", &[media_w as f64, media_h as f64]);

    // Update page_width/page_height for fallback code
    ctx.page_width = pw as u32;
    ctx.page_height = ph as u32;

    // Resize the current device if it keeps itself — the PDF device holds
    // every page of the job, which a new device would lose — or make a new
    // one from the factory.
    let media = (media_w, media_h);
    let kept = ctx
        .device
        .as_mut()
        .is_some_and(|device| device.resize_page(media, (pw, ph)));
    if !kept && let Some(factory) = ctx.device_factory.take() {
        let mut device = factory(media_w, media_h);
        device.resize_page(media, (pw, ph));
        ctx.device = Some(device);
        ctx.device_factory = Some(factory);
    }

    // Compute CTM from HWResolution
    let scale_x = dpi_x / 72.0;
    let scale_y = dpi_y / 72.0;
    let ctm = Matrix::new(scale_x, 0.0, 0.0, -scale_y, 0.0, media_h as f64);
    ctx.gstate.ctm = ctm;
    ctx.gstate.default_ctm = ctm;

    // PLRM setpagedevice: "reinitializes everything in the graphics state
    // except the font parameter, including parameters not affected by
    // initgraphics". The page device and CTM were just computed.
    let page_device = ctx.gstate.page_device;
    let default_ctm = ctx.gstate.default_ctm;
    let saved_ctm = ctx.gstate.ctm;
    let current_font = ctx.gstate.current_font;
    let clip_path_version = ctx.gstate.clip_path_version;
    ctx.gstate = ctx.initial_gstate();
    ctx.gstate.page_device = page_device;
    ctx.gstate.current_font = current_font;
    // Advanced, not reset: `grestore` compares versions to decide whether
    // the device clip needs re-emitting.
    ctx.gstate.clip_path_version = clip_path_version + 1;
    ctx.gstate.ctm = saved_ctm;
    ctx.gstate.default_ctm = default_ctm;

    // Init clip on device, and erase the page: whatever was painted before
    // setpagedevice does not carry onto the new device's first page.
    if let Some(ref mut device) = ctx.device {
        device.init_clip();
        device.erase_page();
    }
    ctx.display_list.clear();
    // Erased with the graphics state just reinitialized, transfer function
    // included (PLRM `setpagedevice`), so it is white.
    ctx.page_erase_fill = None;

    // Push Install and BeginPage procs on e_stack for execution.
    // e_stack is LIFO: last pushed runs first.
    // We want execution order: Install first, then BeginPage (with the showpage count on o_stack).
    // So push order is: BeginPage setup (bottom), Install (top).
    let install_obj = get_pd_value(ctx, b"Install").filter(is_nonempty_proc);
    let begin_obj = get_pd_value(ctx, b"BeginPage").filter(is_nonempty_proc);

    // e_stack is LIFO: last pushed runs first.
    // Desired execution order: Install, then push the count, then BeginPage.
    // So push order (bottom→top): BeginPage, count literal, Install.
    if let Some(begin_obj) = begin_obj {
        ctx.e_stack.push(begin_obj)?;
        // A literal int on e_stack gets pushed to o_stack by the eval loop
        ctx.e_stack.push(PsObject::int(ctx.showpage_count))?;
    }

    if let Some(install_obj) = install_obj {
        ctx.e_stack.push(install_obj)?;
    }

    Ok(())
}

/// `currentpagedevice`: — → dict
///
/// Returns a read-only copy of the current page device dictionary, with the
/// device's `PageCount`.
pub fn op_currentpagedevice(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(pd) = ctx.gstate.page_device {
        // Create a read-only copy, in local VM for the reason the page
        // device itself is there (`alloc_page_device`).
        let copy = alloc_page_device(ctx);
        copy_dict(ctx, pd, copy);
        // The count is the device's, not the dict's (`Context::page_count`).
        let page_count = DictKey::Name(ctx.names.intern(b"PageCount"));
        if ctx.dicts.known(copy, &page_count) {
            ctx.dicts
                .put(copy, page_count, PsObject::int(ctx.page_count));
        }
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
    // The program has asked for no output; remember it for the job so the
    // end-of-job dropped-page diagnostic stays quiet (see the field's docs).
    ctx.null_device_used = true;

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
        let mut p = stet_fonts::geometry::PsPath::new();
        p.segments.push(PathSegment::MoveTo(0.0, 0.0));
        p
    });

    // Clear current path and current point
    ctx.gstate.path.clear();
    ctx.gstate.current_point = None;

    Ok(())
}

// ---------- flushpage ----------

/// `flushpage`: — → — (render current page without any state changes)
///
/// Forces immediate rendering of the current page contents to the output
/// device without erasing the page, advancing the page count, or
/// reinitializing the graphics state. Does not call EndPage/BeginPage.
pub fn op_flushpage(ctx: &mut Context) -> Result<(), PsError> {
    if is_null_device(ctx) {
        return Ok(());
    }

    // In viewer mode, send a clone of the display list through the channel
    // without clearing it (unlike showpage's take_display_list).
    if let Some(ref sender) = ctx.display_list_sender {
        let dpi = ctx.current_page_dpi();
        let (w, h) = ctx
            .device
            .as_ref()
            .map(|d| d.page_size())
            .unwrap_or((ctx.page_width, ctx.page_height));
        let _ = sender.send((ctx.display_list.clone(), dpi, w, h, None, false));
        return Ok(());
    }

    // Non-viewer mode: replay directly to device without consuming the list
    if let Some(ref mut device) = ctx.device {
        stet_core::device::replay_to_device(&ctx.display_list, device.as_mut());
    }

    Ok(())
}

// ---------- copypage ----------

/// `copypage`: — → — (transmit the current page)
///
/// In LanguageLevel 3, `copypage` is `showpage` without the `initgraphics`
/// (PLRM 3e, `copypage`): it passes `EndPage` reason code 0 "as if the call
/// were coming from `showpage`", erases the page once it is transmitted, and
/// calls `BeginPage` last. Ghostscript behaves the same. The LanguageLevel 2
/// behaviour — reason 1, the page kept for the next — is gone from the
/// language stet implements.
pub fn op_copypage(ctx: &mut Context) -> Result<(), PsError> {
    if !ctx.group_stack.is_empty() {
        return Err(PsError::RangeCheck);
    }
    if crate::device_ops::is_null_device(ctx) {
        return Ok(());
    }

    if let Some(end_page) = end_page_proc(ctx) {
        ctx.o_stack.push(PsObject::int(ctx.showpage_count))?;
        ctx.o_stack.push(PsObject::int(0))?; // reason 0, as for showpage
        let continue_name = ctx.names.intern(b".copypage_continue");
        if let Some(continue_op) = ctx.dict_load(&stet_core::dict::DictKey::Name(continue_name)) {
            ctx.e_stack.push(continue_op)?;
        }
        ctx.e_stack.push(end_page)?;
        return Ok(());
    }

    // No EndPage procedure: transmit and erase directly.
    transmit_page_without_end_page(ctx);
    Ok(())
}

/// `showpage` and `copypage` without an `EndPage` procedure: transmit the
/// page, then erase it.
pub(crate) fn transmit_page_without_end_page(ctx: &mut Context) {
    ctx.showpage_count += 1;
    lay_down_erase_fill(ctx);
    if ctx.device.is_some() {
        if ctx.output_path.is_some() {
            let list = ctx.take_display_list();
            let device = ctx.device.as_mut().unwrap();
            let path = ctx.output_path.as_ref().unwrap();
            if let Err(e) = device.replay_and_show(list, path) {
                eprintln!("showpage error: {}", e);
            }
        } else {
            let device = ctx.device.as_mut().unwrap();
            stet_core::device::replay_to_device(&ctx.display_list, device.as_mut());
            ctx.display_list.clear();
        }
    } else {
        ctx.display_list.clear();
    }
    erase_sent_page(ctx);
}

/// Put the colour the last erase left (if not white) under the page's marks,
/// as the page is about to be sent.
fn lay_down_erase_fill(ctx: &mut Context) {
    if let Some(fill) = ctx.page_erase_fill.take() {
        let marks = std::mem::take(&mut ctx.display_list);
        let mut page = stet_core::display_list::DisplayList::new();
        page.set_page_group_color_space(marks.page_group_color_space());
        page.push(fill);
        for element in marks.into_elements() {
            page.push(element);
        }
        ctx.display_list = page;
    }
}

/// After a page is sent: erase it, which paints gray 1 through the transfer
/// function in force (PLRM `erasepage`). The colour waits in
/// `page_erase_fill` until the next page is sent.
fn erase_sent_page(ctx: &mut Context) {
    if let Some(ref mut device) = ctx.device {
        device.erase_page();
    }
    ctx.page_erase_fill = crate::paint_ops::erase_fill(ctx);
}

/// The page device's `EndPage` procedure, if it has one.
pub(crate) fn end_page_proc(ctx: &Context) -> Option<PsObject> {
    ctx.gstate.page_device?;
    get_pd_value(ctx, b"EndPage").filter(is_nonempty_proc)
}

// ---------- showpage continuation ----------

/// `.showpage_continue`: internal operator run after `showpage`'s `EndPage`
/// procedure, with its result on the operand stack.
pub fn op_showpage_continue(ctx: &mut Context) -> Result<(), PsError> {
    finish_page(ctx, false)
}

/// `.copypage_continue`: internal operator run after `copypage`'s `EndPage`
/// procedure, with its result on the operand stack.
pub fn op_copypage_continue(ctx: &mut Context) -> Result<(), PsError> {
    finish_page(ctx, true)
}

/// The rest of `showpage` or `copypage` once `EndPage` has returned
/// (PLRM 3e §6.2.6).
///
/// `true`: transmit the page, count it in `PageCount`, and erase it.
/// `false`: neither transmit nor erase — the page carries over, which is
/// how an `EndPage` accumulates several pages on one sheet. Either way
/// `showpage` then performs `initgraphics`, `copypage` does not, and both
/// call `BeginPage` last with the number of executions so far.
fn finish_page(ctx: &mut Context, copypage: bool) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let result = ctx.o_stack.pop()?;
    let transmit = match result.value {
        PsValue::Bool(b) => b,
        _ => true, // default to rendering if EndPage returned non-bool
    };
    ctx.showpage_count += 1;

    if transmit {
        transmit_page(ctx, if copypage { "copypage" } else { "showpage" })?;
        erase_sent_page(ctx);
    }

    if !copypage {
        reinitialize_graphics(ctx);
    }

    // Note: gstate_stack is NOT cleared by showpage (per PLRM). Programs
    // like dvi_ps rely on gsave/grestore around showpage to preserve
    // coordinate system setup across page boundaries.

    if let Some(begin_obj) = get_pd_value(ctx, b"BeginPage")
        && is_nonempty_proc(&begin_obj)
    {
        ctx.o_stack.push(PsObject::int(ctx.showpage_count))?;
        ctx.e_stack.push(begin_obj)?;
    }

    Ok(())
}

/// Transmit the current page, which `EndPage` has accepted: count it in
/// `PageCount`, which numbers the pages produced, and hand the display list
/// to the device unless `--pages` leaves it out.
fn transmit_page(ctx: &mut Context, operator: &str) -> Result<(), PsError> {
    ctx.page_count += 1;
    let page_count = ctx.page_count;

    // pdfmark: track completed-page count so /ANN pdfmarks issued without an
    // explicit /Page key can resolve to "the page being assembled right now"
    // via `current_page + 1`. After N pages we are mid-assembly of page N+1;
    // the operator uses that computation, not raw `current_page`.
    ctx.doc_structure.current_page = page_count as u32;

    let in_filter = ctx
        .page_filter
        .as_ref()
        .is_none_or(|f| f.contains(&page_count));
    lay_down_erase_fill(ctx);
    if in_filter {
        let list = ctx.take_display_list();
        let path = resolve_page_output(ctx, page_count)?;
        if let Some(ref mut device) = ctx.device
            && let Err(e) = device.replay_and_show(list, &path)
        {
            eprintln!("{operator} error: {e}");
        }
    } else {
        ctx.display_list.clear();
    }
    Ok(())
}

/// `showpage`'s `initgraphics`, with the CTM taken from the page device.
fn reinitialize_graphics(ctx: &mut Context) {
    ctx.gstate.init_graphics();
    let page_device = ctx.gstate.page_device;
    let default_ctm = ctx.gstate.default_ctm;

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

    if let Some(ref mut device) = ctx.device {
        device.init_clip();
    }
}

// ---------- device deactivation ----------

/// Deactivate the page device (PLRM 3e §6.2.6): call its `EndPage` with
/// reason code 2, and transmit the page if it returns `true` — an `EndPage`
/// that gathers several pages on one sheet flushes the last, partial sheet
/// this way. The standard procedures return `false`, discarding a page no
/// `showpage` ended.
///
/// Called by `setpagedevice` before it replaces the device, and at the end
/// of a job. The page is left in place either way; `setpagedevice` erases it,
/// and at the end of a job nothing follows. Returns whether the page was
/// transmitted.
pub fn deactivate_page_device(ctx: &mut Context) -> Result<bool, PsError> {
    if is_null_device(ctx) || !has_pd_key(ctx, b".IsPageDevice") {
        return Ok(false);
    }
    let Some(end_page) = end_page_proc(ctx) else {
        return Ok(false);
    };
    let depth = ctx.o_stack.len();
    ctx.o_stack.push(PsObject::int(ctx.showpage_count))?;
    ctx.o_stack.push(PsObject::int(2))?;
    ctx.exec_sync(end_page)?;
    if ctx.o_stack.len() <= depth {
        return Err(PsError::StackUnderflow);
    }
    let transmit = !matches!(ctx.o_stack.pop()?.value, PsValue::Bool(false));
    if transmit {
        transmit_page(ctx, "end of page device")?;
    }
    Ok(transmit)
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

/// Allocate a page device dictionary, or a copy of one, in local VM.
///
/// Local, whatever `currentglobal` says, for two reasons. A page device made
/// inside a `save` is then reclaimed by its `restore`, which reinstates the
/// graphics state, and with it the page device, from before the `save`;
/// stet has no garbage collector, so one in global VM would stay for the
/// life of the process. And a local dict may hold the request's values
/// whichever VM they are in, where a global one could not hold local ones
/// (PLRM 3.7.2). `PageCount`, which must survive a `restore`, is kept in
/// [`Context::page_count`] rather than here.
fn alloc_page_device(ctx: &mut Context) -> EntityId {
    crate::vm_ops::alloc_dict_in(ctx, 50, b"pagedevice", false)
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

/// Resolve where the page about to be written should go, and count it.
///
/// Shared by `showpage` and `copypage`, which differ in everything except
/// this. On success the page is recorded as emitted, so the next call sees a
/// higher index; on failure the job is aborted.
fn resolve_page_output(ctx: &mut Context, page_number: i32) -> Result<String, PsError> {
    let emitted_index = ctx.pages_emitted + 1;
    // A device that accumulates every page into one file (PDF) is served by a
    // single name however many pages arrive; only a page-per-file device can
    // collide with itself, so the others always look like the first page.
    let effective_index = if ctx.device.as_ref().is_none_or(|d| d.writes_file_per_page()) {
        emitted_index
    } else {
        1
    };
    match generate_output_path(
        ctx.output_template.as_ref(),
        ctx.output_path.as_deref(),
        page_number,
        effective_index,
    ) {
        Ok(path) => {
            ctx.pages_emitted = emitted_index;
            Ok(path)
        }
        Err(e) => {
            // A no-token `--output` on a job that turned out to be
            // multi-page. Page 1 is already on disk and stays there; abort
            // rather than overwrite it.
            //
            // This is a mistake in the command line, not in the PostScript
            // program, so it leaves through `exit_code` + `Quit` rather than
            // raising `ioerror` — the program did nothing wrong, and an
            // operand-stack dump would only bury the message that says how to
            // fix the invocation.
            eprintln!("Error: {}", e);
            ctx.exit_code = Some(1);
            Err(PsError::Quit)
        }
    }
}

/// Generate the output file path for a page about to be written.
///
/// With an explicit `-o` / `--output` template the template decides: a `%d`
/// conversion expands to `page_number`, and a template without one names a
/// single file, which is an error once `emitted_index` passes 1.
///
/// Without a template this keeps the historical PostScript convention —
/// `{basename}-{pagenum:04d}.png`, applied even to a single-page job — because
/// the visual suites and every existing script depend on those names.
fn generate_output_path(
    template: Option<&OutputTemplate>,
    base_path: Option<&str>,
    page_number: i32,
    emitted_index: u32,
) -> Result<String, ExpandError> {
    if let Some(template) = template {
        return template.expand(page_number, emitted_index);
    }
    Ok(match base_path {
        Some(path) => {
            // Strip extension, add page number
            let base = if let Some(pos) = path.rfind('.') {
                &path[..pos]
            } else {
                path
            };
            format!("{}-{:04}.png", base, page_number)
        }
        None => format!("output-{:04}.png", page_number),
    })
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
        assert_eq!(
            generate_output_path(None, Some("test.png"), 1, 1).unwrap(),
            "test-0001.png"
        );
        assert_eq!(
            generate_output_path(None, Some("/tmp/foo.ps.png"), 3, 3).unwrap(),
            "/tmp/foo.ps-0003.png"
        );
        assert_eq!(
            generate_output_path(None, None, 1, 1).unwrap(),
            "output-0001.png"
        );
    }

    #[test]
    fn test_generate_output_path_with_template() {
        // An explicit template overrides the derived name entirely, and a
        // token expands even for a single-page job.
        let tok = OutputTemplate::parse("p-%03d.png").unwrap();
        assert_eq!(
            generate_output_path(Some(&tok), Some("ignored.png"), 1, 1).unwrap(),
            "p-001.png"
        );

        // No token: the first emitted page takes the literal path...
        let plain = OutputTemplate::parse("out.png").unwrap();
        assert_eq!(
            generate_output_path(Some(&plain), Some("ignored.png"), 1, 1).unwrap(),
            "out.png"
        );
        // ...and a second one is refused instead of overwriting it.
        assert!(generate_output_path(Some(&plain), None, 2, 2).is_err());
    }

    #[test]
    fn test_generate_output_path_template_ignores_filtered_pages() {
        // `--pages 5` renders one file; the logical number is 5 but it is the
        // first emitted page, so a no-token template is still valid.
        let plain = OutputTemplate::parse("out.png").unwrap();
        assert_eq!(
            generate_output_path(Some(&plain), None, 5, 1).unwrap(),
            "out.png"
        );
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
        let arr = ctx.arrays.allocate_from_at_level_zero(&items);
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
