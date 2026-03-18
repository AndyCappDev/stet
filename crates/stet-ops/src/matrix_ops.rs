// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Matrix operators: matrix, identmatrix, currentmatrix, setmatrix, defaultmatrix,
//! initmatrix, translate, scale, rotate, concat, concatmatrix, invertmatrix,
//! transform, itransform, dtransform, idtransform.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{EntityId, PsObject, PsValue};
use stet_fonts::geometry::Matrix;

/// Read a 6-element Matrix from a PS array object on the stack.
fn read_matrix_from_array(
    ctx: &Context,
    entity: EntityId,
    start: u32,
    len: u32,
) -> Result<Matrix, PsError> {
    if len != 6 {
        return Err(PsError::RangeCheck);
    }
    let elems = ctx.arrays.get(entity, start, len);
    let mut vals = [0.0f64; 6];
    for (i, elem) in elems.iter().enumerate() {
        vals[i] = elem.as_f64().ok_or(PsError::TypeCheck)?;
    }
    Ok(Matrix::new(
        vals[0], vals[1], vals[2], vals[3], vals[4], vals[5],
    ))
}

/// Write a Matrix into a PS array.
fn write_matrix_to_array(ctx: &mut Context, entity: EntityId, start: u32, m: &Matrix) {
    let vals = m.to_array();
    for (i, &v) in vals.iter().enumerate() {
        ctx.arrays
            .set_element(entity, start + i as u32, PsObject::real(v));
    }
}

/// Extract array entity/start/len from a PsObject, returning TypeCheck if not an array.
fn extract_array(obj: &PsObject) -> Result<(EntityId, u32, u32), PsError> {
    match obj.value {
        PsValue::Array { entity, start, len } => Ok((entity, start, len)),
        _ => Err(PsError::TypeCheck),
    }
}

/// `matrix`: — → matrix (create identity matrix array)
pub fn op_matrix(ctx: &mut Context) -> Result<(), PsError> {
    let entity = crate::vm_ops::alloc_array(ctx, 6);
    let m = Matrix::identity();
    write_matrix_to_array(ctx, entity, 0, &m);
    let obj = crate::vm_ops::make_array_obj(ctx, entity, 6);
    ctx.o_stack.push(obj)?;
    Ok(())
}

/// `identmatrix`: matrix → matrix (fill array with identity)
pub fn op_identmatrix(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let arr = ctx.o_stack.peek(0)?;
    let (entity, start, len) = extract_array(&arr)?;
    arr.flags.require_write()?;
    if len != 6 {
        return Err(PsError::RangeCheck);
    }
    let m = Matrix::identity();
    write_matrix_to_array(ctx, entity, start, &m);
    // leave array on stack
    Ok(())
}

/// `currentmatrix`: matrix → matrix (copy CTM into array)
pub fn op_currentmatrix(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let arr = ctx.o_stack.peek(0)?;
    let (entity, start, len) = extract_array(&arr)?;
    arr.flags.require_write()?;
    if len != 6 {
        return Err(PsError::RangeCheck);
    }
    let m = ctx.gstate.ctm;
    write_matrix_to_array(ctx, entity, start, &m);
    Ok(())
}

/// `setmatrix`: matrix → — (replace CTM with matrix from array)
pub fn op_setmatrix(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let arr = ctx.o_stack.peek(0)?;
    let (entity, start, len) = extract_array(&arr)?;
    arr.flags.require_read()?;
    let m = read_matrix_from_array(ctx, entity, start, len)?;
    ctx.o_stack.pop()?;
    ctx.gstate.ctm = m;
    Ok(())
}

/// `defaultmatrix`: matrix → matrix (copy default CTM into array)
pub fn op_defaultmatrix(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let arr = ctx.o_stack.peek(0)?;
    let (entity, start, len) = extract_array(&arr)?;
    arr.flags.require_write()?;
    if len != 6 {
        return Err(PsError::RangeCheck);
    }
    let m = ctx.gstate.default_ctm;
    write_matrix_to_array(ctx, entity, start, &m);
    Ok(())
}

