// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF interactive form fields (AcroForm).
//!
//! AcroForm declares interactive fields — text inputs, checkboxes,
//! radio buttons, choice lists, signatures — at the document level.
//! Each terminal field has a fully-qualified name (parent names joined
//! with `.`) and one or more *widget* annotations that give it a
//! visible presence on a page.
//!
//! [`FormCatalog`] is the top-level container; [`FormField`] is a
//! single field (terminal or non-terminal container) with type-tagged
//! data in [`FieldKind`].
//!
//! ## Cross-linking with annotations
//!
//! Each terminal field carries a list of widget object numbers
//! ([`FormField::widget_obj_nums`]). Consumers that want the
//! corresponding [`Annotation`] data look up matching annotations on
//! each page via [`PdfDocument::page_annotations`]; the annotation
//! whose source object number equals the widget's is the one to use.
//!
//! [`Annotation`]: crate::Annotation
//! [`PdfDocument::page_annotations`]: crate::PdfDocument::page_annotations

use std::collections::HashSet;

use crate::diagnostics::{LocationHint, ParsePhase, Severity, WarningSink};
use crate::metadata::pdf_string_to_rust_pub;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

/// Maximum form-field tree depth.
///
/// Real-world AcroForms rarely exceed depth 4. 32 is generous.
const MAX_FIELD_DEPTH: u32 = 32;

/// Maximum total fields parsed from a single AcroForm.
const MAX_FORM_FIELDS: usize = 100_000;

/// Top-level AcroForm dictionary.
#[derive(Debug, Clone, Default)]
pub struct FormCatalog {
    /// Field tree (top-level fields and their descendants).
    pub fields: Vec<FormField>,
    /// `/NeedAppearances` — viewer must regenerate appearance
    /// streams. Default `false`.
    pub need_appearances: bool,
    /// `/SigFlags` — bit 0 = signatures exist; bit 1 = append-only.
    pub sig_flags: SigFlags,
    /// `/CO` — array of fully-qualified field names defining
    /// calculation order for fields with calculation actions.
    pub calculation_order: Vec<String>,
    /// `/DA` — default appearance string used by text fields lacking
    /// their own.
    pub default_appearance: Option<String>,
    /// `/Q` — default quadding (0 = left, 1 = center, 2 = right).
    pub quadding: u8,
    /// Whether `/XFA` is present (XFA forms — deprecated in PDF 2.0
    /// but still seen in the wild). The XFA payload itself is not
    /// parsed; consumers can fetch it via the resolver if needed.
    pub has_xfa: bool,
}

/// `/SigFlags` bit field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SigFlags {
    /// Bit 0: at least one signature field exists in the document.
    pub signatures_exist: bool,
    /// Bit 1: incremental updates only — modifying the document
    /// outside of new signatures must be done via append.
    pub append_only: bool,
}

impl SigFlags {
    fn from_bits(bits: i64) -> Self {
        Self {
            signatures_exist: bits & 0x01 != 0,
            append_only: bits & 0x02 != 0,
        }
    }
}

/// One form field — terminal (has a value) or non-terminal (container).
#[derive(Debug, Clone)]
pub struct FormField {
    /// Fully-qualified field name: parent names joined with `.`.
    /// Empty for fields lacking `/T`.
    pub name: String,
    /// Partial field name `/T` — just this node's name segment.
    pub partial_name: String,
    /// `/TU` — alternate (tooltip) name.
    pub alternate_name: Option<String>,
    /// `/TM` — mapping name for export.
    pub mapping_name: Option<String>,
    /// Common field flags from `/Ff` bits 1, 2, 3.
    pub flags: FieldFlags,
    /// Field type and subtype-specific data. `Container` for
    /// non-terminal nodes that exist purely to namespace children.
    pub kind: FieldKind,
    /// Current value (`/V`).
    pub value: FieldValue,
    /// Default value (`/DV`).
    pub default_value: FieldValue,
    /// Object numbers of widget annotations attached to this field.
    /// Cross-link with [`PdfDocument::page_annotations`] to fetch the
    /// renderable widgets.
    ///
    /// [`PdfDocument::page_annotations`]: crate::PdfDocument::page_annotations
    pub widget_obj_nums: Vec<u32>,
    /// Child fields (for container nodes; empty for terminal fields
    /// in the common single-widget case).
    pub children: Vec<FormField>,
    /// `/AA` additional-actions presence — bookkeeping flag, not a
    /// parsed action set.
    pub has_additional_actions: bool,
}

