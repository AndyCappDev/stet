// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Execution context: stacks, storage, operator table, and state.

use std::io::Write;

use crate::device::OutputDevice;
use crate::dict::DictKey;
use crate::display_list::DisplayList;
use crate::dual_array_store::DualArrayStore;
use crate::dual_dict_store::DualDictStore;
use crate::dual_string_store::DualStringStore;
use crate::error::PsError;
use crate::file_store::FileStore;
use crate::graphics_state::{GraphicsState, Matrix, PathSegment};
use crate::name::NameTable;
use crate::object::{EntityId, NameId, ObjFlags, PsObject, PsValue, SaveLevel};
use crate::save_stack::{SaveRecord, SaveStack, StoreType};
use crate::stack::Stack;

/// Operator table entry: function pointer + name.
pub struct OpEntry {
    pub func: fn(&mut Context) -> Result<(), PsError>,
    pub name: NameId,
}

/// Pre-interned `NameId`s for frequently-used names in hot paths.
pub struct NameCache {
    pub n_def: NameId,
    pub n_true: NameId,
    pub n_false: NameId,
    pub n_null: NameId,
    pub n_mark: NameId,
    // Font-related names
    pub n_font_name: NameId,
    pub n_font_type: NameId,
    pub n_font_matrix: NameId,
    pub n_font_bbox: NameId,
    pub n_encoding: NameId,
    pub n_char_strings: NameId,
    pub n_private: NameId,
    pub n_fid: NameId,
    pub n_paint_type: NameId,
    pub n_subrs: NameId,
    pub n_len_iv: NameId,
    pub n_notdef: NameId,
    pub n_metrics: NameId,
    pub n_font_directory: NameId,
    // Resource system names
    pub n_find_resource: NameId,
    pub n_define_resource: NameId,
    pub n_undef_resource: NameId,
    pub n_resource_status: NameId,
    pub n_resource_for_all: NameId,
    pub n_category: NameId,
    pub n_instance_type: NameId,
    pub n_resource_dir: NameId,
    pub n_resource_ext: NameId,
    // Type 3 font names
    pub n_build_char: NameId,
    pub n_build_glyph: NameId,
}

/// Loop state for `for`, `repeat`, `loop`, and `forall`.
pub struct LoopState {
    pub loop_type: LoopType,
    pub proc_entity: EntityId,
    pub proc_start: u32,
    pub proc_len: u32,

    // for/repeat state
    pub counter: f64,
    pub increment: f64,
    pub limit: f64,
    pub use_int: bool,

    // forall state
    pub source: PsObject,
    pub index: u32,
    /// Snapshot of dict keys for dict forall (avoids re-collecting every iteration).
    pub dict_keys: Option<Vec<DictKey>>,

    // pathforall state
    pub path_segments: Option<Vec<PathSegment>>,
    pub path_procs: Option<[PsObject; 4]>, // [move, line, curve, close]
    pub path_ictm: Option<Matrix>,
}

/// Type of loop iteration.
pub enum LoopType {
    For,
    Repeat,
    Loop,
    Forall,
    PathForall,
}

/// Function pointer type for synchronous procedure execution.
/// Set by the engine crate to enable inline PS procedure calls from operators.
pub type ExecSyncFn = fn(&mut Context, PsObject) -> Result<(), PsError>;

pub struct Context {
    // Stacks
    pub o_stack: Stack,
    pub e_stack: Stack,
    pub d_stack: Vec<EntityId>,

    // Storage
    pub strings: DualStringStore,
    pub arrays: DualArrayStore,
    pub dicts: DualDictStore,
    pub names: NameTable,
    pub files: FileStore,

    // Loop state storage (indexed by EntityId)
    pub loops: Vec<LoopState>,

    // Operator table
    pub operators: Vec<OpEntry>,

    // Well-known dict IDs
    pub systemdict: EntityId,
    pub globaldict: EntityId,
    pub userdict: EntityId,
    pub errordict: EntityId,
    pub dollar_error: EntityId,

    // State
    pub rand_state: u64,
    pub rand_seed: i32,
    /// Current source line number (1-based), updated during scanning.
    pub current_source_line: u32,
    /// Packing mode for array/procedure creation (setpacking/currentpacking).
    pub packing_mode: bool,

    // Pre-interned names
    pub name_cache: NameCache,

    // Output: writer for print/= operators (allows capture in tests)
    pub stdout: Box<dyn Write>,

    // VM save/restore
    pub save_stack: SaveStack,