/// `initmatrix`: — → — (reset CTM to default)
///
/// If a page device is active, recomputes the CTM from PageSize and HWResolution.
/// For null devices, sets CTM to identity. Otherwise falls back to default_ctm.
pub fn op_initmatrix(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.gstate.page_device.is_some() {
        if crate::device_ops::is_null_device(ctx) {
            ctx.gstate.ctm = Matrix::identity();
            ctx.gstate.default_ctm = Matrix::identity();
            return Ok(());
        }
        if let Ok((_pw, ph)) = crate::device_ops::get_pd_f64_pair(ctx, b"PageSize")
            && let Ok((dpi_x, dpi_y)) = crate::device_ops::get_pd_f64_pair(ctx, b"HWResolution")
        {
            let scale_x = dpi_x / 72.0;
            let scale_y = dpi_y / 72.0;
            let media_h = (ph * scale_y).round() as u32;
            let ctm = Matrix::new(scale_x, 0.0, 0.0, -scale_y, 0.0, media_h as f64);
            ctx.gstate.ctm = ctm;
            ctx.gstate.default_ctm = ctm;
            return Ok(());
        }
    }
    ctx.gstate.ctm = ctx.gstate.default_ctm;
    Ok(())
}

/// `translate`: tx ty → — (modify CTM) OR tx ty matrix → matrix
pub fn op_translate(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    // Check if top is array (3-operand form)
    let top = ctx.o_stack.peek(0)?;
    if let PsValue::Array { entity, start, len } = top.value {
        // tx ty matrix → matrix
        if ctx.o_stack.len() < 3 {
            return Err(PsError::StackUnderflow);
        }
        top.flags.require_write()?;
        if len != 6 {
            return Err(PsError::RangeCheck);
        }
        let ty_obj = ctx.o_stack.peek(1)?;
        let tx_obj = ctx.o_stack.peek(2)?;
        let tx = tx_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let ty = ty_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let m = Matrix::translate(tx, ty);
        ctx.o_stack.pop()?; // matrix
        ctx.o_stack.pop()?; // ty
        ctx.o_stack.pop()?; // tx
        write_matrix_to_array(ctx, entity, start, &m);
        ctx.o_stack.push(top)?;
    } else {
        // tx ty → — (modify CTM)
        let ty_obj = ctx.o_stack.peek(0)?;
        let tx_obj = ctx.o_stack.peek(1)?;
        let tx = tx_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let ty = ty_obj.as_f64().ok_or(PsError::TypeCheck)?;
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        let t = Matrix::translate(tx, ty);
        ctx.gstate.ctm = ctx.gstate.ctm.concat(&t);
    }
    Ok(())
}

/// `scale`: sx sy → — (modify CTM) OR sx sy matrix → matrix
pub fn op_scale(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    let top = ctx.o_stack.peek(0)?;
    if let PsValue::Array { entity, start, len } = top.value {
        if ctx.o_stack.len() < 3 {
            return Err(PsError::StackUnderflow);
        }
        top.flags.require_write()?;
        if len != 6 {
            return Err(PsError::RangeCheck);
        }
        let sy_obj = ctx.o_stack.peek(1)?;
        let sx_obj = ctx.o_stack.peek(2)?;
        let sx = sx_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let sy = sy_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let m = Matrix::scale(sx, sy);
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        write_matrix_to_array(ctx, entity, start, &m);
        ctx.o_stack.push(top)?;
    } else {
        let sy_obj = ctx.o_stack.peek(0)?;
        let sx_obj = ctx.o_stack.peek(1)?;
        let sx = sx_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let sy = sy_obj.as_f64().ok_or(PsError::TypeCheck)?;
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        let s = Matrix::scale(sx, sy);
        ctx.gstate.ctm = ctx.gstate.ctm.concat(&s);
    }
    Ok(())
}

/// `rotate`: angle → — (modify CTM) OR angle matrix → matrix
pub fn op_rotate(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }

    let top = ctx.o_stack.peek(0)?;
    if let PsValue::Array { entity, start, len } = top.value {
        if ctx.o_stack.len() < 2 {
            return Err(PsError::StackUnderflow);
        }
        top.flags.require_write()?;
        if len != 6 {
            return Err(PsError::RangeCheck);
        }
        let angle_obj = ctx.o_stack.peek(1)?;
        let angle = angle_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let m = Matrix::rotate(angle);
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        write_matrix_to_array(ctx, entity, start, &m);
        ctx.o_stack.push(top)?;
    } else {
        let angle_obj = ctx.o_stack.peek(0)?;
        let angle = angle_obj.as_f64().ok_or(PsError::TypeCheck)?;
        ctx.o_stack.pop()?;
        let r = Matrix::rotate(angle);
        ctx.gstate.ctm = ctx.gstate.ctm.concat(&r);
    }
    Ok(())
}