/// `/Ff` flags shared by all field types.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FieldFlags {
    /// Bit 1 — `ReadOnly`.
    pub read_only: bool,
    /// Bit 2 — `Required` (must have a value when form is submitted).
    pub required: bool,
    /// Bit 3 — `NoExport` (do not include when exporting form data).
    pub no_export: bool,
}

impl FieldFlags {
    fn from_bits(bits: i64) -> Self {
        Self {
            read_only: bits & 0x0000_0001 != 0,
            required: bits & 0x0000_0002 != 0,
            no_export: bits & 0x0000_0004 != 0,
        }
    }
}

/// Field type plus subtype-specific data.
#[derive(Debug, Clone)]
pub enum FieldKind {
    /// `/FT /Btn`.
    Button(ButtonField),
    /// `/FT /Tx`.
    Text(TextField),
    /// `/FT /Ch`.
    Choice(ChoiceField),
    /// `/FT /Sig`.
    Signature(SignatureField),
    /// Non-terminal container — no `/FT`, exists to namespace children.
    Container,
    /// `/FT` is present but the value isn't one we recognise; raw
    /// name preserved.
    Other { ft: String },
}

/// `/FT /Btn` — pushbutton, checkbox, or radio button.
#[derive(Debug, Clone, Default)]
pub struct ButtonField {
    pub button_type: ButtonType,
    /// `/Opt` — for radio groups, the export value of each child
    /// widget in declaration order.
    pub options: Vec<String>,
    /// Bit 15 — `NoToggleToOff` (radio): one button must always be on.
    pub no_toggle_to_off: bool,
    /// Bit 16 — `Radio`: this is a radio group (else checkbox).
    pub is_radio: bool,
    /// Bit 17 — `Pushbutton`: action button, no value.
    pub is_pushbutton: bool,
    /// Bit 26 — `RadiosInUnison`: radios with same /V toggle together.
    pub radios_in_unison: bool,
}

/// Resolved button kind (pre-classified for caller convenience).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ButtonType {
    /// Default kind: not pushbutton, not radio.
    #[default]
    Checkbox,
    Radio,
    Pushbutton,
}

/// `/FT /Tx` — text input.
#[derive(Debug, Clone, Default)]
pub struct TextField {
    /// `/MaxLen` — maximum character count; `None` for no limit.
    pub max_length: Option<u32>,
    /// Bit 13 — `Multiline`.
    pub multiline: bool,
    /// Bit 14 — `Password`.
    pub password: bool,
    /// Bit 21 — `FileSelect` (input is a file path).
    pub file_select: bool,
    /// Bit 23 — `DoNotSpellCheck`.
    pub do_not_spell_check: bool,
    /// Bit 24 — `DoNotScroll`.
    pub do_not_scroll: bool,
    /// Bit 25 — `Comb` (fixed-width per-character).
    pub comb: bool,
    /// Bit 26 — `RichText`.
    pub rich_text: bool,
    /// `/DA` — default appearance string.
    pub default_appearance: Option<String>,
    /// `/Q` — quadding override.
    pub quadding: Option<u8>,
    /// `/RV` — rich-text value (XHTML/XFA fragment).
    pub rich_value: Option<String>,
}

/// `/FT /Ch` — list box or combo box.
#[derive(Debug, Clone, Default)]
pub struct ChoiceField {
    /// `/Opt` — choices.
    pub options: Vec<ChoiceOption>,
    /// `/TI` — top index for scrolling list boxes.
    pub top_index: u32,
    /// `/I` — currently-selected indices (for multi-select).
    pub selected_indices: Vec<u32>,
    /// Bit 18 — `Combo` (else list box).
    pub combo: bool,
    /// Bit 19 — `Edit` (combo with editable text field).
    pub edit: bool,
    /// Bit 20 — `Sort`.
    pub sort: bool,
    /// Bit 22 — `MultiSelect`.
    pub multi_select: bool,
    /// Bit 23 — `DoNotSpellCheck`.
    pub do_not_spell_check: bool,
    /// Bit 27 — `CommitOnSelChange`.
    pub commit_on_sel_change: bool,
}

/// One entry in `/Opt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    /// Internal export value (often equals `display` if the option is
    /// just a single string).
    pub export: String,
    /// User-visible label.
    pub display: String,
}

