// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Userpath operators: setbbox, ucache, uappend, upath, ufill, ueofill,
//! ustroke, ustrokepath, inufill, inueofill, inustroke.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{EntityId, PsObject, PsValue};
use stet_fonts::geometry::{Matrix, PathSegment};

use crate::graphics_state_ops::{op_grestore, op_gsave};
use crate::insideness_ops::{op_ineofill, op_infill, op_instroke};
use crate::paint_ops::{op_eofill, op_fill, op_stroke};
use crate::path_ops::{
    op_arc, op_arcn, op_arct, op_closepath, op_curveto, op_lineto, op_moveto, op_newpath,
    op_rcurveto, op_rlineto, op_rmoveto,
};
use crate::path_query_ops::op_strokepath;

/// Number of operands consumed by each encoded userpath opcode.
const OPCODE_ARGS: [usize; 12] = [
    4, // 0: setbbox
    2, // 1: moveto
    2, // 2: rmoveto
    2, // 3: lineto
    2, // 4: rlineto
    6, // 5: curveto
    6, // 6: rcurveto
    5, // 7: arc
    5, // 8: arcn
    5, // 9: arct
    0, // 10: closepath
    0, // 11: ucache
];

/// Op function type alias for dispatch table.
type OpFn = fn(&mut Context) -> Result<(), PsError>;

/// Dispatch table for encoded userpath opcodes.
const OPCODE_FNS: [OpFn; 12] = [
    op_setbbox,   // 0
    op_moveto,    // 1
    op_rmoveto,   // 2
    op_lineto,    // 3
    op_rlineto,   // 4
    op_curveto,   // 5
    op_rcurveto,  // 6
    op_arc,       // 7
    op_arcn,      // 8
    op_arct,      // 9
    op_closepath, // 10
    op_ucache,    // 11
];

/// `setbbox`: llx lly urx ury → —
///
/// Store user-space bounding box on graphics state. Used by userpath operators
/// to provide pathbbox without computing from path segments.
pub fn op_setbbox(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }
    let ury_obj = ctx.o_stack.peek(0)?;
    let urx_obj = ctx.o_stack.peek(1)?;
    let lly_obj = ctx.o_stack.peek(2)?;
    let llx_obj = ctx.o_stack.peek(3)?;
    let llx = llx_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let lly = lly_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let urx = urx_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let ury = ury_obj.as_f64().ok_or(PsError::TypeCheck)?;
    if urx < llx || ury < lly {
        return Err(PsError::RangeCheck);
    }
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.bbox = Some([llx, lly, urx, ury]);
    Ok(())
}

/// `ucache`: — → — (no-op; caching hint for future optimization)
pub fn op_ucache(_ctx: &mut Context) -> Result<(), PsError> {
    Ok(())
}

/// Detect whether a userpath is ordinary (executable array/procedure) or
/// encoded (2-element literal array of [data_array opcode_string]).
fn is_encoded_userpath(ctx: &Context, obj: &PsObject) -> bool {
    match obj.value {
        PsValue::Array { entity, start, len } if !obj.flags.is_executable() && len == 2 => {
            let elems = ctx.arrays.get(entity, start, 2);
            elems[0].is_array_type() && matches!(elems[1].value, PsValue::String { .. })
        }
        _ => false,
    }
}