    // VM allocation mode: true = global, false = local
    pub vm_alloc_mode: bool,

    /// Binary object format (0-4). Default 0.
    pub object_format: i32,

    // Error dispatch state
    pub current_operator: Option<NameId>,
    pub in_error_handler: bool,
    /// True during init script execution — relaxes access checks.
    pub initializing: bool,
    /// When true, PS programs can change HWResolution via setpagedevice.
    /// Set by WASM frontend; CLI leaves false to keep DPI under user control.
    pub allow_ps_resolution: bool,

    // Graphics state
    pub gstate: GraphicsState,
    pub gstate_stack: Vec<crate::graphics_state::GstateEntry>,
    /// Storage for gstate objects (PsValue::Gstate indexes into this).
    pub gstate_store: Vec<GraphicsState>,
    pub device: Option<Box<dyn OutputDevice>>,
    pub display_list: DisplayList,
    /// When `Some`, each showpage clones the display list here before consuming it.
    /// Used by the WASM frontend to retain display lists for viewport re-rendering.
    /// Each entry is (DisplayList, dpi) where dpi is from the pagedevice HWResolution.
    pub capture_display_lists: Option<Vec<(DisplayList, f64)>>,
    /// When `Some`, each showpage sends a clone of the display list through this channel.
    /// Used by the CLI viewer for incremental display list delivery.
    /// Tuple: (DisplayList, dpi, page_width, page_height).
    pub display_list_sender:
        Option<std::sync::mpsc::Sender<(DisplayList, f64, u32, u32)>>,
    pub page_width: u32,
    pub page_height: u32,
    pub output_path: Option<String>,
    /// Factory closure for creating raster devices (registered by CLI).
    #[allow(clippy::type_complexity)]
    pub device_factory: Option<Box<dyn Fn(u32, u32) -> Box<dyn OutputDevice>>>,

    // Font system
    pub font_directory: EntityId,
    pub font_resource_path: Option<String>,
    pub next_fid: i32,

    // Resource system
    pub global_resources: EntityId,
    pub local_resources: EntityId,
    pub category_registry: EntityId,
    pub resource_base_path: Option<String>,

    // Parameter system
    pub user_params: EntityId,
    pub system_params: EntityId,

    // Internal dict (lazily created for `internaldict` operator)
    pub internaldict: Option<EntityId>,

    // Synchronous procedure execution (set by engine crate)
    pub exec_sync_fn: Option<ExecSyncFn>,

    // Character width set by setcachedevice/setcharwidth during BuildChar execution
    pub char_width: Option<(f64, f64)>,

    // CID passed from cshow to nested show call for Type 0 composite fonts
    pub cshow_pending_cid: Option<i32>,

    // Timing
    pub start_time: Option<std::time::Instant>,

    // Name resolution cache: invalidated on begin/end/def
    pub dict_version: u64,
    /// Name resolution cache indexed by NameId. Each entry is (dict_version, resolved_object).
    /// Public for inline cache checks in the eval loop's hot path.
    pub name_resolve_cache: Vec<(u64, PsObject)>,
}

impl Context {
    /// Execute a PostScript procedure synchronously and return.
    /// Equivalent to PostForge's `exec_exec()`.
    pub fn exec_sync(&mut self, proc_obj: PsObject) -> Result<(), PsError> {
        let f = self.exec_sync_fn.expect("exec_sync not initialized");
        f(self, proc_obj)
    }