/// `/FT /Sig` — digital signature.
#[derive(Debug, Clone, Default)]
pub struct SignatureField {
    /// `/Lock` dict presence (locks fields after signing).
    pub has_lock: bool,
    /// `/SV` (seed value) dict presence.
    pub has_seed_value: bool,
}

/// Field value. PDF stores values type-erased; the form-walker maps
/// each `/V` (or `/DV`) to one of these variants based on the field
/// type and the value's PDF type.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum FieldValue {
    /// `/V` is absent or `null`.
    #[default]
    None,
    /// Text-field value or rich-text fallback.
    Text(String),
    /// Checkbox / radio name (e.g. `/Yes`, `/Off`).
    Name(String),
    /// Multi-select list — array of strings.
    Array(Vec<String>),
    /// Boolean (rare; some signature seed-values use booleans).
    Bool(bool),
    /// Integer (rare).
    Integer(i64),
}

impl FieldValue {
    fn from_pdf_object(obj: &PdfObj) -> FieldValue {
        match obj {
            PdfObj::Null => FieldValue::None,
            PdfObj::Bool(b) => FieldValue::Bool(*b),
            PdfObj::Int(n) => FieldValue::Integer(*n),
            PdfObj::Real(r) => FieldValue::Integer(*r as i64),
            PdfObj::Name(n) => FieldValue::Name(String::from_utf8_lossy(n).into_owned()),
            PdfObj::Str(s) => FieldValue::Text(crate::metadata::decode_pdf_text_string_pub(s)),
            PdfObj::Array(arr) => {
                let strings: Vec<String> = arr
                    .iter()
                    .filter_map(|o| match o {
                        PdfObj::Str(s) => Some(crate::metadata::decode_pdf_text_string_pub(s)),
                        PdfObj::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
                        _ => None,
                    })
                    .collect();
                if strings.is_empty() {
                    FieldValue::None
                } else {
                    FieldValue::Array(strings)
                }
            }
            _ => FieldValue::None,
        }
    }
}

/// Parse the document's `/AcroForm` if present.
///
/// Returns `None` when the catalog has no `/AcroForm` entry. Always
/// returns a populated value otherwise; malformed sub-entries are
/// tolerated (defaulted) rather than fatal, and structural truncations
/// (cycle, depth-cap, field-cap) push [`ParseWarning`]s into `sink`.
///
/// [`ParseWarning`]: crate::ParseWarning
pub fn parse_acroform(resolver: &Resolver, sink: &WarningSink) -> Option<FormCatalog> {
    let catalog = catalog_dict(resolver)?;
    let acroform_obj = catalog.get(b"AcroForm")?;
    let acroform = resolver.deref(acroform_obj).ok()?;
    let dict = acroform.as_dict()?;

    let mut form = FormCatalog {
        need_appearances: dict
            .get(b"NeedAppearances")
            .and_then(as_bool)
            .unwrap_or(false),
        sig_flags: SigFlags::from_bits(dict.get_int(b"SigFlags").unwrap_or(0)),
        default_appearance: dict.get(b"DA").and_then(pdf_string_to_rust_pub),
        quadding: dict.get_int(b"Q").unwrap_or(0).clamp(0, 2) as u8,
        has_xfa: dict.get(b"XFA").is_some(),
        ..Default::default()
    };

    if let Some(co_arr) = dict.get_array(b"CO") {
        form.calculation_order = co_arr
            .iter()
            .filter_map(|o| match o {
                PdfObj::Str(s) => Some(crate::metadata::decode_pdf_text_string_pub(s)),
                _ => None,
            })
            .collect();
    }

    if let Some(fields_arr) = dict.get_array(b"Fields") {
        let mut visited = HashSet::new();
        let mut total_fields = 0usize;
        let mut roots = Vec::with_capacity(fields_arr.len());
        for field_obj in fields_arr {
            if let Some(field) = walk_field(
                resolver,
                field_obj,
                "",
                &FieldDefaults::from_form(&form),
                &mut visited,
                &mut total_fields,
                0,
                sink,
            ) {
                roots.push(field);
            }
        }
        form.fields = roots;
    }

    Some(form)
}

/// Inheritable defaults that flow from the AcroForm or a parent field
/// down to terminal children. Currently we don't expose these on
/// children that override them; the per-field structs carry their
/// own values when present, parent inheritance happens at parse time.
#[derive(Clone, Default)]
struct FieldDefaults {
    da: Option<String>,
    q: u8,
}