/// `concat`: matrix → — (premultiply CTM)
pub fn op_concat(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let arr = ctx.o_stack.peek(0)?;
    let (entity, start, len) = extract_array(&arr)?;
    arr.flags.require_read()?;
    let m = read_matrix_from_array(ctx, entity, start, len)?;
    ctx.o_stack.pop()?;
    ctx.gstate.ctm = ctx.gstate.ctm.concat(&m);
    Ok(())
}

/// `concatmatrix`: matrix1 matrix2 matrix3 → matrix3 (matrix1 * matrix2 stored in matrix3)
pub fn op_concatmatrix(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let arr3 = ctx.o_stack.peek(0)?;
    let arr2 = ctx.o_stack.peek(1)?;
    let arr1 = ctx.o_stack.peek(2)?;

    let (e3, s3, l3) = extract_array(&arr3)?;
    let (e2, s2, l2) = extract_array(&arr2)?;
    let (e1, s1, l1) = extract_array(&arr1)?;
    arr1.flags.require_read()?;
    arr2.flags.require_read()?;
    arr3.flags.require_write()?;

    let m1 = read_matrix_from_array(ctx, e1, s1, l1)?;
    let m2 = read_matrix_from_array(ctx, e2, s2, l2)?;
    if l3 != 6 {
        return Err(PsError::RangeCheck);
    }

    let result = m2.multiply(&m1);
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    write_matrix_to_array(ctx, e3, s3, &result);
    ctx.o_stack.push(arr3)?;
    Ok(())
}

/// `invertmatrix`: matrix1 matrix2 → matrix2 (inverse of matrix1 stored in matrix2)
pub fn op_invertmatrix(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let arr2 = ctx.o_stack.peek(0)?;
    let arr1 = ctx.o_stack.peek(1)?;
    let (e2, s2, l2) = extract_array(&arr2)?;
    let (e1, s1, l1) = extract_array(&arr1)?;
    arr1.flags.require_read()?;
    arr2.flags.require_write()?;

    let m1 = read_matrix_from_array(ctx, e1, s1, l1)?;
    if l2 != 6 {
        return Err(PsError::RangeCheck);
    }
    let inv = m1.invert().ok_or(PsError::UndefinedResult)?;

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    write_matrix_to_array(ctx, e2, s2, &inv);
    ctx.o_stack.push(arr2)?;
    Ok(())
}

/// `transform`: x y → x' y' (by CTM) OR x y matrix → x' y'
pub fn op_transform(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    let top = ctx.o_stack.peek(0)?;
    if let PsValue::Array { entity, start, len } = top.value {
        // x y matrix → x' y'
        top.flags.require_read()?;
        if ctx.o_stack.len() < 3 {
            return Err(PsError::StackUnderflow);
        }
        let m = read_matrix_from_array(ctx, entity, start, len)?;
        let y_obj = ctx.o_stack.peek(1)?;
        let x_obj = ctx.o_stack.peek(2)?;
        let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let (x2, y2) = m.transform_point(x, y);
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.push(PsObject::real(x2))?;
        ctx.o_stack.push(PsObject::real(y2))?;
    } else {
        // x y → x' y' (using CTM)
        let y_obj = ctx.o_stack.peek(0)?;
        let x_obj = ctx.o_stack.peek(1)?;
        let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let (x2, y2) = ctx.gstate.ctm.transform_point(x, y);
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.push(PsObject::real(x2))?;
        ctx.o_stack.push(PsObject::real(y2))?;
    }
    Ok(())
}

/// `itransform`: x' y' → x y (inverse CTM) OR x' y' matrix → x y
pub fn op_itransform(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    let top = ctx.o_stack.peek(0)?;
    if let PsValue::Array { entity, start, len } = top.value {
        top.flags.require_read()?;
        if ctx.o_stack.len() < 3 {
            return Err(PsError::StackUnderflow);
        }
        let m = read_matrix_from_array(ctx, entity, start, len)?;
        let inv = m.invert().ok_or(PsError::UndefinedResult)?;
        let y_obj = ctx.o_stack.peek(1)?;
        let x_obj = ctx.o_stack.peek(2)?;
        let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let (x2, y2) = inv.transform_point(x, y);
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.push(PsObject::real(x2))?;
        ctx.o_stack.push(PsObject::real(y2))?;
    } else {
        let inv = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
        let y_obj = ctx.o_stack.peek(0)?;
        let x_obj = ctx.o_stack.peek(1)?;
        let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let (x2, y2) = inv.transform_point(x, y);
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.push(PsObject::real(x2))?;
        ctx.o_stack.push(PsObject::real(y2))?;
    }
    Ok(())
}