    /// Create a new context with empty stacks and stores.
    /// Call `build_system_dict` afterward to populate operators.
    pub fn new() -> Self {
        let mut names = NameTable::new();

        let name_cache = NameCache {
            n_def: names.intern(b"def"),
            n_true: names.intern(b"true"),
            n_false: names.intern(b"false"),
            n_null: names.intern(b"null"),
            n_mark: names.intern(b"mark"),
            n_font_name: names.intern(b"FontName"),
            n_font_type: names.intern(b"FontType"),
            n_font_matrix: names.intern(b"FontMatrix"),
            n_font_bbox: names.intern(b"FontBBox"),
            n_encoding: names.intern(b"Encoding"),
            n_char_strings: names.intern(b"CharStrings"),
            n_private: names.intern(b"Private"),
            n_fid: names.intern(b"FID"),
            n_paint_type: names.intern(b"PaintType"),
            n_subrs: names.intern(b"Subrs"),
            n_len_iv: names.intern(b"lenIV"),
            n_notdef: names.intern(b".notdef"),
            n_metrics: names.intern(b"Metrics"),
            n_font_directory: names.intern(b"FontDirectory"),
            // Resource system
            n_find_resource: names.intern(b"FindResource"),
            n_define_resource: names.intern(b"DefineResource"),
            n_undef_resource: names.intern(b"UndefineResource"),
            n_resource_status: names.intern(b"ResourceStatus"),
            n_resource_for_all: names.intern(b"ResourceForAll"),
            n_category: names.intern(b"Category"),
            n_instance_type: names.intern(b"InstanceType"),
            n_resource_dir: names.intern(b"ResourceDir"),
            n_resource_ext: names.intern(b"ResourceExtension"),
            n_build_char: names.intern(b"BuildChar"),
            n_build_glyph: names.intern(b"BuildGlyph"),
        };

        let mut strings = DualStringStore::new();
        let mut dicts = DualDictStore::new();

        // Only systemdict is pre-allocated in Rust — it's needed to register native
        // operators. All other well-known dicts (globaldict, userdict, errordict, $error,
        // FontDirectory) are created by the init scripts in sysdict.ps.
        let systemdict = dicts.allocate_with(400, b"systemdict", 0, true, 0);
        let globaldict = dicts.allocate_with(100, b"globaldict", 0, true, 0);
        let userdict = dicts.allocate(200, b"userdict");
        let errordict = dicts.allocate(50, b"errordict");
        let dollar_error = dicts.allocate(20, b"$error");
        let font_directory = dicts.allocate(50, b"FontDirectory");

        // Resource system dicts (global VM)
        let global_resources = dicts.allocate_with(20, b"GlobalResources", 0, true, 0);
        let local_resources = dicts.allocate(20, b"LocalResources");
        let category_registry = dicts.allocate_with(30, b"CategoryRegistry", 0, true, 0);

        // Parameter dicts — pre-populate user_params with recognized keys
        // (matching PostForge context_init.py). setuserparams only updates
        // existing keys; unknown keys are ignored per PLRM.
        let user_params = dicts.allocate(25, b"UserParams");
        for key_name in [
            "MaxDictStack",
            "MaxExecStack",
            "MaxOpStack",
            "MaxFontItem",
            "MaxFormItem",
            "MaxPatternItem",
            "MaxUPathItem",
            "MaxScreenItem",
            "MaxSuperScreen",
            "MinFontCompress",
            "MaxLocalVM",
            "VMReclaim",
            "VMThreshold",
        ] {
            dicts.put(
                user_params,
                DictKey::Name(names.intern(key_name.as_bytes())),
                PsObject::int(0),
            );
        }
        dicts.put(
            user_params,
            DictKey::Name(names.intern(b"JobName")),
            PsObject::string(strings.allocate_from(b""), 0),
        );
        dicts.put(
            user_params,
            DictKey::Name(names.intern(b"ExecutionHistory")),
            PsObject::bool(false),
        );
        dicts.put(
            user_params,
            DictKey::Name(names.intern(b"ExecutionHistorySize")),
            PsObject::int(20),
        );
        dicts.put(
            user_params,
            DictKey::Name(names.intern(b"IdiomRecognition")),
            PsObject::bool(true),
        );
        dicts.put(
            user_params,
            DictKey::Name(names.intern(b"AccurateScreens")),
            PsObject::bool(false),
        );
        dicts.put(
            user_params,
            DictKey::Name(names.intern(b"HalftoneMode")),
            PsObject::int(0),
        );

        let system_params = dicts.allocate(30, b"SystemParams");
        // Cache size limits (PLRM Table C.2 - system parameters)
        for (key, val) in [
            ("MaxFontCache", 67108864),
            ("MaxFormCache", 131072),
            ("MaxPatternCache", 131072),
            ("MaxUPathCache", 131072),
            ("MaxScreenStorage", 524288),
            ("MaxDisplayList", 2097152),
            ("MaxDisplayAndSourceList", 4194304),
            ("MaxSourceList", 2097152),
            ("MaxImageBuffer", 524288),
            ("MaxOutlineCache", 65536),
            ("MaxStoredScreenCache", 0),
            // Read-only current cache usage counters
            ("CurFontCache", 0),
            ("CurFormCache", 0),
            ("CurPatternCache", 0),
            ("CurUPathCache", 0),
            ("CurScreenStorage", 0),
            ("CurSourceList", 0),
            ("CurStoredScreenCache", 0),
            ("CurOutlineCache", 0),
            ("PageCount", 0),
            ("Revision", 1),
        ] {
            dicts.put(
                system_params,
                DictKey::Name(names.intern(key.as_bytes())),
                PsObject::int(val),
            );
        }
        let printer_str = strings.allocate_from(b"stet");
        dicts.put(
            system_params,
            DictKey::Name(names.intern(b"PrinterName")),
            PsObject::string(printer_str, 6),
        );
        let realfmt_str = strings.allocate_from(b"IEE");
        dicts.put(
            system_params,
            DictKey::Name(names.intern(b"RealFormat")),
            PsObject::string(realfmt_str, 3),
        );
        let pw_str = strings.allocate_from(b"0");
        dicts.put(
            system_params,
            DictKey::Name(names.intern(b"SystemParamsPassword")),
            PsObject::string(pw_str, 1),
        );
        let pw_str2 = strings.allocate_from(b"0");
        dicts.put(
            system_params,
            DictKey::Name(names.intern(b"StartJobPassword")),
            PsObject::string(pw_str2, 1),
        );

        // Put self-referencing entries
        let sd_obj = PsObject::dict(systemdict);
        dicts.put(
            systemdict,
            DictKey::Name(names.intern(b"systemdict")),
            sd_obj,
        );

        let ud_obj = PsObject::dict(userdict);
        dicts.put(systemdict, DictKey::Name(names.intern(b"userdict")), ud_obj);

        let gd_obj = PsObject::dict(globaldict);
        dicts.put(
            systemdict,
            DictKey::Name(names.intern(b"globaldict")),
            gd_obj,
        );

        let ed_obj = PsObject::dict(errordict);
        dicts.put(
            systemdict,
            DictKey::Name(names.intern(b"errordict")),
            ed_obj,
        );

        let de_obj = PsObject::dict(dollar_error);
        dicts.put(systemdict, DictKey::Name(names.intern(b"$error")), de_obj);

        let fd_obj = PsObject::dict(font_directory);
        dicts.put(
            systemdict,
            DictKey::Name(name_cache.n_font_directory),
            fd_obj,
        );

        // Register constants in systemdict
        dicts.put(
            systemdict,
            DictKey::Name(names.intern(b"true")),
            PsObject::bool(true),
        );
        dicts.put(
            systemdict,
            DictKey::Name(names.intern(b"false")),
            PsObject::bool(false),
        );
        dicts.put(
            systemdict,
            DictKey::Name(names.intern(b"null")),
            PsObject::null(),
        );

        // mark — literal mark object
        dicts.put(
            systemdict,
            DictKey::Name(names.intern(b"mark")),
            PsObject::mark(),
        );

        // [ is an alias for mark
        dicts.put(
            systemdict,
            DictKey::Name(names.intern(b"[")),
            PsObject::mark(),
        );

        // << is a dict mark (distinct from [ mark so ] doesn't match it)
        dicts.put(
            systemdict,
            DictKey::Name(names.intern(b"<<")),
            PsObject::dict_mark(),
        );

        // version and languagelevel
        dicts.put(
            systemdict,
            DictKey::Name(names.intern(b"languagelevel")),
            PsObject::int(3),
        );

        // Dictionary stack: systemdict, globaldict, userdict
        let d_stack = vec![systemdict, globaldict, userdict];

        Self {
            o_stack: Stack::new(500),
            e_stack: Stack::new(250),
            d_stack,
            strings,
            arrays: DualArrayStore::new(),
            dicts,
            names,
            files: FileStore::new(),
            loops: Vec::new(),
            operators: Vec::new(),
            systemdict,
            globaldict,
            userdict,
            errordict,
            dollar_error,
            rand_state: 0,
            rand_seed: 0,
            current_source_line: 1,
            packing_mode: false,
            name_cache,
            stdout: Box::new(std::io::stdout()),
            save_stack: SaveStack::new(),
            vm_alloc_mode: false,
            object_format: 0,
            current_operator: None,
            in_error_handler: false,
            initializing: true,
            allow_ps_resolution: false,
            gstate: GraphicsState::new(),
            gstate_stack: Vec::new(),
            gstate_store: Vec::new(),
            device: None,
            display_list: DisplayList::new(),
            capture_display_lists: None,
            display_list_sender: None,
            page_width: 612,
            page_height: 792,
            output_path: None,
            device_factory: None,
            font_directory,
            font_resource_path: None,
            next_fid: 0,
            global_resources,
            local_resources,
            category_registry,
            resource_base_path: None,
            user_params,
            system_params,
            internaldict: None,
            exec_sync_fn: None,
            char_width: None,
            cshow_pending_cid: None,
            #[cfg(not(target_arch = "wasm32"))]
            start_time: Some(std::time::Instant::now()),
            #[cfg(target_arch = "wasm32")]
            start_time: None,
            dict_version: 0,
            name_resolve_cache: Vec::new(),
        }
    }

