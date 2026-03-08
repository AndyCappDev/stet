// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! CFF font operators.
//!
//! Implements `.cff_startdata` — the internal operator that backs the FontSetInit
//! ProcSet's `StartData` procedure. Reads binary CFF data from the current file,
//! parses it with `cff_parser`, and registers each font via `definefont`.

use stet_core::cff_parser::{self, CffFont};
use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{ObjFlags, PsObject, PsValue};

/// `.cff_startdata`: byte_count → —
///
/// Read `byte_count` bytes of CFF binary data from the current file,
/// parse the CFF data, build PostScript font dictionaries for each
/// font found, and register them as Font resources.
///
/// Called via FontSetInit ProcSet's `StartData` procedure.
/// The PS wrapper does: `fontsetname byte_count StartData`
/// where `StartData` is `{ exch pop .cff_startdata }`.
pub fn op_cff_startdata(ctx: &mut Context) -> Result<(), PsError> {
    // Validate stack
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let byte_count = ctx.o_stack.peek(0)?.as_i32().ok_or(PsError::TypeCheck)? as usize;
    ctx.o_stack.pop()?;

    // Find topmost file on exec stack (same as currentfile)
    let file_entity = {
        let mut found = None;
        for i in 0..ctx.e_stack.len() {
            if let Ok(obj) = ctx.e_stack.peek(i)
                && let PsValue::File(e) = obj.value
            {
                found = Some(e);
                break;
            }
        }
        found.ok_or(PsError::InvalidFont)?
    };

    // Read exactly byte_count bytes from the file
    let mut cff_data = Vec::with_capacity(byte_count);
    let mut remaining = byte_count;
    while remaining > 0 {
        let chunk_size = remaining.min(65536);
        let mut buf = vec![0u8; chunk_size];
        let n = ctx
            .files
            .read_into(file_entity, &mut buf)
            .map_err(|_| PsError::InvalidFont)?;
        if n == 0 {
            break;
        }
        cff_data.extend_from_slice(&buf[..n]);
        remaining -= n;
    }

    // Parse CFF data
    let cff_fonts = cff_parser::parse_cff(&cff_data).map_err(|_| PsError::InvalidFont)?;

    // Build and register PostScript font dictionaries
    // CFF fonts go in global VM per PLRM
    let saved_vm_mode = ctx.vm_alloc_mode;
    ctx.vm_alloc_mode = true;

    // Store raw CFF binary as a PS string for PDF embedding
    let cff_str_entity = ctx.strings.allocate_from(&cff_data);
    let cff_data_len = cff_data.len() as u32;

    for cff_font in &cff_fonts {
        register_cff_font(ctx, cff_font, cff_str_entity, cff_data_len)?;
    }

    ctx.vm_alloc_mode = saved_vm_mode;
    Ok(())
}

