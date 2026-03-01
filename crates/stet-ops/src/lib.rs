// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! PostScript operator implementations and registration.

pub mod array_ops;
pub mod cff_ops;
pub mod clip_ops;
pub mod color_ops;
pub mod composite_ops;
pub mod control_ops;
pub mod device_ops;
pub mod dict_ops;
pub mod file_ops;
pub mod filter_ops;
pub mod font_ops;
pub mod graphics_state_ops;
pub mod halftone_ops;
pub mod image_ops;
pub mod insideness_ops;
pub mod math_ops;
pub mod matrix_ops;
pub mod misc_ops;
pub mod paint_ops;
pub mod param_ops;
pub mod path_ops;
pub mod path_query_ops;
pub mod relational_ops;
pub mod resource_ops;
pub mod shading_ops;
pub mod show_ops;
pub mod stack_ops;
pub mod string_ops;
pub mod type_ops;
pub mod vm_ops;

use stet_core::context::{Context, OpEntry};
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{OpCode, PsObject, PsValue};

/// Register all Phase 1-6 operators into systemdict and the operator table.
pub fn build_system_dict(ctx: &mut Context) {
    let sd = ctx.systemdict;

    // --- Stack operators ---
    register(ctx, sd, "pop", stack_ops::op_pop);
    register(ctx, sd, "dup", stack_ops::op_dup);
    register(ctx, sd, "exch", stack_ops::op_exch);
    register(ctx, sd, "copy", op_copy_dispatch);
    register(ctx, sd, "index", stack_ops::op_index);
    register(ctx, sd, "roll", stack_ops::op_roll);
    register(ctx, sd, "clear", stack_ops::op_clear);
    register(ctx, sd, "count", stack_ops::op_count);
    register(ctx, sd, "mark", stack_ops::op_mark);
    register(ctx, sd, "cleartomark", stack_ops::op_cleartomark);
    register(ctx, sd, "counttomark", stack_ops::op_counttomark);

    // --- Math operators ---
    register(ctx, sd, "add", math_ops::op_add);
    register(ctx, sd, "sub", math_ops::op_sub);
    register(ctx, sd, "mul", math_ops::op_mul);
    register(ctx, sd, "div", math_ops::op_div);
    register(ctx, sd, "idiv", math_ops::op_idiv);
    register(ctx, sd, "mod", math_ops::op_mod);
    register(ctx, sd, "abs", math_ops::op_abs);
    register(ctx, sd, "neg", math_ops::op_neg);
    register(ctx, sd, "ceiling", math_ops::op_ceiling);
    register(ctx, sd, "floor", math_ops::op_floor);
    register(ctx, sd, "round", math_ops::op_round);
    register(ctx, sd, "truncate", math_ops::op_truncate);
    register(ctx, sd, "sqrt", math_ops::op_sqrt);
    register(ctx, sd, "exp", math_ops::op_exp);
    register(ctx, sd, "ln", math_ops::op_ln);
    register(ctx, sd, "log", math_ops::op_log);
    register(ctx, sd, "sin", math_ops::op_sin);
    register(ctx, sd, "cos", math_ops::op_cos);
    register(ctx, sd, "atan", math_ops::op_atan);
    register(ctx, sd, "rand", math_ops::op_rand);
    register(ctx, sd, "srand", math_ops::op_srand);
    register(ctx, sd, "rrand", math_ops::op_rrand);
    register(ctx, sd, "max", math_ops::op_max);
    register(ctx, sd, "min", math_ops::op_min);
    register(ctx, sd, "realtime", misc_ops::op_realtime);
    register(ctx, sd, "usertime", misc_ops::op_usertime);

    // --- Relational/Boolean/Bitwise operators ---
    register(ctx, sd, "eq", relational_ops::op_eq);
    register(ctx, sd, "ne", relational_ops::op_ne);
    register(ctx, sd, "gt", relational_ops::op_gt);
    register(ctx, sd, "ge", relational_ops::op_ge);
    register(ctx, sd, "lt", relational_ops::op_lt);
    register(ctx, sd, "le", relational_ops::op_le);
    register(ctx, sd, "and", relational_ops::op_and);
    register(ctx, sd, "or", relational_ops::op_or);
    register(ctx, sd, "xor", relational_ops::op_xor);
    register(ctx, sd, "not", relational_ops::op_not);
    register(ctx, sd, "bitshift", relational_ops::op_bitshift);

    // --- Type/Conversion operators ---
    register(ctx, sd, "type", type_ops::op_type);
    register(ctx, sd, "cvx", type_ops::op_cvx);
    register(ctx, sd, "cvlit", type_ops::op_cvlit);
    register(ctx, sd, "cvn", type_ops::op_cvn);
    register(ctx, sd, "cvs", type_ops::op_cvs);
    register(ctx, sd, "cvrs", type_ops::op_cvrs);
    register(ctx, sd, "cvi", type_ops::op_cvi);
    register(ctx, sd, "cvr", type_ops::op_cvr);
    register(ctx, sd, "xcheck", type_ops::op_xcheck);
    register(ctx, sd, "executeonly", type_ops::op_executeonly);
    register(ctx, sd, "noaccess", type_ops::op_noaccess);
    register(ctx, sd, "readonly", type_ops::op_readonly);
    register(ctx, sd, "rcheck", type_ops::op_rcheck);
    register(ctx, sd, "wcheck", type_ops::op_wcheck);

    // --- Dictionary operators ---
    register(ctx, sd, "dict", dict_ops::op_dict);
    register(ctx, sd, "begin", dict_ops::op_begin);
    register(ctx, sd, "end", dict_ops::op_end);
    register(ctx, sd, "def", dict_ops::op_def);
    register(ctx, sd, "load", dict_ops::op_load);
    register(ctx, sd, "store", dict_ops::op_store);
    register(ctx, sd, "known", dict_ops::op_known);
    register(ctx, sd, "where", dict_ops::op_where);
    register(ctx, sd, "maxlength", dict_ops::op_maxlength);
    register(ctx, sd, "currentdict", dict_ops::op_currentdict);
    register(ctx, sd, "countdictstack", dict_ops::op_countdictstack);
    register(ctx, sd, "dictstack", dict_ops::op_dictstack);
    register(ctx, sd, "undef", dict_ops::op_undef);
    register(ctx, sd, "cleardictstack", dict_ops::op_cleardictstack);
    register(ctx, sd, "dictname", dict_ops::op_dictname);

    // --- Control flow operators ---
    register(ctx, sd, "exec", control_ops::op_exec);
    register(ctx, sd, "if", control_ops::op_if);
    register(ctx, sd, "ifelse", control_ops::op_ifelse);
    register(ctx, sd, "for", control_ops::op_for);
    register(ctx, sd, "repeat", control_ops::op_repeat);
    register(ctx, sd, "loop", control_ops::op_loop);
    register(ctx, sd, "forall", control_ops::op_forall);
    register(ctx, sd, "exit", control_ops::op_exit);
    register(ctx, sd, "stop", control_ops::op_stop);
    register(ctx, sd, "stopped", control_ops::op_stopped);
    register(ctx, sd, "quit", control_ops::op_quit);
    register(ctx, sd, "countexecstack", control_ops::op_countexecstack);
    register(ctx, sd, "execstack", control_ops::op_execstack);

    // --- Composite operators ---
    register(ctx, sd, "length", composite_ops::op_length);
    register(ctx, sd, "get", composite_ops::op_get);
    register(ctx, sd, "put", composite_ops::op_put);
    register(ctx, sd, "getinterval", composite_ops::op_getinterval);
    register(ctx, sd, "putinterval", composite_ops::op_putinterval);

    // --- Array operators ---
    register(ctx, sd, "array", array_ops::op_array);
    register(ctx, sd, "aload", array_ops::op_aload);
    register(ctx, sd, "astore", array_ops::op_astore);
    register(ctx, sd, "reverse", array_ops::op_reverse);
    register(ctx, sd, "printarray", array_ops::op_printarray);
    register(ctx, sd, "]", array_ops::op_array_from_mark);
    register(ctx, sd, ">>", array_ops::op_dict_from_mark);

    // --- String operators ---
    register(ctx, sd, "string", string_ops::op_string);
    register(ctx, sd, "anchorsearch", string_ops::op_anchorsearch);
    register(ctx, sd, "search", string_ops::op_search);

    // --- File/Output operators ---
    register(ctx, sd, "print", file_ops::op_print);
    register(ctx, sd, "=", file_ops::op_equal_sign);
    register(ctx, sd, "==", file_ops::op_double_equal);
    register(ctx, sd, "flush", file_ops::op_flush);
    register(ctx, sd, "pstack", file_ops::op_pstack);
    register(ctx, sd, "file", file_ops::op_file);
    register(ctx, sd, "closefile", file_ops::op_closefile);
    register(ctx, sd, "read", file_ops::op_read);
    register(ctx, sd, "write", file_ops::op_write);
    register(ctx, sd, "readstring", file_ops::op_readstring);
    register(ctx, sd, "writestring", file_ops::op_writestring);
    register(ctx, sd, "readline", file_ops::op_readline);
    register(ctx, sd, "readhexstring", file_ops::op_readhexstring);
    register(ctx, sd, "writehexstring", file_ops::op_writehexstring);
    register(ctx, sd, "token", file_ops::op_token);
    register(ctx, sd, "bytesavailable", file_ops::op_bytesavailable);
    register(ctx, sd, "flushfile", file_ops::op_flushfile);
    register(ctx, sd, "currentfile", file_ops::op_currentfile);
    register(ctx, sd, "line", file_ops::op_line);
    register(ctx, sd, "fileposition", file_ops::op_fileposition);
    register(ctx, sd, "setfileposition", file_ops::op_setfileposition);
    register(ctx, sd, "status", file_ops::op_status);
    register(ctx, sd, "deletefile", file_ops::op_deletefile);
    register(ctx, sd, "renamefile", file_ops::op_renamefile);
    register(ctx, sd, "filenameforall", file_ops::op_filenameforall);

    // --- Filter / eexec operators ---
    register(ctx, sd, "filter", filter_ops::op_filter);
    register(ctx, sd, "eexec", file_ops::op_eexec);

    // --- Misc operators ---
    register(ctx, sd, "bind", misc_ops::op_bind);
    register(ctx, sd, "run", misc_ops::op_run);
    register(ctx, sd, "join", misc_ops::op_join);
    register(ctx, sd, ".error", misc_ops::op_dot_error);
    register(ctx, sd, "setpacking", misc_ops::op_setpacking);
    register(ctx, sd, "currentpacking", misc_ops::op_currentpacking);
    register(ctx, sd, "packedarray", misc_ops::op_packedarray);
    register(ctx, sd, "setoverprint", misc_ops::op_setoverprint);
    register(ctx, sd, "currentoverprint", misc_ops::op_currentoverprint);
    register(ctx, sd, "break", misc_ops::op_break);
    register(ctx, sd, "setcacheparams", misc_ops::op_setcacheparams);
    register(
        ctx,
        sd,
        "currentcacheparams",
        misc_ops::op_currentcacheparams,
    );
    register(ctx, sd, "copypage", device_ops::op_copypage);
    register(ctx, sd, "resetfile", misc_ops::op_resetfile);
    register(ctx, sd, "defineuserobject", misc_ops::op_defineuserobject);
    register(
        ctx,
        sd,
        "undefineuserobject",
        misc_ops::op_undefineuserobject,
    );
    register(ctx, sd, "execuserobject", misc_ops::op_execuserobject);
    register(ctx, sd, "setobjectformat", misc_ops::op_setobjectformat);
    register(
        ctx,
        sd,
        "currentobjectformat",
        misc_ops::op_currentobjectformat,
    );
    register(ctx, sd, "printobject", misc_ops::op_printobject);
    register(ctx, sd, "writeobject", misc_ops::op_writeobject);

    // --- Internal/stub operators ---
    register(ctx, sd, ".nextfid", misc_ops::op_nextfid);
    register(ctx, sd, ".loadsystemfont", misc_ops::op_loadsystemfont);
    register(ctx, sd, ".loadfont", font_ops::op_dot_loadfont);
    register(ctx, sd, ".cff_startdata", cff_ops::op_cff_startdata);
    register(
        ctx,
        sd,
        ".loadbinarysystemfont",
        misc_ops::op_loadbinarysystemfont,
    );
    register(
        ctx,
        sd,
        ".loadbinaryfontfile",
        misc_ops::op_loadbinaryfontfile,
    );
    register(ctx, sd, ".systemundef", misc_ops::op_systemundef);
    register(
        ctx,
        sd,
        ".setinteractivepaint",
        misc_ops::op_setinteractivepaint,
    );
    register(ctx, sd, "pauseexechistory", misc_ops::op_pauseexechistory);
    register(ctx, sd, "resumeexechistory", misc_ops::op_resumeexechistory);
    register(ctx, sd, "exechistorystack", misc_ops::op_exechistorystack);
    register(ctx, sd, "exitserver", misc_ops::op_exitserver);
    register(ctx, sd, "startjob", misc_ops::op_startjob);
    register(ctx, sd, "internaldict", misc_ops::op_internaldict);

    // --- VM operators ---
    register(ctx, sd, "save", vm_ops::op_save);
    register(ctx, sd, "restore", vm_ops::op_restore);
    register(ctx, sd, "vmstatus", vm_ops::op_vmstatus);
    register(ctx, sd, "setglobal", vm_ops::op_setglobal);
    register(ctx, sd, "currentglobal", vm_ops::op_currentglobal);
    register(ctx, sd, "gcheck", vm_ops::op_gcheck);
    register(ctx, sd, "vmreclaim", vm_ops::op_vmreclaim);

    // --- Error handling ---
    // handleerror is NOT registered as a native operator — it's defined in PS
    // by sysdict.ps. If registered here, `bind` in PS procs would resolve it
    // to the Rust version, making it impossible for tests to redefine.

    // --- Parameter operators ---
    register(ctx, sd, "setuserparams", param_ops::op_setuserparams);
    register(
        ctx,
        sd,
        "currentuserparams",
        param_ops::op_currentuserparams,
    );
    register(ctx, sd, "setsystemparams", param_ops::op_setsystemparams);
    register(
        ctx,
        sd,
        "currentsystemparams",
        param_ops::op_currentsystemparams,
    );
    register(ctx, sd, "setdevparams", param_ops::op_setdevparams);
    register(ctx, sd, "currentdevparams", param_ops::op_currentdevparams);

    // --- Resource operators ---
    register(ctx, sd, "findresource", resource_ops::op_findresource);
    register(ctx, sd, "defineresource", resource_ops::op_defineresource);
    register(
        ctx,
        sd,
        "undefineresource",
        resource_ops::op_undefineresource,
    );
    register(ctx, sd, "resourcestatus", resource_ops::op_resourcestatus);
    register(ctx, sd, "resourceforall", resource_ops::op_resourceforall);
    register(
        ctx,
        sd,
        "globalresourcedict",
        resource_ops::op_globalresourcedict,
    );
    register(
        ctx,
        sd,
        "localresourcedict",
        resource_ops::op_localresourcedict,
    );
    register(ctx, sd, "categoryimpdict", resource_ops::op_categoryimpdict);

    // --- Matrix operators ---
    register(ctx, sd, "matrix", matrix_ops::op_matrix);
    register(ctx, sd, "identmatrix", matrix_ops::op_identmatrix);
    register(ctx, sd, "currentmatrix", matrix_ops::op_currentmatrix);
    register(ctx, sd, "setmatrix", matrix_ops::op_setmatrix);
    register(ctx, sd, "defaultmatrix", matrix_ops::op_defaultmatrix);
    register(ctx, sd, "initmatrix", matrix_ops::op_initmatrix);
    register(ctx, sd, "translate", matrix_ops::op_translate);
    register(ctx, sd, "scale", matrix_ops::op_scale);
    register(ctx, sd, "rotate", matrix_ops::op_rotate);
    register(ctx, sd, "concat", matrix_ops::op_concat);
    register(ctx, sd, "concatmatrix", matrix_ops::op_concatmatrix);
    register(ctx, sd, "invertmatrix", matrix_ops::op_invertmatrix);
    register(ctx, sd, "transform", matrix_ops::op_transform);
    register(ctx, sd, "itransform", matrix_ops::op_itransform);
    register(ctx, sd, "dtransform", matrix_ops::op_dtransform);
    register(ctx, sd, "idtransform", matrix_ops::op_idtransform);

    // --- Path construction operators ---
    register(ctx, sd, "newpath", path_ops::op_newpath);
    register(ctx, sd, "currentpoint", path_ops::op_currentpoint);
    register(ctx, sd, "moveto", path_ops::op_moveto);
    register(ctx, sd, "rmoveto", path_ops::op_rmoveto);
    register(ctx, sd, "lineto", path_ops::op_lineto);
    register(ctx, sd, "rlineto", path_ops::op_rlineto);
    register(ctx, sd, "curveto", path_ops::op_curveto);
    register(ctx, sd, "rcurveto", path_ops::op_rcurveto);
    register(ctx, sd, "closepath", path_ops::op_closepath);
    register(ctx, sd, "arc", path_ops::op_arc);
    register(ctx, sd, "arcn", path_ops::op_arcn);
    register(ctx, sd, "arcto", path_ops::op_arcto);
    register(ctx, sd, "arct", path_ops::op_arct);

    // --- Color operators ---
    register(ctx, sd, "setgray", color_ops::op_setgray);
    register(ctx, sd, "currentgray", color_ops::op_currentgray);
    register(ctx, sd, "setrgbcolor", color_ops::op_setrgbcolor);
    register(ctx, sd, "currentrgbcolor", color_ops::op_currentrgbcolor);
    register(ctx, sd, "setcmykcolor", color_ops::op_setcmykcolor);
    register(ctx, sd, "currentcmykcolor", color_ops::op_currentcmykcolor);
    register(ctx, sd, "sethsbcolor", color_ops::op_sethsbcolor);
    register(ctx, sd, "currenthsbcolor", color_ops::op_currenthsbcolor);
    register(ctx, sd, "setcolorspace", color_ops::op_setcolorspace);
    register(
        ctx,
        sd,
        "currentcolorspace",
        color_ops::op_currentcolorspace,
    );
    register(ctx, sd, "setcolor", color_ops::op_setcolor);
    register(ctx, sd, "currentcolor", color_ops::op_currentcolor);

    // --- Graphics state operators ---
    register(ctx, sd, "gsave", graphics_state_ops::op_gsave);
    register(ctx, sd, "grestore", graphics_state_ops::op_grestore);
    register(ctx, sd, "grestoreall", graphics_state_ops::op_grestoreall);
    register(ctx, sd, "setlinewidth", graphics_state_ops::op_setlinewidth);
    register(
        ctx,
        sd,
        "currentlinewidth",
        graphics_state_ops::op_currentlinewidth,
    );
    register(ctx, sd, "setlinecap", graphics_state_ops::op_setlinecap);
    register(
        ctx,
        sd,
        "currentlinecap",
        graphics_state_ops::op_currentlinecap,
    );
    register(ctx, sd, "setlinejoin", graphics_state_ops::op_setlinejoin);
    register(
        ctx,
        sd,
        "currentlinejoin",
        graphics_state_ops::op_currentlinejoin,
    );
    register(
        ctx,
        sd,
        "setmiterlimit",
        graphics_state_ops::op_setmiterlimit,
    );
    register(
        ctx,
        sd,
        "currentmiterlimit",
        graphics_state_ops::op_currentmiterlimit,
    );
    register(ctx, sd, "setdash", graphics_state_ops::op_setdash);
    register(ctx, sd, "currentdash", graphics_state_ops::op_currentdash);
    register(ctx, sd, "setflat", graphics_state_ops::op_setflat);
    register(ctx, sd, "currentflat", graphics_state_ops::op_currentflat);
    register(
        ctx,
        sd,
        "setstrokeadjust",
        graphics_state_ops::op_setstrokeadjust,
    );
    register(
        ctx,
        sd,
        "currentstrokeadjust",
        graphics_state_ops::op_currentstrokeadjust,
    );
    register(ctx, sd, "initgraphics", graphics_state_ops::op_initgraphics);

    // --- Painting operators ---
    register(ctx, sd, "fill", paint_ops::op_fill);
    register(ctx, sd, "eofill", paint_ops::op_eofill);
    register(ctx, sd, "stroke", paint_ops::op_stroke);
    register(ctx, sd, "rectfill", paint_ops::op_rectfill);
    register(ctx, sd, "rectstroke", paint_ops::op_rectstroke);
    register(ctx, sd, "erasepage", paint_ops::op_erasepage);
    register(ctx, sd, "showpage", paint_ops::op_showpage);

    // --- Image operators ---
    register(ctx, sd, "image", image_ops::op_image);
    register(ctx, sd, "imagemask", image_ops::op_imagemask);
    register(ctx, sd, "colorimage", image_ops::op_colorimage);

    // --- Clipping operators ---
    register(ctx, sd, "clip", clip_ops::op_clip);
    register(ctx, sd, "eoclip", clip_ops::op_eoclip);
    register(ctx, sd, "clippath", clip_ops::op_clippath);
    register(ctx, sd, "initclip", clip_ops::op_initclip);
    register(ctx, sd, "rectclip", clip_ops::op_rectclip);
    register(ctx, sd, "clipsave", clip_ops::op_clipsave);
    register(ctx, sd, "cliprestore", clip_ops::op_cliprestore);

    // --- Path query operators ---
    register(ctx, sd, "pathbbox", path_query_ops::op_pathbbox);
    register(ctx, sd, "flattenpath", path_query_ops::op_flattenpath);
    register(ctx, sd, "reversepath", path_query_ops::op_reversepath);
    register(ctx, sd, "strokepath", path_query_ops::op_strokepath);
    register(ctx, sd, "pathforall", path_query_ops::op_pathforall);

    // --- Insideness testing operators ---
    register(ctx, sd, "infill", insideness_ops::op_infill);
    register(ctx, sd, "ineofill", insideness_ops::op_ineofill);
    register(ctx, sd, "instroke", insideness_ops::op_instroke);

    // --- Font dictionary operators ---
    register(ctx, sd, "definefont", font_ops::op_definefont);
    register(ctx, sd, "undefinefont", font_ops::op_undefinefont);
    register(ctx, sd, "findfont", font_ops::op_findfont);
    register(ctx, sd, "scalefont", font_ops::op_scalefont);
    register(ctx, sd, "makefont", font_ops::op_makefont);
    register(ctx, sd, "setfont", font_ops::op_setfont);
    register(ctx, sd, "currentfont", font_ops::op_currentfont);
    register(ctx, sd, "rootfont", font_ops::op_rootfont);
    register(ctx, sd, "selectfont", font_ops::op_selectfont);
    register(ctx, sd, "composefont", font_ops::op_composefont);

    // --- Text show operators ---
    register(ctx, sd, "show", show_ops::op_show);
    register(ctx, sd, "ashow", show_ops::op_ashow);
    register(ctx, sd, "widthshow", show_ops::op_widthshow);
    register(ctx, sd, "awidthshow", show_ops::op_awidthshow);
    register(ctx, sd, "kshow", show_ops::op_kshow);
    register(ctx, sd, "stringwidth", show_ops::op_stringwidth);
    register(ctx, sd, "charpath", show_ops::op_charpath);
    register(ctx, sd, "cshow", show_ops::op_cshow);
    register(ctx, sd, "xshow", show_ops::op_xshow);
    register(ctx, sd, "yshow", show_ops::op_yshow);
    register(ctx, sd, "xyshow", show_ops::op_xyshow);
    register(ctx, sd, "setcachedevice", show_ops::op_setcachedevice);
    register(ctx, sd, "setcachedevice2", show_ops::op_setcachedevice2);
    register(ctx, sd, "setcharwidth", show_ops::op_setcharwidth);
    register(ctx, sd, "glyphshow", show_ops::op_glyphshow);

    // --- Halftone/screen operators ---
    register(ctx, sd, "setscreen", halftone_ops::op_setscreen);
    register(ctx, sd, "currentscreen", halftone_ops::op_currentscreen);
    register(ctx, sd, "setcolorscreen", halftone_ops::op_setcolorscreen);
    register(
        ctx,
        sd,
        "currentcolorscreen",
        halftone_ops::op_currentcolorscreen,
    );
    register(ctx, sd, "sethalftone", halftone_ops::op_sethalftone);
    register(ctx, sd, "currenthalftone", halftone_ops::op_currenthalftone);

    // --- Transfer function operators ---
    register(ctx, sd, "settransfer", halftone_ops::op_settransfer);
    register(ctx, sd, "currenttransfer", halftone_ops::op_currenttransfer);
    register(
        ctx,
        sd,
        "setcolortransfer",
        halftone_ops::op_setcolortransfer,
    );
    register(
        ctx,
        sd,
        "currentcolortransfer",
        halftone_ops::op_currentcolortransfer,
    );
    register(
        ctx,
        sd,
        "setblackgeneration",
        halftone_ops::op_setblackgeneration,
    );
    register(
        ctx,
        sd,
        "currentblackgeneration",
        halftone_ops::op_currentblackgeneration,
    );
    register(
        ctx,
        sd,
        "setundercolorremoval",
        halftone_ops::op_setundercolorremoval,
    );
    register(
        ctx,
        sd,
        "currentundercolorremoval",
        halftone_ops::op_currentundercolorremoval,
    );

    // --- Pattern/shading operators ---
    register(ctx, sd, "shfill", shading_ops::op_shfill);
    register(ctx, sd, "makepattern", halftone_ops::op_makepattern);
    register(ctx, sd, "execform", halftone_ops::op_execform);

    // --- Page device operators ---
    register(ctx, sd, "setpagedevice", device_ops::op_setpagedevice);
    register(
        ctx,
        sd,
        "currentpagedevice",
        device_ops::op_currentpagedevice,
    );
    register(ctx, sd, "nulldevice", device_ops::op_nulldevice);
    register(
        ctx,
        sd,
        ".showpage_continue",
        device_ops::op_showpage_continue,
    );
    register(
        ctx,
        sd,
        ".copypage_continue",
        device_ops::op_copypage_continue,
    );

    // --- Page size no-ops ---
    register(ctx, sd, "letter", font_ops::op_letter);
    register(ctx, sd, "legal", font_ops::op_legal);
    register(ctx, sd, "a4", font_ops::op_a4);
    register(ctx, sd, "a3", font_ops::op_a3);
    register(ctx, sd, "b5", font_ops::op_b5);

    // --- Default error handlers in errordict ---
    setup_errordict(ctx);

    // --- Version string ---
    let version_entity = ctx.strings.allocate_from(b"0.1.0");
    let version_obj = PsObject::string(version_entity, 5);
    let version_name = ctx.names.intern(b"version");
    ctx.dicts.put(sd, DictKey::Name(version_name), version_obj);
}