    /// Create a context that captures stdout to a buffer (for testing).
    pub fn new_with_output(output: Box<dyn Write>) -> Self {
        let mut ctx = Self::new();
        ctx.stdout = output;
        ctx
    }

    // --- Dictionary stack operations ---

    /// Look up a name in the dictionary stack (top to bottom).
    #[inline]
    pub fn dict_load(&mut self, key: &DictKey) -> Option<PsObject> {
        // Fast path: check name resolution cache
        if let DictKey::Name(name_id) = key {
            let idx = name_id.0 as usize;
            if idx < self.name_resolve_cache.len() {
                let (ver, obj) = self.name_resolve_cache[idx];
                if ver == self.dict_version {
                    return Some(obj);
                }
            }
        }

        // Slow path: search dict stack
        for &dict_id in self.d_stack.iter().rev() {
            if let Some(val) = self.dicts.get(dict_id, key) {
                // Cache the result for Name keys
                if let DictKey::Name(name_id) = key {
                    let idx = name_id.0 as usize;
                    if idx >= self.name_resolve_cache.len() {
                        self.name_resolve_cache
                            .resize(idx + 64, (u64::MAX, PsObject::null()));
                    }
                    self.name_resolve_cache[idx] = (self.dict_version, val);
                }
                return Some(val);
            }
        }
        None
    }