/// Build a PostScript font dictionary from a parsed CffFont and register it.
fn register_cff_font(
    ctx: &mut Context,
    cff_font: &CffFont,
    cff_str_entity: stet_core::object::EntityId,
    cff_data_len: u32,
) -> Result<(), PsError> {
    // Create font dictionary
    let font_entity = ctx.dicts.allocate(16, b"CFF");

    // FontType = 2
    let ft_key = DictKey::Name(ctx.name_cache.n_font_type);
    ctx.dicts.put(font_entity, ft_key, PsObject::int(2));

    // FontName
    let font_name_id = ctx.names.intern(cff_font.name.as_bytes());
    let fn_key = DictKey::Name(ctx.names.intern(b"FontName"));
    ctx.dicts.put(
        font_entity,
        fn_key,
        PsObject {
            value: PsValue::Name(font_name_id),
            flags: ObjFlags::literal(),
        },
    );

    // FontMatrix
    let fm_key = DictKey::Name(ctx.name_cache.n_font_matrix);
    let fm_entity = ctx.arrays.allocate(6);
    for (i, &val) in cff_font.font_matrix.iter().enumerate() {
        ctx.arrays
            .set_element(fm_entity, i as u32, PsObject::real(val));
    }
    ctx.dicts.put(
        font_entity,
        fm_key,
        PsObject {
            value: PsValue::Array {
                entity: fm_entity,
                start: 0,
                len: 6,
            },
            flags: ObjFlags::literal_composite(),
        },
    );

    // FontBBox
    let bbox_key = DictKey::Name(ctx.names.intern(b"FontBBox"));
    let bbox_entity = ctx.arrays.allocate(4);
    for (i, &val) in cff_font.font_bbox.iter().enumerate() {
        ctx.arrays
            .set_element(bbox_entity, i as u32, PsObject::real(val));
    }
    ctx.dicts.put(
        font_entity,
        bbox_key,
        PsObject {
            value: PsValue::Array {
                entity: bbox_entity,
                start: 0,
                len: 4,
            },
            flags: ObjFlags::literal_composite(),
        },
    );

    // Encoding — 256-element array: code → Name
    let enc_key = DictKey::Name(ctx.name_cache.n_encoding);
    let enc_entity = ctx.arrays.allocate(256);
    for code in 0..256u32 {
        let gid = cff_font.encoding[code as usize] as usize;
        let glyph_name = if gid < cff_font.charset.len() {
            &cff_font.charset[gid]
        } else {
            ".notdef"
        };
        let name_id = ctx.names.intern(glyph_name.as_bytes());
        ctx.arrays.set_element(
            enc_entity,
            code,
            PsObject {
                value: PsValue::Name(name_id),
                flags: ObjFlags::literal(),
            },
        );
    }
    ctx.dicts.put(
        font_entity,
        enc_key,
        PsObject {
            value: PsValue::Array {
                entity: enc_entity,
                start: 0,
                len: 256,
            },
            flags: ObjFlags::literal_composite(),
        },
    );

    // CharStrings — dict mapping glyph name → String (raw bytes)
    // For CID fonts, also add int-keyed entries (DictKey::Int(gid)) for CID lookup
    let cs_key = DictKey::Name(ctx.name_cache.n_char_strings);
    let cs_entity = ctx.dicts.allocate(16, b"CFF");
    for (gid, cs_data) in cff_font.char_strings.iter().enumerate() {
        let str_entity = ctx.strings.allocate_from(cs_data);
        let cs_obj = PsObject {
            value: PsValue::String {
                entity: str_entity,
                start: 0,
                len: cs_data.len() as u32,
            },
            flags: ObjFlags::literal_composite(),
        };

        // Name-keyed entry (for glyphshow and non-CID lookup)
        if gid < cff_font.charset.len() {
            let glyph_name = &cff_font.charset[gid];
            let name_id = ctx.names.intern(glyph_name.as_bytes());
            ctx.dicts.put(cs_entity, DictKey::Name(name_id), cs_obj);
        }

        // Int-keyed entry for CID fonts (CID = GID for CFF CID fonts)
        if cff_font.is_cid {
            ctx.dicts.put(cs_entity, DictKey::Int(gid as i32), cs_obj);
        }
    }
    ctx.dicts.put(
        font_entity,
        cs_key,
        PsObject {
            value: PsValue::Dict(cs_entity),
            flags: ObjFlags::literal_composite(),
        },
    );

    // Private dictionary
    let priv_key = DictKey::Name(ctx.name_cache.n_private);
    let priv_entity = ctx.dicts.allocate(16, b"CFF");

    let dwx_key = DictKey::Name(ctx.names.intern(b"defaultWidthX"));
    ctx.dicts.put(
        priv_entity,
        dwx_key,
        PsObject::real(cff_font.default_width_x),
    );

    let nwx_key = DictKey::Name(ctx.names.intern(b"nominalWidthX"));
    ctx.dicts.put(
        priv_entity,
        nwx_key,
        PsObject::real(cff_font.nominal_width_x),
    );

    // Local Subrs as array of String
    if !cff_font.local_subrs.is_empty() {
        let subrs_key = DictKey::Name(ctx.name_cache.n_subrs);
        let subrs_len = cff_font.local_subrs.len() as u32;
        let subrs_entity = ctx.arrays.allocate(subrs_len as usize);
        for (i, subr_data) in cff_font.local_subrs.iter().enumerate() {
            let str_entity = ctx.strings.allocate_from(subr_data);
            ctx.arrays.set_element(
                subrs_entity,
                i as u32,
                PsObject {
                    value: PsValue::String {
                        entity: str_entity,
                        start: 0,
                        len: subr_data.len() as u32,
                    },
                    flags: ObjFlags::literal_composite(),
                },
            );
        }
        ctx.dicts.put(
            priv_entity,
            subrs_key,
            PsObject {
                value: PsValue::Array {
                    entity: subrs_entity,
                    start: 0,
                    len: subrs_len,
                },
                flags: ObjFlags::literal_composite(),
            },
        );
    }

    ctx.dicts.put(
        font_entity,
        priv_key,
        PsObject {
            value: PsValue::Dict(priv_entity),
            flags: ObjFlags::literal_composite(),
        },
    );

    // Global subroutines as array of String
    if !cff_font.global_subrs.is_empty() {
        let gs_key = DictKey::Name(ctx.names.intern(b"_cff_global_subrs"));
        let gs_len = cff_font.global_subrs.len() as u32;
        let gs_entity = ctx.arrays.allocate(gs_len as usize);
        for (i, subr_data) in cff_font.global_subrs.iter().enumerate() {
            let str_entity = ctx.strings.allocate_from(subr_data);
            ctx.arrays.set_element(
                gs_entity,
                i as u32,
                PsObject {
                    value: PsValue::String {
                        entity: str_entity,
                        start: 0,
                        len: subr_data.len() as u32,
                    },
                    flags: ObjFlags::literal_composite(),
                },
            );
        }
        ctx.dicts.put(
            font_entity,
            gs_key,
            PsObject {
                value: PsValue::Array {
                    entity: gs_entity,
                    start: 0,
                    len: gs_len,
                },
                flags: ObjFlags::literal_composite(),
            },
        );
    }

    // Store raw CFF binary for PDF embedding (keyed as _CFFData on font dict)
    let cff_data_key = DictKey::Name(ctx.names.intern(b"_CFFData"));
    ctx.dicts.put(
        font_entity,
        cff_data_key,
        PsObject {
            value: PsValue::String {
                entity: cff_str_entity,
                start: 0,
                len: cff_data_len,
            },
            flags: ObjFlags::literal_composite(),
        },
    );

    // CID-specific data
    if cff_font.is_cid {
        let cid_ft_key = DictKey::Name(ctx.names.intern(b"CIDFontType"));
        ctx.dicts.put(font_entity, cid_ft_key, PsObject::int(0));

        // CIDCount = number of charstrings
        let cid_count_key = DictKey::Name(ctx.names.intern(b"CIDCount"));
        ctx.dicts.put(
            font_entity,
            cid_count_key,
            PsObject::int(cff_font.char_strings.len() as i32),
        );

        // CIDSystemInfo dict from ROS (Registry-Ordering-Supplement)
        if let Some((ref registry, ref ordering, supplement)) = cff_font.ros {
            let csi_entity = ctx.dicts.allocate(4, b"CFF");
            let reg_str = ctx.strings.allocate_from(registry.as_bytes());
            ctx.dicts.put(
                csi_entity,
                DictKey::Name(ctx.names.intern(b"Registry")),
                PsObject::string(reg_str, registry.len() as u32),
            );
            let ord_str = ctx.strings.allocate_from(ordering.as_bytes());
            ctx.dicts.put(
                csi_entity,
                DictKey::Name(ctx.names.intern(b"Ordering")),
                PsObject::string(ord_str, ordering.len() as u32),
            );
            ctx.dicts.put(
                csi_entity,
                DictKey::Name(ctx.names.intern(b"Supplement")),
                PsObject::int(supplement),
            );
            ctx.dicts.put(
                font_entity,
                DictKey::Name(ctx.names.intern(b"CIDSystemInfo")),
                PsObject {
                    value: PsValue::Dict(csi_entity),
                    flags: ObjFlags::literal_composite(),
                },
            );
        }

        // FDArray
        if !cff_font.fd_array.is_empty() {
            let fda_key = DictKey::Name(ctx.names.intern(b"_cff_fd_array"));
            let fda_len = cff_font.fd_array.len() as u32;
            let fda_entity = ctx.arrays.allocate(fda_len as usize);
            for (i, fd_entry) in cff_font.fd_array.iter().enumerate() {
                let fd_dict = ctx.dicts.allocate(16, b"CFF");
                let fd_priv = ctx.dicts.allocate(16, b"CFF");

                ctx.dicts.put(
                    fd_priv,
                    DictKey::Name(ctx.names.intern(b"defaultWidthX")),
                    PsObject::real(fd_entry.default_width_x),
                );
                ctx.dicts.put(
                    fd_priv,
                    DictKey::Name(ctx.names.intern(b"nominalWidthX")),
                    PsObject::real(fd_entry.nominal_width_x),
                );

                if !fd_entry.local_subrs.is_empty() {
                    let fd_subrs_len = fd_entry.local_subrs.len() as u32;
                    let fd_subrs_entity = ctx.arrays.allocate(fd_subrs_len as usize);
                    for (j, subr_data) in fd_entry.local_subrs.iter().enumerate() {
                        let str_entity = ctx.strings.allocate_from(subr_data);
                        ctx.arrays.set_element(
                            fd_subrs_entity,
                            j as u32,
                            PsObject {
                                value: PsValue::String {
                                    entity: str_entity,
                                    start: 0,
                                    len: subr_data.len() as u32,
                                },
                                flags: ObjFlags::literal_composite(),
                            },
                        );
                    }
                    ctx.dicts.put(
                        fd_priv,
                        DictKey::Name(ctx.name_cache.n_subrs),
                        PsObject {
                            value: PsValue::Array {
                                entity: fd_subrs_entity,
                                start: 0,
                                len: fd_subrs_len,
                            },
                            flags: ObjFlags::literal_composite(),
                        },
                    );
                }

                ctx.dicts.put(
                    fd_dict,
                    DictKey::Name(ctx.name_cache.n_private),
                    PsObject {
                        value: PsValue::Dict(fd_priv),
                        flags: ObjFlags::literal_composite(),
                    },
                );

                ctx.arrays.set_element(
                    fda_entity,
                    i as u32,
                    PsObject {
                        value: PsValue::Dict(fd_dict),
                        flags: ObjFlags::literal_composite(),
                    },
                );
            }
            ctx.dicts.put(
                font_entity,
                fda_key,
                PsObject {
                    value: PsValue::Array {
                        entity: fda_entity,
                        start: 0,
                        len: fda_len,
                    },
                    flags: ObjFlags::literal_composite(),
                },
            );
        }

        // FDSelect
        if !cff_font.fd_select.is_empty() {
            let fds_key = DictKey::Name(ctx.names.intern(b"_cff_fd_select"));
            let fds_len = cff_font.fd_select.len() as u32;
            let fds_entity = ctx.arrays.allocate(fds_len as usize);
            for (i, &fd_idx) in cff_font.fd_select.iter().enumerate() {
                ctx.arrays
                    .set_element(fds_entity, i as u32, PsObject::int(fd_idx as i32));
            }
            ctx.dicts.put(
                font_entity,
                fds_key,
                PsObject {
                    value: PsValue::Array {
                        entity: fds_entity,
                        start: 0,
                        len: fds_len,
                    },
                    flags: ObjFlags::literal_composite(),
                },
            );
        }
    }

    // Register via definefont — push key and font dict on o_stack, call definefont
    let font_dict_obj = PsObject {
        value: PsValue::Dict(font_entity),
        flags: ObjFlags::literal_composite(),
    };
    let font_name_obj = PsObject {
        value: PsValue::Name(font_name_id),
        flags: ObjFlags::literal(),
    };

    ctx.o_stack.push(font_name_obj)?;
    ctx.o_stack.push(font_dict_obj)?;
    crate::font_ops::op_definefont(ctx)?;

    // Pop the font dict that definefont leaves on the stack
    let registered_font = if !ctx.o_stack.is_empty() {
        ctx.o_stack.pop()?
    } else {
        font_dict_obj
    };

    // For CID fonts, also register in the CIDFont resource category
    // so composefont can resolve them via /CIDFont findresource
    if cff_font.is_cid {
        let cidfont_cat_name = ctx.names.intern(b"CIDFont");
        let cat_key = DictKey::Name(cidfont_cat_name);
        // Get or create the CIDFont category dict in global resources
        let cat_dict = if let Some(cat_obj) = ctx.dicts.get(ctx.global_resources, &cat_key) {
            match cat_obj.value {
                PsValue::Dict(e) => e,
                _ => {
                    let d = ctx.dicts.allocate(8, b"CIDFont");
                    ctx.dicts
                        .put(ctx.global_resources, cat_key, PsObject::dict(d));
                    d
                }
            }
        } else {
            let d = ctx.dicts.allocate(8, b"CIDFont");
            ctx.dicts
                .put(ctx.global_resources, cat_key, PsObject::dict(d));
            d
        };
        ctx.dicts
            .put(cat_dict, DictKey::Name(font_name_id), registered_font);
    }

    Ok(())
}