/// Execute an encoded userpath: [data_array opcode_string].
fn uappend_encoded(ctx: &mut Context, entity: EntityId, start: u32) -> Result<(), PsError> {
    let elems = ctx.arrays.get(entity, start, 2);
    let data_obj = elems[0];
    let ops_obj = elems[1];

    // Extract data array elements
    let (data_entity, data_start, data_len) = match data_obj.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            (entity, start, len)
        }
        _ => return Err(PsError::TypeCheck),
    };

    // Extract opcode string bytes
    let ops_bytes = match ops_obj.value {
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len).to_vec(),
        _ => return Err(PsError::TypeCheck),
    };

    // Read all data elements upfront
    let data_elems: Vec<PsObject> = ctx.arrays.get(data_entity, data_start, data_len).to_vec();
    let mut data_idx: usize = 0;

    // Process opcodes
    let mut i = 0;
    while i < ops_bytes.len() {
        let byte = ops_bytes[i];
        i += 1;

        if byte >= 32 {
            // Repeat prefix: repeat next opcode (byte - 32) times
            let repeat_count = (byte - 32) as usize;
            if i >= ops_bytes.len() {
                return Err(PsError::RangeCheck);
            }
            let opcode = ops_bytes[i] as usize;
            i += 1;
            if opcode >= OPCODE_ARGS.len() {
                return Err(PsError::RangeCheck);
            }
            for _ in 0..repeat_count {
                let n_args = OPCODE_ARGS[opcode];
                if data_idx + n_args > data_elems.len() {
                    return Err(PsError::RangeCheck);
                }
                for j in 0..n_args {
                    ctx.o_stack.push(data_elems[data_idx + j])?;
                }
                data_idx += n_args;
                OPCODE_FNS[opcode](ctx)?;
            }
        } else {
            let opcode = byte as usize;
            if opcode >= OPCODE_ARGS.len() {
                return Err(PsError::RangeCheck);
            }
            let n_args = OPCODE_ARGS[opcode];
            if data_idx + n_args > data_elems.len() {
                return Err(PsError::RangeCheck);
            }
            for j in 0..n_args {
                ctx.o_stack.push(data_elems[data_idx + j])?;
            }
            data_idx += n_args;
            OPCODE_FNS[opcode](ctx)?;
        }
    }

    Ok(())
}

/// Core uappend logic: execute a userpath (ordinary or encoded) against the current path.
fn uappend_userpath(ctx: &mut Context, userpath: PsObject) -> Result<(), PsError> {
    if is_encoded_userpath(ctx, &userpath) {
        // Encoded form: [data_array opcode_string]
        let (entity, start, _len) = match userpath.value {
            PsValue::Array { entity, start, len } => (entity, start, len),
            _ => return Err(PsError::TypeCheck),
        };
        uappend_encoded(ctx, entity, start)
    } else if userpath.is_array_type() && userpath.flags.is_executable() {
        // Ordinary form: executable array (procedure)
        ctx.exec_sync(userpath)
    } else {
        Err(PsError::TypeCheck)
    }
}

/// `uappend`: userpath → —
pub fn op_uappend(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let userpath = ctx.o_stack.peek(0)?;
    if !userpath.is_array_type() {
        return Err(PsError::TypeCheck);
    }
    ctx.o_stack.pop()?;
    uappend_userpath(ctx, userpath)
}

/// `upath`: bool → userpath
///
/// Build an executable array from the current path. If bool is true, include
/// ucache as the first element. Coordinates are inverse-transformed from
/// device space back to user space.
pub fn op_upath(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let flag_obj = ctx.o_stack.peek(0)?;
    let include_ucache = match flag_obj.value {
        PsValue::Bool(b) => b,
        _ => return Err(PsError::TypeCheck),
    };
    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }
    ctx.o_stack.pop()?;

    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let mut elems: Vec<PsObject> = Vec::new();

    if include_ucache {
        let ucache_name = ctx.names.intern(b"ucache");
        elems.push(PsObject::name_exec(ucache_name));
    }

    // Build setbbox from path bbox if path is non-empty
    if !ctx.gstate.path.is_empty() {
        let (llx, lly, urx, ury) = if let Some(bbox) = ctx.gstate.bbox {
            (bbox[0], bbox[1], bbox[2], bbox[3])
        } else {
            // Compute bbox from path in user space
            let mut min_x = f64::INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut max_y = f64::NEG_INFINITY;
            for seg in &ctx.gstate.path.segments {
                let points: Vec<(f64, f64)> = match seg {
                    PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => {
                        vec![ictm.transform_point(*x, *y)]
                    }
                    PathSegment::CurveTo {
                        x1,
                        y1,
                        x2,
                        y2,
                        x3,
                        y3,
                    } => {
                        vec![
                            ictm.transform_point(*x1, *y1),
                            ictm.transform_point(*x2, *y2),
                            ictm.transform_point(*x3, *y3),
                        ]
                    }
                    PathSegment::ClosePath => vec![],
                };
                for (px, py) in points {
                    min_x = min_x.min(px);
                    min_y = min_y.min(py);
                    max_x = max_x.max(px);
                    max_y = max_y.max(py);
                }
            }
            (min_x, min_y, max_x, max_y)
        };
        elems.push(PsObject::real(llx));
        elems.push(PsObject::real(lly));
        elems.push(PsObject::real(urx));
        elems.push(PsObject::real(ury));
        let setbbox_name = ctx.names.intern(b"setbbox");
        elems.push(PsObject::name_exec(setbbox_name));
    }

    // Emit path segments as operator calls
    let moveto_name = ctx.names.intern(b"moveto");
    let lineto_name = ctx.names.intern(b"lineto");
    let curveto_name = ctx.names.intern(b"curveto");
    let closepath_name = ctx.names.intern(b"closepath");

    for seg in &ctx.gstate.path.segments {
        match seg {
            PathSegment::MoveTo(dx, dy) => {
                let (ux, uy) = ictm.transform_point(*dx, *dy);
                elems.push(PsObject::real(ux));
                elems.push(PsObject::real(uy));
                elems.push(PsObject::name_exec(moveto_name));
            }
            PathSegment::LineTo(dx, dy) => {
                let (ux, uy) = ictm.transform_point(*dx, *dy);
                elems.push(PsObject::real(ux));
                elems.push(PsObject::real(uy));
                elems.push(PsObject::name_exec(lineto_name));
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                let (ux1, uy1) = ictm.transform_point(*x1, *y1);
                let (ux2, uy2) = ictm.transform_point(*x2, *y2);
                let (ux3, uy3) = ictm.transform_point(*x3, *y3);
                elems.push(PsObject::real(ux1));
                elems.push(PsObject::real(uy1));
                elems.push(PsObject::real(ux2));
                elems.push(PsObject::real(uy2));
                elems.push(PsObject::real(ux3));
                elems.push(PsObject::real(uy3));
                elems.push(PsObject::name_exec(curveto_name));
            }
            PathSegment::ClosePath => {
                elems.push(PsObject::name_exec(closepath_name));
            }
        }
    }

    let len = elems.len() as u32;
    let entity = ctx.arrays.allocate_from(&elems);
    ctx.o_stack.push(PsObject::procedure(entity, len))?;
    Ok(())
}