    /// Invalidate the name resolution cache (call on begin/end/def).
    #[inline]
    pub fn invalidate_name_cache(&mut self) {
        self.dict_version = self.dict_version.wrapping_add(1);
    }

    /// Look up and return `(dict_entity, value)` pair.
    pub fn dict_where(&self, key: &DictKey) -> Option<(EntityId, PsObject)> {
        for &dict_id in self.d_stack.iter().rev() {
            if let Some(val) = self.dicts.get(dict_id, key) {
                return Some((dict_id, val));
            }
        }
        None
    }

    /// Store in current dict (top of d_stack).
    pub fn dict_def(&mut self, key: DictKey, value: PsObject) -> Result<(), PsError> {
        let current = *self.d_stack.last().ok_or(PsError::DictStackUnderflow)?;
        self.cow_check_dict(current);
        self.invalidate_name_cache();
        self.dicts.put(current, key, value);
        Ok(())
    }

    /// Store in first dict that contains key, or current dict if not found.
    pub fn dict_store(&mut self, key: DictKey, value: PsObject) -> Result<(), PsError> {
        self.invalidate_name_cache();
        for &dict_id in self.d_stack.iter().rev() {
            if self.dicts.known(dict_id, &key) {
                self.cow_check_dict(dict_id);
                self.dicts.put(dict_id, key, value);
                return Ok(());
            }
        }
        // Not found — store in current dict
        self.dict_def(key, value)
    }