/// Register a single operator: add to operator table and systemdict.
fn register(
    ctx: &mut Context,
    dict: stet_core::object::EntityId,
    name: &str,
    func: fn(&mut Context) -> Result<(), PsError>,
) {
    let name_id = ctx.names.intern(name.as_bytes());
    let opcode = OpCode(ctx.operators.len() as u16);
    ctx.operators.push(OpEntry {
        func,
        name: name_id,
    });
    let op_obj = PsObject::operator(opcode);
    ctx.dicts.put(dict, DictKey::Name(name_id), op_obj);
}

/// Set up default error handlers in errordict.
/// Each PLRM error gets a handler that calls `stop` (which triggers
/// `stopped` to catch it).
fn setup_errordict(ctx: &mut Context) {
    let ed = ctx.errordict;

    // List of all PLRM error names
    let error_names = [
        "VMerror",
        "dictfull",
        "dictstackoverflow",
        "dictstackunderflow",
        "execstackoverflow",
        "invalidaccess",
        "invalidexit",
        "invalidfileaccess",
        "invalidfont",
        "invalidrestore",
        "ioerror",
        "limitcheck",
        "nocurrentpoint",
        "rangecheck",
        "stackoverflow",
        "stackunderflow",
        "syntaxerror",
        "timeout",
        "typecheck",
        "undefined",
        "undefinedfilename",
        "undefinedresource",
        "undefinedresult",
        "unmatchedmark",
        "unregistered",
        "unsupported",
        "configurationerror",
    ];

    // For each error, create a procedure that calls `stop`
    // This is: { /handleerror cvx exec }
    // But simpler: just register a procedure that calls stop
    let stop_name = ctx.names.intern(b"stop");
    let stop_obj = PsObject::name_exec(stop_name);

    for name in &error_names {
        let name_id = ctx.names.intern(name.as_bytes());
        // Create a 1-element procedure containing `stop`
        let entity = ctx.arrays.allocate_from(&[stop_obj]);
        let proc_obj = PsObject::procedure(entity, 1);
        ctx.dicts.put(ed, DictKey::Name(name_id), proc_obj);
    }
}

/// Polymorphic `copy`: if top is Int → stack copy, otherwise → composite copy.
fn op_copy_dispatch(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let top = ctx.o_stack.peek(0)?;
    match top.value {
        PsValue::Int(_) => stack_ops::op_copy(ctx),
        PsValue::String { .. }
        | PsValue::Array { .. }
        | PsValue::PackedArray { .. }
        | PsValue::Dict(_) => composite_ops::op_copy_composite(ctx),
        _ => Err(PsError::TypeCheck),
    }
}