/// `ufill`: userpath → —
pub fn op_ufill(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let userpath = ctx.o_stack.peek(0)?;
    if !userpath.is_array_type() {
        return Err(PsError::TypeCheck);
    }
    ctx.o_stack.pop()?;
    op_gsave(ctx)?;
    let result = (|| {
        op_newpath(ctx)?;
        uappend_userpath(ctx, userpath)?;
        op_fill(ctx)
    })();
    op_grestore(ctx)?;
    result
}

/// `ueofill`: userpath → —
pub fn op_ueofill(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let userpath = ctx.o_stack.peek(0)?;
    if !userpath.is_array_type() {
        return Err(PsError::TypeCheck);
    }
    ctx.o_stack.pop()?;
    op_gsave(ctx)?;
    let result = (|| {
        op_newpath(ctx)?;
        uappend_userpath(ctx, userpath)?;
        op_eofill(ctx)
    })();
    op_grestore(ctx)?;
    result
}

/// Check if top of stack is a 6-element numeric array (potential matrix).
fn is_matrix_on_stack(ctx: &Context) -> bool {
    if ctx.o_stack.is_empty() {
        return false;
    }
    let top = match ctx.o_stack.peek(0) {
        Ok(obj) => obj,
        Err(_) => return false,
    };
    match top.value {
        PsValue::Array {
            entity,
            start,
            len: 6,
        } => {
            let elems = ctx.arrays.get(entity, start, 6);
            elems.iter().all(|e| e.as_f64().is_some())
        }
        _ => false,
    }
}

/// Read a matrix from the top of stack and apply it via concat.
fn pop_and_concat_matrix(ctx: &mut Context) -> Result<(), PsError> {
    let arr = ctx.o_stack.pop()?;
    let (entity, start, _len) = match arr.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let elems = ctx.arrays.get(entity, start, 6);
    let mut vals = [0.0f64; 6];
    for (i, elem) in elems.iter().enumerate() {
        vals[i] = elem.as_f64().ok_or(PsError::TypeCheck)?;
    }
    let m = Matrix::new(vals[0], vals[1], vals[2], vals[3], vals[4], vals[5]);
    ctx.gstate.ctm = ctx.gstate.ctm.concat(&m);
    Ok(())
}