    /// Convert a `PsObject` to a `DictKey`.
    pub fn make_dict_key(&mut self, obj: &PsObject) -> Result<DictKey, PsError> {
        match obj.value {
            PsValue::Name(id) => Ok(DictKey::Name(id)),
            PsValue::Int(v) => Ok(DictKey::Int(v)),
            PsValue::Real(v) => Ok(DictKey::Real(v.to_bits())),
            PsValue::Bool(v) => Ok(DictKey::Bool(v)),
            PsValue::String { entity, start, len } => {
                // Intern string as name — PostScript treats string and name
                // keys as equivalent in dict lookups (matching PostForge behavior)
                let bytes = self.strings.get(entity, start, len).to_vec();
                let name_id = self.names.intern(&bytes);
                Ok(DictKey::Name(name_id))
            }
            PsValue::Operator(op) => Ok(DictKey::Operator(op.0)),
            PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
                Ok(DictKey::Identity(entity.0, start, len))
            }
            PsValue::Dict(entity) => Ok(DictKey::Identity(entity.0, 0, 0)),
            PsValue::Null => Err(PsError::TypeCheck),
            _ => Err(PsError::TypeCheck),
        }
    }

    /// Allocate a new loop state, returning its EntityId.
    pub fn alloc_loop(&mut self, state: LoopState) -> EntityId {
        let id = EntityId(self.loops.len() as u32);
        self.loops.push(state);
        id
    }

    /// Get a loop state by EntityId.
    pub fn get_loop(&self, entity: EntityId) -> &LoopState {
        &self.loops[entity.0 as usize]
    }

    /// Get a mutable loop state by EntityId.
    pub fn get_loop_mut(&mut self, entity: EntityId) -> &mut LoopState {
        &mut self.loops[entity.0 as usize]
    }

    /// Take the display list, optionally capturing a clone for viewport re-rendering.
    ///
    /// This replaces `std::mem::take(&mut ctx.display_list)` at showpage/copypage
    /// call sites. When `capture_display_lists` is active, a clone is saved
    /// along with the current page DPI from the pagedevice HWResolution.
    pub fn take_display_list(&mut self) -> DisplayList {
        if self.capture_display_lists.is_some() {
            let dpi = self.current_page_dpi();
            if let Some(ref mut captures) = self.capture_display_lists {
                captures.push((self.display_list.clone(), dpi));
            }
        }
        if let Some(ref sender) = self.display_list_sender {
            let dpi = self.current_page_dpi();
            // Use the device's actual page size (device pixels), not
            // self.page_width/page_height which are point values.
            let (w, h) = self
                .device
                .as_ref()
                .map(|d| d.page_size())
                .unwrap_or((self.page_width, self.page_height));
            let _ = sender.send((self.display_list.clone(), dpi, w, h));
        }
        std::mem::take(&mut self.display_list)
    }

    /// Read the current page DPI from the pagedevice HWResolution, defaulting to 72.
    fn current_page_dpi(&self) -> f64 {
        use crate::dict::DictKey;
        if let Some(pd) = self.gstate.page_device {
            if let Some(name_id) = self.names.find(b"HWResolution") {
                if let Some(obj) = self.dicts.get(pd, &DictKey::Name(name_id)) {
                    if let PsValue::Array { entity, .. } = obj.value {
                        let first = self.arrays.get_element(entity, 0);
                        return match first.value {
                            PsValue::Real(r) => r,
                            PsValue::Int(i) => i as f64,
                            _ => 72.0,
                        };
                    }
                }
            }
        }
        72.0
    }

    // --- VM save/restore ---

    /// Perform a `save`: snapshot the current VM state.
    /// Returns a Save PsObject.
    pub fn vm_save(&mut self) -> PsObject {
        let d_depth = self.d_stack.len();
        let gstate_snapshot = self.gstate.clone();
        let gstate_stack_snapshot = self.gstate_stack.clone();
        let (_level, save_id) = self.save_stack.save(
            d_depth,
            self.packing_mode,
            self.vm_alloc_mode,
            self.object_format,
            gstate_snapshot,
            gstate_stack_snapshot,
        );

        // Implicit gsave: push current gstate marked as save-created (per PLRM).
        // grestoreall stops at this entry; grestore skips it.
        self.gstate_stack.push(crate::graphics_state::GstateEntry {
            state: self.gstate.clone(),
            saved_by_save: true,
        });

        PsObject {
            value: PsValue::Save(SaveLevel(save_id)),
            flags: crate::object::ObjFlags::literal(),
        }
    }

    /// Perform a `restore`: revert VM to the given save state.
    pub fn vm_restore(&mut self, save_id: u32) -> Result<(), PsError> {
        // Validate save_id
        if !self.save_stack.is_valid(save_id) {
            return Err(PsError::InvalidRestore);
        }

        // Per PLRM: "restore can reset VM to the state represented by any
        // save object that is still valid, not necessarily the one produced
        // by the most recent save."  Pop the target level AND all newer
        // levels, undoing COW records from newest to target.
        let levels = self
            .save_stack
            .restore_to(save_id)
            .ok_or(PsError::InvalidRestore)?;

        // Undo COW records from newest level to oldest (reverse order).
        // Each level's records are also processed in reverse.
        // After swapping offsets, reset save_level to 0 so future COW
        // checks at the same save level don't incorrectly skip the backup.
        for level in levels.iter().rev() {
            for record in level.records.iter().rev() {
                match record.store_type {
                    StoreType::String => {
                        self.strings.swap_offsets(record.src, record.copy);
                        self.strings.entity_meta_mut(record.src).save_level = 0;
                    }
                    StoreType::Array => {
                        self.arrays.swap_offsets(record.src, record.copy);
                        self.arrays.entity_meta_mut(record.src).save_level = 0;
                    }
                    StoreType::Dict => {
                        self.dicts.swap_offsets(record.src, record.copy);
                        self.dicts.entity_meta_mut(record.src).save_level = 0;
                    }
                }
            }
        }

        // Restore context parameters from the TARGET save level (first in vec)
        let target = &levels[0];
        self.packing_mode = target.packing_mode;
        self.vm_alloc_mode = target.vm_alloc_mode;
        self.object_format = target.object_format;

        // Restore graphics state from the target level
        self.gstate = target.gstate.clone();
        self.gstate_stack = target.gstate_stack.clone();

        // Restore d_stack depth from the target level
        self.d_stack.truncate(target.d_stack_depth);

        self.invalidate_name_cache();
        Ok(())
    }

    // --- COW check methods ---

    /// Check if a string entity needs COW before mutation.
    /// If yes, creates a backup copy and records it.
    pub fn cow_check_string(&mut self, entity: EntityId) {
        let current_level = self.save_stack.current_level();
        if current_level == 0 {
            return; // No save active
        }

        if entity.is_global() {
            return; // Global entities skip local COW
        }
        let meta = self.strings.entity_meta(entity);
        if meta.save_level >= current_level {
            return; // Already copied at this level
        }

        // Perform COW copy
        let copy_id = self.strings.cow_copy(entity);
        self.strings.entity_meta_mut(entity).save_level = current_level;

        self.save_stack.add_record(SaveRecord {
            src: entity,
            copy: copy_id,
            store_type: StoreType::String,
        });
    }

    /// Check if an array entity needs COW before mutation.
    pub fn cow_check_array(&mut self, entity: EntityId) {
        let current_level = self.save_stack.current_level();
        if current_level == 0 {
            return;
        }

        if entity.is_global() {
            return;
        }
        let meta = self.arrays.entity_meta(entity);
        if meta.save_level >= current_level {
            return;
        }

        let copy_id = self.arrays.cow_copy(entity);
        self.arrays.entity_meta_mut(entity).save_level = current_level;

        self.save_stack.add_record(SaveRecord {
            src: entity,
            copy: copy_id,
            store_type: StoreType::Array,
        });
    }

    /// Check if a dict entity needs COW before mutation.
    pub fn cow_check_dict(&mut self, entity: EntityId) {
        let current_level = self.save_stack.current_level();
        if current_level == 0 {
            return;
        }

        if entity.is_global() {
            return;
        }
        let meta = self.dicts.entity_meta(entity);
        if meta.save_level >= current_level {
            return;
        }

        let copy_id = self.dicts.cow_copy(entity);
        self.dicts.entity_meta_mut(entity).save_level = current_level;

        self.save_stack.add_record(SaveRecord {
            src: entity,
            copy: copy_id,
            store_type: StoreType::Dict,
        });
    }

    // --- Token conversion ---

    /// Convert a tokenizer token into a PsObject.
    pub fn token_to_object(&mut self, token: crate::tokenizer::Token) -> Result<PsObject, PsError> {
        use crate::tokenizer::Token;
        match token {
            Token::Int(v) => Ok(PsObject::int(v)),
            Token::Real(v) => Ok(PsObject::real(v)),
            Token::String(bytes) => {
                let save_level = self.save_stack.current_level();
                let global = self.vm_alloc_mode;
                let created = self.save_stack.last_save_id();
                let entity = self.strings.allocate_with(bytes.len(), save_level, global, created);
                self.strings
                    .get_mut(entity, 0, bytes.len() as u32)
                    .copy_from_slice(&bytes);
                let mut obj = PsObject::string(entity, bytes.len() as u32);
                if global {
                    obj.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, false, true, true);
                }
                Ok(obj)
            }
            Token::Name(bytes, is_exec) => {
                let id = self.names.intern(&bytes);
                if is_exec {
                    Ok(PsObject::name_exec(id))
                } else {
                    Ok(PsObject::name_lit(id))
                }
            }
            Token::LiteralName(bytes) => {
                let id = self.names.intern(&bytes);
                Ok(PsObject::name_lit(id))
            }
            Token::ImmediateName(bytes) => {
                let id = self.names.intern(&bytes);
                let key = DictKey::Name(id);
                self.dict_load(&key).ok_or(PsError::Undefined)
            }
            Token::ArrayBegin => {
                let id = self.names.intern(b"[");
                Ok(PsObject::name_exec(id))
            }
            Token::ArrayEnd => {
                let id = self.names.intern(b"]");
                Ok(PsObject::name_exec(id))
            }
            Token::DictBegin => {
                let id = self.names.intern(b"<<");
                Ok(PsObject::name_exec(id))
            }
            Token::DictEnd => {
                let id = self.names.intern(b">>");
                Ok(PsObject::name_exec(id))
            }
            Token::ProcBegin | Token::ProcEnd | Token::Eof => Err(PsError::SyntaxError),
        }
    }

    /// Reset local VM stores (for job boundary cleanup).
    /// Full implementation deferred until job server loop is built.
    pub fn reset_local_vm(&mut self) {
        self.strings.reset_local();
        self.arrays.reset_local();
        self.dicts.reset_local();
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_creation() {
        let ctx = Context::new();
        assert!(ctx.o_stack.is_empty());
        assert!(ctx.e_stack.is_empty());
        assert_eq!(ctx.d_stack.len(), 3); // systemdict, globaldict, userdict
    }

    #[test]
    fn test_dict_def_and_load() {
        let mut ctx = Context::new();
        let key = DictKey::Name(ctx.names.intern(b"foo"));
        ctx.dict_def(key.clone(), PsObject::int(42)).unwrap();

        let val = ctx.dict_load(&key).unwrap();
        assert_eq!(val.as_i32(), Some(42));
    }

    #[test]
    fn test_dict_where() {
        let mut ctx = Context::new();
        let key = DictKey::Name(ctx.names.intern(b"true"));
        let result = ctx.dict_where(&key);
        assert!(result.is_some());
        let (dict_id, val) = result.unwrap();
        assert_eq!(dict_id, ctx.systemdict);
        assert!(matches!(val.value, PsValue::Bool(true)));
    }

    #[test]
    fn test_dict_store_existing() {
        let mut ctx = Context::new();
        let key = DictKey::Name(ctx.names.intern(b"myvar"));

        // Define in userdict
        ctx.dict_def(key.clone(), PsObject::int(1)).unwrap();

        // Store should update the existing entry in userdict
        ctx.dict_store(key.clone(), PsObject::int(2)).unwrap();

        let val = ctx.dict_load(&key).unwrap();
        assert_eq!(val.as_i32(), Some(2));
    }

    #[test]
    fn test_save_restore_basic() {
        let mut ctx = Context::new();
        let key = DictKey::Name(ctx.names.intern(b"testvar"));

        // Define before save
        ctx.dict_def(key.clone(), PsObject::int(1)).unwrap();

        // Save
        let save_obj = ctx.vm_save();
        let save_id = match save_obj.value {
            PsValue::Save(SaveLevel(id)) => id,
            _ => panic!("Expected Save"),
        };

        // Modify after save
        ctx.dict_def(key.clone(), PsObject::int(2)).unwrap();
        assert_eq!(ctx.dict_load(&key).unwrap().as_i32(), Some(2));

        // Restore
        ctx.vm_restore(save_id).unwrap();
        assert_eq!(ctx.dict_load(&key).unwrap().as_i32(), Some(1));
    }

    #[test]
    fn test_save_restore_string() {
        let mut ctx = Context::new();

        let entity = ctx.strings.allocate_from(b"hello");

        // Save
        let save_obj = ctx.vm_save();
        let save_id = match save_obj.value {
            PsValue::Save(SaveLevel(id)) => id,
            _ => panic!("Expected Save"),
        };

        // Modify after save
        ctx.cow_check_string(entity);
        ctx.strings.put_byte(entity, 0, b'H');
        assert_eq!(ctx.strings.get(entity, 0, 5), b"Hello");

        // Restore
        ctx.vm_restore(save_id).unwrap();
        assert_eq!(ctx.strings.get(entity, 0, 5), b"hello");
    }

    #[test]
    fn test_save_restore_array() {
        let mut ctx = Context::new();

        let items = [PsObject::int(1), PsObject::int(2), PsObject::int(3)];
        let entity = ctx.arrays.allocate_from(&items);

        let save_obj = ctx.vm_save();
        let save_id = match save_obj.value {
            PsValue::Save(SaveLevel(id)) => id,
            _ => panic!("Expected Save"),
        };

        ctx.cow_check_array(entity);
        ctx.arrays.set_element(entity, 1, PsObject::int(99));
        assert_eq!(ctx.arrays.get_element(entity, 1).as_i32(), Some(99));

        ctx.vm_restore(save_id).unwrap();
        assert_eq!(ctx.arrays.get_element(entity, 1).as_i32(), Some(2));
    }

    #[test]
    fn test_invalid_restore() {
        let mut ctx = Context::new();
        // Restore without save
        assert_eq!(ctx.vm_restore(999), Err(PsError::InvalidRestore));
    }
}