impl FieldDefaults {
    fn from_form(form: &FormCatalog) -> Self {
        Self {
            da: form.default_appearance.clone(),
            q: form.quadding,
        }
    }

    fn merge(&self, dict: &PdfDict) -> Self {
        Self {
            da: dict
                .get(b"DA")
                .and_then(pdf_string_to_rust_pub)
                .or_else(|| self.da.clone()),
            q: dict
                .get_int(b"Q")
                .map(|n| n.clamp(0, 2) as u8)
                .unwrap_or(self.q),
        }
    }
}

#[allow(clippy::too_many_arguments)] // recursive walker; context struct doesn't pay off
fn walk_field(
    resolver: &Resolver,
    field_obj: &PdfObj,
    parent_qualified_name: &str,
    parent_defaults: &FieldDefaults,
    visited: &mut HashSet<u32>,
    total_fields: &mut usize,
    depth: u32,
    sink: &WarningSink,
) -> Option<FormField> {
    if depth >= MAX_FIELD_DEPTH {
        sink.record(
            ParsePhase::Form,
            Some(LocationHint::FieldName(parent_qualified_name.to_string())),
            Severity::Error,
            format!(
                "form-field depth limit {MAX_FIELD_DEPTH} reached; \
                 deeper sub-fields dropped"
            ),
        );
        return None;
    }
    if *total_fields >= MAX_FORM_FIELDS {
        sink.record(
            ParsePhase::Form,
            None,
            Severity::Error,
            format!(
                "form-field count limit {MAX_FORM_FIELDS} reached; \
                 remaining fields dropped"
            ),
        );
        return None;
    }
    let obj_num = field_obj.as_ref().map(|(n, _)| n);
    if let Some(n) = obj_num
        && !visited.insert(n)
    {
        sink.record(
            ParsePhase::Form,
            Some(LocationHint::Object {
                obj_num: n,
                gen_num: 0,
            }),
            Severity::Warning,
            "form-field cycle detected; sub-tree truncated",
        );
        return None;
    }

    let resolved = resolver.deref(field_obj).ok()?;
    let dict = resolved.as_dict()?;
    *total_fields += 1;

    let partial_name = dict
        .get(b"T")
        .and_then(pdf_string_to_rust_pub)
        .unwrap_or_default();
    let qualified_name = if parent_qualified_name.is_empty() {
        partial_name.clone()
    } else if partial_name.is_empty() {
        parent_qualified_name.to_string()
    } else {
        format!("{parent_qualified_name}.{partial_name}")
    };

    let alternate_name = dict.get(b"TU").and_then(pdf_string_to_rust_pub);
    let mapping_name = dict.get(b"TM").and_then(pdf_string_to_rust_pub);
    let ff_bits = dict.get_int(b"Ff").unwrap_or(0);
    let flags = FieldFlags::from_bits(ff_bits);
    let has_additional_actions = dict.get(b"AA").is_some();

    let value = dict
        .get(b"V")
        .map(FieldValue::from_pdf_object)
        .unwrap_or_default();
    let default_value = dict
        .get(b"DV")
        .map(FieldValue::from_pdf_object)
        .unwrap_or_default();

    let merged_defaults = parent_defaults.merge(dict);

    let mut widget_obj_nums = Vec::new();
    let mut children = Vec::new();

    let ft = dict.get_name(b"FT").map(|n| n.to_vec());
    let kind = match ft.as_deref() {
        Some(b"Btn") => FieldKind::Button(parse_button(dict, ff_bits)),
        Some(b"Tx") => FieldKind::Text(parse_text(dict, ff_bits, &merged_defaults)),
        Some(b"Ch") => FieldKind::Choice(parse_choice(dict, ff_bits)),
        Some(b"Sig") => FieldKind::Signature(SignatureField {
            has_lock: dict.get(b"Lock").is_some(),
            has_seed_value: dict.get(b"SV").is_some(),
        }),
        Some(other) => FieldKind::Other {
            ft: String::from_utf8_lossy(other).into_owned(),
        },
        None => FieldKind::Container,
    };

    // Self-as-widget: if the field dict carries /Subtype /Widget, the
    // field itself is its sole widget.
    if dict.get_name(b"Subtype") == Some(b"Widget")
        && let Some(n) = obj_num
    {
        widget_obj_nums.push(n);
    }

    // Walk /Kids: each kid is either a child field (has /T or /FT) or
    // a widget annotation (just /Subtype /Widget) attached to this
    // terminal field.
    if let Some(kids_arr) = dict.get_array(b"Kids") {
        for kid_obj in kids_arr {
            let Ok(kid_resolved) = resolver.deref(kid_obj) else {
                continue;
            };
            let Some(kid_dict) = kid_resolved.as_dict() else {
                continue;
            };
            let kid_obj_num = kid_obj.as_ref().map(|(n, _)| n);
            let kid_is_widget = kid_dict.get_name(b"Subtype") == Some(b"Widget");
            let kid_has_field_keys = kid_dict.get(b"T").is_some() || kid_dict.get(b"FT").is_some();

            if kid_is_widget && !kid_has_field_keys {
                if let Some(n) = kid_obj_num {
                    widget_obj_nums.push(n);
                }
                continue;
            }

            // Recurse — kid is a sub-field (possibly itself a widget).
            if let Some(child) = walk_field(
                resolver,
                kid_obj,
                &qualified_name,
                &merged_defaults,
                visited,
                total_fields,
                depth + 1,
                sink,
            ) {
                children.push(child);
            }
        }
    }

    Some(FormField {
        name: qualified_name,
        partial_name,
        alternate_name,
        mapping_name,
        flags,
        kind,
        value,
        default_value,
        widget_obj_nums,
        children,
        has_additional_actions,
    })
}