/// `ustroke`: `userpath [matrix] → —`
pub fn op_ustroke(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    // Check for matrix form: matrix on top, userpath below
    let has_matrix = is_matrix_on_stack(ctx) && ctx.o_stack.len() >= 2 && {
        let below = ctx.o_stack.peek(1).ok();
        below.is_some_and(|o| o.is_array_type())
    };

    let matrix_obj = if has_matrix {
        Some(ctx.o_stack.pop()?)
    } else {
        None
    };

    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let userpath = ctx.o_stack.peek(0)?;
    if !userpath.is_array_type() {
        return Err(PsError::TypeCheck);
    }
    ctx.o_stack.pop()?;

    op_gsave(ctx)?;
    let result = (|| {
        op_newpath(ctx)?;
        uappend_userpath(ctx, userpath)?;
        if let Some(m) = matrix_obj {
            ctx.o_stack.push(m)?;
            pop_and_concat_matrix(ctx)?;
        }
        op_stroke(ctx)
    })();
    op_grestore(ctx)?;
    result
}

/// `ustrokepath`: `userpath [matrix] → —`
pub fn op_ustrokepath(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let has_matrix = is_matrix_on_stack(ctx) && ctx.o_stack.len() >= 2 && {
        let below = ctx.o_stack.peek(1).ok();
        below.is_some_and(|o| o.is_array_type())
    };

    let matrix_obj = if has_matrix {
        Some(ctx.o_stack.pop()?)
    } else {
        None
    };

    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let userpath = ctx.o_stack.peek(0)?;
    if !userpath.is_array_type() {
        return Err(PsError::TypeCheck);
    }
    ctx.o_stack.pop()?;

    op_newpath(ctx)?;
    uappend_userpath(ctx, userpath)?;
    if let Some(m) = matrix_obj {
        ctx.o_stack.push(m)?;
        pop_and_concat_matrix(ctx)?;
    }
    op_strokepath(ctx)
}

/// `inufill`: x y userpath → bool
pub fn op_inufill(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let userpath = ctx.o_stack.peek(0)?;
    let y_obj = ctx.o_stack.peek(1)?;
    let x_obj = ctx.o_stack.peek(2)?;
    if !userpath.is_array_type() {
        return Err(PsError::TypeCheck);
    }
    let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    op_gsave(ctx)?;
    let result = (|| {
        op_newpath(ctx)?;
        uappend_userpath(ctx, userpath)?;
        ctx.o_stack.push(PsObject::real(x))?;
        ctx.o_stack.push(PsObject::real(y))?;
        op_infill(ctx)
    })();
    op_grestore(ctx)?;
    result
}

/// `inueofill`: x y userpath → bool
pub fn op_inueofill(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let userpath = ctx.o_stack.peek(0)?;
    let y_obj = ctx.o_stack.peek(1)?;
    let x_obj = ctx.o_stack.peek(2)?;
    if !userpath.is_array_type() {
        return Err(PsError::TypeCheck);
    }
    let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    op_gsave(ctx)?;
    let result = (|| {
        op_newpath(ctx)?;
        uappend_userpath(ctx, userpath)?;
        ctx.o_stack.push(PsObject::real(x))?;
        ctx.o_stack.push(PsObject::real(y))?;
        op_ineofill(ctx)
    })();
    op_grestore(ctx)?;
    result
}

/// `inustroke`: `x y userpath [matrix] → bool`
pub fn op_inustroke(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    // Check for matrix form
    let has_matrix = is_matrix_on_stack(ctx) && ctx.o_stack.len() >= 4 && {
        let below = ctx.o_stack.peek(1).ok();
        below.is_some_and(|o| o.is_array_type())
    };

    let matrix_obj = if has_matrix {
        Some(ctx.o_stack.pop()?)
    } else {
        None
    };

    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let userpath = ctx.o_stack.peek(0)?;
    let y_obj = ctx.o_stack.peek(1)?;
    let x_obj = ctx.o_stack.peek(2)?;
    if !userpath.is_array_type() {
        return Err(PsError::TypeCheck);
    }
    let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    op_gsave(ctx)?;
    let result = (|| {
        op_newpath(ctx)?;
        uappend_userpath(ctx, userpath)?;
        if let Some(m) = matrix_obj {
            ctx.o_stack.push(m)?;
            pop_and_concat_matrix(ctx)?;
        }
        ctx.o_stack.push(PsObject::real(x))?;
        ctx.o_stack.push(PsObject::real(y))?;
        op_instroke(ctx)
    })();
    op_grestore(ctx)?;
    result
}