/// `dtransform`: dx dy → dx' dy' (distance by CTM) OR dx dy matrix → dx' dy'
pub fn op_dtransform(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    let top = ctx.o_stack.peek(0)?;
    if let PsValue::Array { entity, start, len } = top.value {
        top.flags.require_read()?;
        if ctx.o_stack.len() < 3 {
            return Err(PsError::StackUnderflow);
        }
        let m = read_matrix_from_array(ctx, entity, start, len)?;
        let dy_obj = ctx.o_stack.peek(1)?;
        let dx_obj = ctx.o_stack.peek(2)?;
        let dx = dx_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let dy = dy_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let (dx2, dy2) = m.transform_delta(dx, dy);
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.push(PsObject::real(dx2))?;
        ctx.o_stack.push(PsObject::real(dy2))?;
    } else {
        let dy_obj = ctx.o_stack.peek(0)?;
        let dx_obj = ctx.o_stack.peek(1)?;
        let dx = dx_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let dy = dy_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let (dx2, dy2) = ctx.gstate.ctm.transform_delta(dx, dy);
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.push(PsObject::real(dx2))?;
        ctx.o_stack.push(PsObject::real(dy2))?;
    }
    Ok(())
}

/// `idtransform`: dx' dy' → dx dy (inverse distance) OR dx' dy' matrix → dx dy
pub fn op_idtransform(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    let top = ctx.o_stack.peek(0)?;
    if let PsValue::Array { entity, start, len } = top.value {
        top.flags.require_read()?;
        if ctx.o_stack.len() < 3 {
            return Err(PsError::StackUnderflow);
        }
        let m = read_matrix_from_array(ctx, entity, start, len)?;
        let inv = m.invert().ok_or(PsError::UndefinedResult)?;
        let dy_obj = ctx.o_stack.peek(1)?;
        let dx_obj = ctx.o_stack.peek(2)?;
        let dx = dx_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let dy = dy_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let (dx2, dy2) = inv.transform_delta(dx, dy);
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.push(PsObject::real(dx2))?;
        ctx.o_stack.push(PsObject::real(dy2))?;
    } else {
        let inv = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
        let dy_obj = ctx.o_stack.peek(0)?;
        let dx_obj = ctx.o_stack.peek(1)?;
        let dx = dx_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let dy = dy_obj.as_f64().ok_or(PsError::TypeCheck)?;
        let (dx2, dy2) = inv.transform_delta(dx, dy);
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.push(PsObject::real(dx2))?;
        ctx.o_stack.push(PsObject::real(dy2))?;
    }
    Ok(())
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
    fn test_matrix_creates_identity() {
        let mut ctx = setup();
        op_matrix(&mut ctx).unwrap();
        let arr = ctx.o_stack.pop().unwrap();
        let (entity, start, len) = extract_array(&arr).unwrap();
        let m = read_matrix_from_array(&ctx, entity, start, len).unwrap();
        let vals = m.to_array();
        assert!((vals[0] - 1.0).abs() < 1e-10);
        assert!((vals[3] - 1.0).abs() < 1e-10);
        assert!((vals[4]).abs() < 1e-10);
    }

    #[test]
    fn test_identmatrix() {
        let mut ctx = setup();
        // Create a 6-element array with non-identity values
        let entity = ctx.arrays.allocate_from(&[
            PsObject::real(2.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(2.0),
            PsObject::real(10.0),
            PsObject::real(20.0),
        ]);
        let arr = PsObject::array(entity, 6);
        ctx.o_stack.push(arr).unwrap();
        op_identmatrix(&mut ctx).unwrap();
        let result = ctx.o_stack.peek(0).unwrap();
        let (e, s, l) = extract_array(&result).unwrap();
        let m = read_matrix_from_array(&ctx, e, s, l).unwrap();
        assert!((m.a - 1.0).abs() < 1e-10);
        assert!((m.tx).abs() < 1e-10);
    }

    #[test]
    fn test_currentmatrix_setmatrix() {
        let mut ctx = setup();
        // Set CTM to a non-identity matrix
        ctx.gstate.ctm = Matrix::new(2.0, 0.0, 0.0, 3.0, 10.0, 20.0);

        // currentmatrix
        let entity = ctx.arrays.allocate(6);
        let arr = PsObject::array(entity, 6);
        ctx.o_stack.push(arr).unwrap();
        op_currentmatrix(&mut ctx).unwrap();

        let result = ctx.o_stack.pop().unwrap();
        let (e, s, l) = extract_array(&result).unwrap();
        let m = read_matrix_from_array(&ctx, e, s, l).unwrap();
        assert!((m.a - 2.0).abs() < 1e-10);
        assert!((m.ty - 20.0).abs() < 1e-10);

        // Change CTM
        ctx.gstate.ctm = Matrix::identity();

        // setmatrix restores it
        ctx.o_stack.push(result).unwrap();
        op_setmatrix(&mut ctx).unwrap();
        assert!((ctx.gstate.ctm.a - 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_translate_ctm() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(20.0)).unwrap();
        op_translate(&mut ctx).unwrap();
        let (x, y) = ctx.gstate.ctm.transform_point(0.0, 0.0);
        assert!((x - 10.0).abs() < 1e-10);
        assert!((y - 20.0).abs() < 1e-10);
    }

    #[test]
    fn test_translate_matrix_form() {
        let mut ctx = setup();
        let entity = ctx.arrays.allocate(6);
        let arr = PsObject::array(entity, 6);
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(20.0)).unwrap();
        ctx.o_stack.push(arr).unwrap();
        op_translate(&mut ctx).unwrap();
        // CTM should be unchanged
        assert!((ctx.gstate.ctm.a - 1.0).abs() < 1e-10);
        assert!((ctx.gstate.ctm.tx).abs() < 1e-10);
        // Result matrix on stack
        let result = ctx.o_stack.pop().unwrap();
        let (e, s, l) = extract_array(&result).unwrap();
        let m = read_matrix_from_array(&ctx, e, s, l).unwrap();
        assert!((m.tx - 10.0).abs() < 1e-10);
        assert!((m.ty - 20.0).abs() < 1e-10);
    }

    #[test]
    fn test_scale_ctm() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(2.0)).unwrap();
        ctx.o_stack.push(PsObject::real(3.0)).unwrap();
        op_scale(&mut ctx).unwrap();
        let (x, y) = ctx.gstate.ctm.transform_point(5.0, 7.0);
        assert!((x - 10.0).abs() < 1e-10);
        assert!((y - 21.0).abs() < 1e-10);
    }

    #[test]
    fn test_rotate_ctm() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(90.0)).unwrap();
        op_rotate(&mut ctx).unwrap();
        let (x, y) = ctx.gstate.ctm.transform_point(1.0, 0.0);
        assert!(x.abs() < 1e-10);
        assert!((y - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_concat() {
        let mut ctx = setup();
        let entity = ctx.arrays.allocate_from(&[
            PsObject::real(2.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(2.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
        ]);
        let arr = PsObject::array(entity, 6);
        ctx.o_stack.push(arr).unwrap();
        op_concat(&mut ctx).unwrap();
        let (x, y) = ctx.gstate.ctm.transform_point(5.0, 3.0);
        assert!((x - 10.0).abs() < 1e-10);
        assert!((y - 6.0).abs() < 1e-10);
    }

    #[test]
    fn test_concatmatrix() {
        let mut ctx = setup();
        // m1 = translate(10, 0)
        let e1 = ctx.arrays.allocate_from(&[
            PsObject::real(1.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(1.0),
            PsObject::real(10.0),
            PsObject::real(0.0),
        ]);
        // m2 = scale(2, 2)
        let e2 = ctx.arrays.allocate_from(&[
            PsObject::real(2.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(2.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
        ]);
        let e3 = ctx.arrays.allocate(6);

        ctx.o_stack.push(PsObject::array(e1, 6)).unwrap();
        ctx.o_stack.push(PsObject::array(e2, 6)).unwrap();
        ctx.o_stack.push(PsObject::array(e3, 6)).unwrap();
        op_concatmatrix(&mut ctx).unwrap();

        let result = ctx.o_stack.pop().unwrap();
        let (e, s, l) = extract_array(&result).unwrap();
        let m = read_matrix_from_array(&ctx, e, s, l).unwrap();
        // result = m2 * m1 = scale(2,2) * translate(10,0)
        let (x, y) = m.transform_point(5.0, 3.0);
        assert!((x - 30.0).abs() < 1e-10); // 2*(5+10) = 30
        assert!((y - 6.0).abs() < 1e-10);
    }

    #[test]
    fn test_invertmatrix() {
        let mut ctx = setup();
        let e1 = ctx.arrays.allocate_from(&[
            PsObject::real(2.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(3.0),
            PsObject::real(10.0),
            PsObject::real(20.0),
        ]);
        let e2 = ctx.arrays.allocate(6);
        ctx.o_stack.push(PsObject::array(e1, 6)).unwrap();
        ctx.o_stack.push(PsObject::array(e2, 6)).unwrap();
        op_invertmatrix(&mut ctx).unwrap();

        let result = ctx.o_stack.pop().unwrap();
        let (e, s, l) = extract_array(&result).unwrap();
        let inv = read_matrix_from_array(&ctx, e, s, l).unwrap();
        assert!((inv.a - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_transform_ctm() {
        let mut ctx = setup();
        ctx.gstate.ctm = Matrix::translate(10.0, 20.0);
        ctx.o_stack.push(PsObject::real(5.0)).unwrap();
        ctx.o_stack.push(PsObject::real(3.0)).unwrap();
        op_transform(&mut ctx).unwrap();
        let y = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let x = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((x - 15.0).abs() < 1e-10);
        assert!((y - 23.0).abs() < 1e-10);
    }

    #[test]
    fn test_itransform_ctm() {
        let mut ctx = setup();
        ctx.gstate.ctm = Matrix::translate(10.0, 20.0);
        ctx.o_stack.push(PsObject::real(15.0)).unwrap();
        ctx.o_stack.push(PsObject::real(23.0)).unwrap();
        op_itransform(&mut ctx).unwrap();
        let y = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let x = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((x - 5.0).abs() < 1e-10);
        assert!((y - 3.0).abs() < 1e-10);
    }

    #[test]
    fn test_dtransform_ctm() {
        let mut ctx = setup();
        ctx.gstate.ctm = Matrix::scale(2.0, 3.0);
        ctx.o_stack.push(PsObject::real(5.0)).unwrap();
        ctx.o_stack.push(PsObject::real(7.0)).unwrap();
        op_dtransform(&mut ctx).unwrap();
        let dy = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let dx = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((dx - 10.0).abs() < 1e-10);
        assert!((dy - 21.0).abs() < 1e-10);
    }

    #[test]
    fn test_idtransform_ctm() {
        let mut ctx = setup();
        ctx.gstate.ctm = Matrix::scale(2.0, 3.0);
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(21.0)).unwrap();
        op_idtransform(&mut ctx).unwrap();
        let dy = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let dx = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((dx - 5.0).abs() < 1e-8);
        assert!((dy - 7.0).abs() < 1e-8);
    }

    #[test]
    fn test_defaultmatrix_initmatrix() {
        let mut ctx = setup();
        ctx.gstate.default_ctm = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 792.0);
        ctx.gstate.ctm = Matrix::scale(2.0, 2.0); // modified

        op_initmatrix(&mut ctx).unwrap();
        assert!((ctx.gstate.ctm.d - (-1.0)).abs() < 1e-10);
        assert!((ctx.gstate.ctm.ty - 792.0).abs() < 1e-10);
    }

    #[test]
    fn test_invertmatrix_singular() {
        let mut ctx = setup();
        let e1 = ctx.arrays.allocate_from(&[
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
        ]);
        let e2 = ctx.arrays.allocate(6);
        ctx.o_stack.push(PsObject::array(e1, 6)).unwrap();
        ctx.o_stack.push(PsObject::array(e2, 6)).unwrap();
        assert_eq!(op_invertmatrix(&mut ctx), Err(PsError::UndefinedResult));
    }

    #[test]
    fn test_transform_matrix_form() {
        let mut ctx = setup();
        let entity = ctx.arrays.allocate_from(&[
            PsObject::real(2.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(3.0),
            PsObject::real(10.0),
            PsObject::real(20.0),
        ]);
        ctx.o_stack.push(PsObject::real(5.0)).unwrap();
        ctx.o_stack.push(PsObject::real(7.0)).unwrap();
        ctx.o_stack.push(PsObject::array(entity, 6)).unwrap();
        op_transform(&mut ctx).unwrap();
        let y = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let x = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((x - 20.0).abs() < 1e-10); // 2*5+0*7+10
        assert!((y - 41.0).abs() < 1e-10); // 0*5+3*7+20
    }
}