fn parse_button(dict: &PdfDict, ff: i64) -> ButtonField {
    let no_toggle_to_off = ff & (1 << 14) != 0;
    let is_radio = ff & (1 << 15) != 0;
    let is_pushbutton = ff & (1 << 16) != 0;
    let radios_in_unison = ff & (1 << 25) != 0;

    let button_type = if is_pushbutton {
        ButtonType::Pushbutton
    } else if is_radio {
        ButtonType::Radio
    } else {
        ButtonType::Checkbox
    };

    let options = dict
        .get_array(b"Opt")
        .map(|arr| {
            arr.iter()
                .filter_map(|o| match o {
                    PdfObj::Str(s) => Some(crate::metadata::decode_pdf_text_string_pub(s)),
                    PdfObj::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    ButtonField {
        button_type,
        options,
        no_toggle_to_off,
        is_radio,
        is_pushbutton,
        radios_in_unison,
    }
}

fn parse_text(dict: &PdfDict, ff: i64, defaults: &FieldDefaults) -> TextField {
    TextField {
        max_length: dict.get_int(b"MaxLen").and_then(|n| u32::try_from(n).ok()),
        multiline: ff & (1 << 12) != 0,
        password: ff & (1 << 13) != 0,
        file_select: ff & (1 << 20) != 0,
        do_not_spell_check: ff & (1 << 22) != 0,
        do_not_scroll: ff & (1 << 23) != 0,
        comb: ff & (1 << 24) != 0,
        rich_text: ff & (1 << 25) != 0,
        default_appearance: dict
            .get(b"DA")
            .and_then(pdf_string_to_rust_pub)
            .or_else(|| defaults.da.clone()),
        quadding: dict.get_int(b"Q").map(|n| n.clamp(0, 2) as u8),
        rich_value: dict.get(b"RV").and_then(pdf_string_to_rust_pub),
    }
}

fn parse_choice(dict: &PdfDict, ff: i64) -> ChoiceField {
    let options = dict
        .get_array(b"Opt")
        .map(|arr| {
            arr.iter()
                .filter_map(|o| match o {
                    PdfObj::Str(s) => {
                        let v = crate::metadata::decode_pdf_text_string_pub(s);
                        Some(ChoiceOption {
                            export: v.clone(),
                            display: v,
                        })
                    }
                    PdfObj::Name(n) => {
                        let v = String::from_utf8_lossy(n).into_owned();
                        Some(ChoiceOption {
                            export: v.clone(),
                            display: v,
                        })
                    }
                    PdfObj::Array(pair) if pair.len() == 2 => {
                        let export = match &pair[0] {
                            PdfObj::Str(s) => crate::metadata::decode_pdf_text_string_pub(s),
                            PdfObj::Name(n) => String::from_utf8_lossy(n).into_owned(),
                            _ => return None,
                        };
                        let display = match &pair[1] {
                            PdfObj::Str(s) => crate::metadata::decode_pdf_text_string_pub(s),
                            PdfObj::Name(n) => String::from_utf8_lossy(n).into_owned(),
                            _ => return None,
                        };
                        Some(ChoiceOption { export, display })
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let selected_indices = dict
        .get_array(b"I")
        .map(|arr| {
            arr.iter()
                .filter_map(|o| o.as_int().and_then(|n| u32::try_from(n).ok()))
                .collect()
        })
        .unwrap_or_default();

    ChoiceField {
        options,
        top_index: dict
            .get_int(b"TI")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        selected_indices,
        combo: ff & (1 << 17) != 0,
        edit: ff & (1 << 18) != 0,
        sort: ff & (1 << 19) != 0,
        multi_select: ff & (1 << 21) != 0,
        do_not_spell_check: ff & (1 << 22) != 0,
        commit_on_sel_change: ff & (1 << 26) != 0,
    }
}

fn as_bool(obj: &PdfObj) -> Option<bool> {
    match obj {
        PdfObj::Bool(b) => Some(*b),
        _ => None,
    }
}

fn catalog_dict(resolver: &Resolver) -> Option<PdfDict> {
    if let Some((num, gen_num)) = resolver.trailer().get_ref(b"Root")
        && let Ok(obj) = resolver.resolve(num, gen_num)
        && let Some(dict) = obj.as_dict()
    {
        return Some(dict.clone());
    }
    crate::find_catalog(resolver).and_then(|obj| obj.as_dict().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_flags_decode() {
        let f = FieldFlags::from_bits(0x07);
        assert!(f.read_only && f.required && f.no_export);
        let f = FieldFlags::from_bits(0x02);
        assert!(!f.read_only && f.required && !f.no_export);
    }

    #[test]
    fn sig_flags_decode() {
        let s = SigFlags::from_bits(0x03);
        assert!(s.signatures_exist && s.append_only);
        let s = SigFlags::from_bits(0x01);
        assert!(s.signatures_exist && !s.append_only);
    }

    #[test]
    fn button_type_classification() {
        // Pushbutton bit dominates radio bit.
        let b = parse_button(&PdfDict::new(), (1 << 15) | (1 << 16));
        assert_eq!(b.button_type, ButtonType::Pushbutton);
        // Radio without pushbutton.
        let b = parse_button(&PdfDict::new(), 1 << 15);
        assert_eq!(b.button_type, ButtonType::Radio);
        // Neither = checkbox.
        let b = parse_button(&PdfDict::new(), 0);
        assert_eq!(b.button_type, ButtonType::Checkbox);
    }

    #[test]
    fn text_field_flags() {
        let t = parse_text(
            &PdfDict::new(),
            (1 << 12) | (1 << 13),
            &FieldDefaults::default(),
        );
        assert!(t.multiline && t.password);
        assert!(!t.comb);
    }

    #[test]
    fn choice_options_pair_form() {
        let mut dict = PdfDict::new();
        dict.insert(
            b"Opt".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Str(b"R".to_vec()),
                PdfObj::Array(vec![
                    PdfObj::Str(b"G".to_vec()),
                    PdfObj::Str(b"Green".to_vec()),
                ]),
                PdfObj::Str(b"B".to_vec()),
            ]),
        );
        let ch = parse_choice(&dict, 0);
        assert_eq!(ch.options.len(), 3);
        assert_eq!(ch.options[0].export, "R");
        assert_eq!(ch.options[0].display, "R");
        assert_eq!(ch.options[1].export, "G");
        assert_eq!(ch.options[1].display, "Green");
        assert_eq!(ch.options[2].export, "B");
    }

    #[test]
    fn field_value_from_pdf() {
        assert_eq!(FieldValue::from_pdf_object(&PdfObj::Null), FieldValue::None);
        assert_eq!(
            FieldValue::from_pdf_object(&PdfObj::Str(b"Scott".to_vec())),
            FieldValue::Text("Scott".to_string())
        );
        assert_eq!(
            FieldValue::from_pdf_object(&PdfObj::Name(b"Yes".to_vec())),
            FieldValue::Name("Yes".to_string())
        );
        assert_eq!(
            FieldValue::from_pdf_object(&PdfObj::Array(vec![
                PdfObj::Str(b"a".to_vec()),
                PdfObj::Str(b"b".to_vec()),
            ])),
            FieldValue::Array(vec!["a".to_string(), "b".to_string()])
        );
    }
}
