// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Type 1 → Type 2 charstring conversion.
//!
//! Written from "Adobe Type 1 Font Format" (chapters 6 and 8) and Adobe
//! TN#5177, "The Type 2 Charstring Format".
//!
//! The Type 1 program is executed, not transliterated: subroutines are
//! inlined, `div` is evaluated, and the OtherSubrs with fixed meanings
//! (flex 0–2, hint replacement 3) are interpreted, so the output has no
//! subroutine calls and depends on nothing but itself. What the conversion
//! keeps:
//!
//! - **Outlines**, exactly. Type 1 coordinates are relative to the side
//!   bearing set by `hsbw`/`sbw`; Type 2 starts at the origin, so the side
//!   bearing is folded into the first moveto. Coordinates are tracked
//!   absolutely and each Type 2 delta is taken from the point the Type 2
//!   interpreter will actually be at, so fractional values from `div` do
//!   not drift.
//! - **Subpaths.** Type 1 `closepath` keeps the current point, and a line
//!   drawn after it starts a new subpath there; Type 2 has no `closepath`
//!   and ends a subpath only at a moveto, so one is emitted.
//! - **Hints.** Stems (`hstem`, `vstem`, `hstem3`, `vstem3`) are collected
//!   into hint groups — hint replacement through OtherSubr 3 starts a new
//!   group — then declared once, sorted, at the start of the charstring.
//!   With more than one group, `hintmask` selects each group where it took
//!   effect.
//! - **Flex** (OtherSubrs 0–2) becomes the Type 2 `flex` operator with the
//!   same flex height.
//! - **The advance width**, returned separately in [`Type2Glyph`] so the
//!   caller can choose `defaultWidthX` / `nominalWidthX` over all glyphs.
//!
//! What it drops: `dotsection` (superseded by hint replacement), the
//! vertical components of `sbw`, and the counter control `hstem3`/`vstem3`
//! imply (their three stems are kept as ordinary stems). `seac` is an
//! error: it builds accented characters from StandardEncoding codes, which
//! mean nothing in a CIDFont, and CIDFonts are what this conversion serves.

use crate::charstring::decrypt_charstring;
use crate::geometry::Matrix;

/// Deepest subroutine nesting allowed ("Adobe Type 1 Font Format" §6.4:
/// "Subroutine calls may be nested 10 deep").
const MAX_SUBR_DEPTH: usize = 10;

/// Most stems a Type 2 charstring can declare (TN#5177 Appendix B).
const MAX_STEMS: usize = 96;

/// Most operands a Type 2 operator can take (TN#5177 Appendix B). One is
/// kept back for the width, which the first stack-clearing operator
/// carries.
const MAX_ARGS: usize = 48 - 1;

/// A glyph converted to Type 2, with its width not yet encoded.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct Type2Glyph {
    /// Advance width, from `hsbw` or `sbw`.
    pub width: f64,
    /// Everything after the width: stems, hint masks, path, `endchar`.
    pub program: Vec<u8>,
}

impl Type2Glyph {
    /// The finished charstring for a Font DICT with the given
    /// `defaultWidthX` and `nominalWidthX`: the width is omitted when it
    /// equals the default and is otherwise written relative to the nominal.
    pub fn charstring(
        &self,
        default_width_x: f64,
        nominal_width_x: f64,
    ) -> Result<Vec<u8>, String> {
        let width = quantize(self.width);
        if width == default_width_x {
            return Ok(self.program.clone());
        }
        let mut out = Vec::with_capacity(self.program.len() + 5);
        push_number(&mut out, quantize(width - nominal_width_x))?;
        out.extend_from_slice(&self.program);
        Ok(out)
    }
}

/// `defaultWidthX` and `nominalWidthX` for a Font DICT whose glyphs have
/// `widths`: the most common width as the default, since those glyphs then
/// carry no width at all, and a nominal that puts as many of the others as
/// possible in the one-byte operand range (±107).
pub fn choose_widths(widths: &[f64]) -> (f64, f64) {
    let mut sorted: Vec<f64> = widths.iter().map(|&w| quantize(w)).collect();
    sorted.sort_by(f64::total_cmp);
    let Some(&first) = sorted.first() else {
        return (0.0, 0.0);
    };

    let mut default = first;
    let mut best = 0;
    let mut i = 0;
    while i < sorted.len() {
        let run = sorted[i..].partition_point(|&w| w == sorted[i]);
        if run > best {
            best = run;
            default = sorted[i];
        }
        i += run;
    }

    let rest: Vec<f64> = sorted.into_iter().filter(|&w| w != default).collect();
    let mut nominal = 0.0;
    let mut best = 0;
    for (i, &low) in rest.iter().enumerate() {
        let n = rest[i..].partition_point(|&w| w <= low + 214.0);
        if n > best {
            best = n;
            nominal = (low + 107.0).floor();
        }
    }
    (default, nominal)
}

/// Convert one Type 1 charstring to Type 2.
///
/// `charstring` and every entry of `subrs` are encrypted with `len_iv`
/// leading bytes, as in a Type 1 font's CharStrings and Subrs;
/// `usize::MAX` stands for `lenIV -1` (not encrypted), as in
/// [`decrypt_charstring`].
pub fn convert_charstring(
    charstring: &[u8],
    subrs: &[Vec<u8>],
    len_iv: usize,
) -> Result<Type2Glyph, String> {
    convert_charstring_transformed(charstring, subrs, len_iv, &Matrix::identity())
}

/// Convert one Type 1 charstring to Type 2, transforming the glyph by
/// `matrix` — for folding a matrix into the outline when the font it goes
/// into cannot carry it.
///
/// The outline is transformed exactly and the width is the horizontal
/// component of the transformed advance. A stem hint survives only where
/// the matrix keeps its axis apart from the other and does not flip it:
/// `hstem`s need `b == 0` and `d > 0`, `vstem`s `c == 0` and `a > 0`.
/// Otherwise that direction's hints are dropped, since a hint that no
/// longer lines up with the outline would distort it.
pub fn convert_charstring_transformed(
    charstring: &[u8],
    subrs: &[Vec<u8>],
    len_iv: usize,
    matrix: &Matrix,
) -> Result<Type2Glyph, String> {
    let mut interp = Interp {
        subrs,
        len_iv,
        stack: Vec::new(),
        ps_stack: Vec::new(),
        x: 0.0,
        y: 0.0,
        sbx: 0.0,
        sby: 0.0,
        width: 0.0,
        need_move: true,
        flex: None,
        hints: Vec::new(),
        hints_changed: false,
        groups: Vec::new(),
        cmds: Vec::new(),
    };
    interp.run(&decrypt_charstring(charstring, len_iv), 0)?;

    let identity = [matrix.a, matrix.b, matrix.c, matrix.d, matrix.tx, matrix.ty]
        == [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let groups: Vec<Vec<Stem>> = if identity {
        interp.groups
    } else {
        interp
            .groups
            .iter()
            .map(|g| g.iter().filter_map(|s| transform_stem(s, matrix)).collect())
            .collect()
    };
    let cmds: Vec<Cmd> = if identity {
        interp.cmds
    } else {
        interp
            .cmds
            .iter()
            .map(|c| transform_cmd(c, matrix))
            .collect()
    };
    Ok(Type2Glyph {
        width: matrix.transform_delta(interp.width, 0.0).0,
        program: emit(&groups, &cmds)?,
    })
}

/// `stem` under `m`, or `None` when `m` breaks it (see
/// [`convert_charstring_transformed`]).
fn transform_stem(stem: &Stem, m: &Matrix) -> Option<Stem> {
    let (scale, shift) = if stem.horizontal {
        (m.b == 0.0 && m.d > 0.0).then_some((m.d, m.ty))?
    } else {
        (m.c == 0.0 && m.a > 0.0).then_some((m.a, m.tx))?
    };
    // An edge hint's width is a marker, not a distance: −21 puts the edge
    // at `pos + width`, −20 at `pos` (TN#5177 §4.3). Move the edge.
    let (pos, width) = match stem.width {
        -21.0 => (scale * (stem.pos - 21.0) + shift + 21.0, -21.0),
        -20.0 => (scale * stem.pos + shift, -20.0),
        w => (scale * stem.pos + shift, scale * w),
    };
    Some(Stem {
        horizontal: stem.horizontal,
        pos,
        width,
    })
}

/// A path command under `m`.
fn transform_cmd(cmd: &Cmd, m: &Matrix) -> Cmd {
    let t = |p: &Pt| m.transform_point(p.0, p.1);
    match cmd {
        Cmd::Move(p) => Cmd::Move(t(p)),
        Cmd::Line(p) => Cmd::Line(t(p)),
        Cmd::Curve(p1, p2, p3) => Cmd::Curve(t(p1), t(p2), t(p3)),
        Cmd::Flex(points, depth) => Cmd::Flex(points.each_ref().map(t), *depth),
        Cmd::Hints(group) => Cmd::Hints(*group),
    }
}

// ---------------------------------------------------------------------------
// Type 1 interpretation
// ---------------------------------------------------------------------------

/// A point in character space.
type Pt = (f64, f64);

/// A stem hint in absolute character-space coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stem {
    /// A horizontal stem (`hstem`), which constrains y.
    horizontal: bool,
    /// The stem's first edge.
    pos: f64,
    /// Distance to its second edge; −20 and −21 mark edge (ghost) hints.
    width: f64,
}

/// A path or hint command, in absolute coordinates.
#[derive(Debug, Clone, PartialEq)]
enum Cmd {
    Move(Pt),
    Line(Pt),
    Curve(Pt, Pt, Pt),
    /// Two curves through the six points, and the flex height in
    /// hundredths of a device pixel.
    Flex([Pt; 6], f64),
    /// Hint group `n` takes effect.
    Hints(usize),
}

/// How execution continues after an operator.
#[derive(PartialEq)]
enum Flow {
    Next,
    /// `return`: this subroutine is done.
    Return,
    /// `endchar`: the glyph is done.
    End,
}

struct Interp<'a> {
    subrs: &'a [Vec<u8>],
    len_iv: usize,
    stack: Vec<f64>,
    /// The PostScript operand stack an OtherSubr leaves results on, for
    /// `pop` to take back.
    ps_stack: Vec<f64>,
    x: f64,
    y: f64,
    sbx: f64,
    sby: f64,
    width: f64,
    /// No subpath is open: after `hsbw` and after `closepath`.
    need_move: bool,
    /// Points collected by OtherSubr 2 between OtherSubrs 1 and 0.
    flex: Option<Vec<Pt>>,
    /// The stems now in force.
    hints: Vec<Stem>,
    /// `hints` differs from the last group committed to `groups`.
    hints_changed: bool,
    groups: Vec<Vec<Stem>>,
    cmds: Vec<Cmd>,
}

impl Interp<'_> {
    fn run(&mut self, data: &[u8], depth: usize) -> Result<Flow, String> {
        if depth > MAX_SUBR_DEPTH {
            return Err("Type 1: subroutines nested too deeply".into());
        }
        let mut pos = 0;
        while pos < data.len() {
            let b = data[pos];
            pos += 1;
            let flow = match b {
                32..=246 => {
                    self.stack.push(f64::from(b) - 139.0);
                    Flow::Next
                }
                247..=254 => {
                    let w = f64::from(*data.get(pos).ok_or("Type 1: truncated number")?);
                    pos += 1;
                    self.stack.push(if b <= 250 {
                        (f64::from(b) - 247.0) * 256.0 + w + 108.0
                    } else {
                        -(f64::from(b) - 251.0) * 256.0 - w - 108.0
                    });
                    Flow::Next
                }
                255 => {
                    let bytes = data.get(pos..pos + 4).ok_or("Type 1: truncated number")?;
                    pos += 4;
                    let v = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    self.stack.push(f64::from(v));
                    Flow::Next
                }
                12 => {
                    let op = *data.get(pos).ok_or("Type 1: truncated escape")?;
                    pos += 1;
                    self.escape(op)?;
                    Flow::Next
                }
                _ => self.op(b, depth)?,
            };
            match flow {
                Flow::Next => {}
                Flow::Return => return Ok(Flow::Next),
                Flow::End => return Ok(Flow::End),
            }
        }
        Ok(Flow::Next)
    }

    /// Take an operator's `N` operands from the top of the stack, and
    /// clear it.
    fn args<const N: usize>(&mut self, name: &str) -> Result<[f64; N], String> {
        let start = self
            .stack
            .len()
            .checked_sub(N)
            .ok_or_else(|| format!("Type 1: {name}: stack underflow"))?;
        let mut out = [0.0; N];
        out.copy_from_slice(&self.stack[start..]);
        self.stack.clear();
        Ok(out)
    }

    /// Execute a one-byte operator.
    fn op(&mut self, op: u8, depth: usize) -> Result<Flow, String> {
        match op {
            1 => {
                let [y, dy] = self.args("hstem")?;
                self.stem(true, self.sby + y, dy);
            }
            3 => {
                let [x, dx] = self.args("vstem")?;
                self.stem(false, self.sbx + x, dx);
            }
            4 => {
                let [dy] = self.args("vmoveto")?;
                self.rmoveto(0.0, dy);
            }
            5 => {
                let [dx, dy] = self.args("rlineto")?;
                self.line(dx, dy);
            }
            6 => {
                let [dx] = self.args("hlineto")?;
                self.line(dx, 0.0);
            }
            7 => {
                let [dy] = self.args("vlineto")?;
                self.line(0.0, dy);
            }
            8 => {
                let [a, b, c, d, e, f] = self.args("rrcurveto")?;
                self.curve(a, b, c, d, e, f);
            }
            9 => {
                // closepath: Type 1 keeps the current point (§6.4).
                self.stack.clear();
                self.need_move = true;
            }
            10 => {
                let idx = self
                    .stack
                    .pop()
                    .ok_or("Type 1: callsubr: stack underflow")?;
                let subr = (idx >= 0.0)
                    .then(|| self.subrs.get(idx as usize))
                    .flatten()
                    .ok_or_else(|| format!("Type 1: callsubr: no subr {idx}"))?;
                let subr = decrypt_charstring(subr, self.len_iv);
                return self.run(&subr, depth + 1);
            }
            11 => return Ok(Flow::Return),
            13 => {
                let [sbx, wx] = self.args("hsbw")?;
                self.side_bearing(sbx, 0.0, wx);
            }
            14 => {
                self.stack.clear();
                return Ok(Flow::End);
            }
            21 => {
                let [dx, dy] = self.args("rmoveto")?;
                self.rmoveto(dx, dy);
            }
            22 => {
                let [dx] = self.args("hmoveto")?;
                self.rmoveto(dx, 0.0);
            }
            30 => {
                let [dy1, dx2, dy2, dx3] = self.args("vhcurveto")?;
                self.curve(0.0, dy1, dx2, dy2, dx3, 0.0);
            }
            31 => {
                let [dx1, dx2, dy2, dy3] = self.args("hvcurveto")?;
                self.curve(dx1, 0.0, dx2, dy2, 0.0, dy3);
            }
            _ => return Err(format!("Type 1: unknown operator {op}")),
        }
        Ok(Flow::Next)
    }

    /// Execute a two-byte operator `12 op`.
    fn escape(&mut self, op: u8) -> Result<(), String> {
        match op {
            // dotsection
            0 => self.stack.clear(),
            1 => {
                let [x0, dx0, x1, dx1, x2, dx2] = self.args("vstem3")?;
                for (x, dx) in [(x0, dx0), (x1, dx1), (x2, dx2)] {
                    self.stem(false, self.sbx + x, dx);
                }
            }
            2 => {
                let [y0, dy0, y1, dy1, y2, dy2] = self.args("hstem3")?;
                for (y, dy) in [(y0, dy0), (y1, dy1), (y2, dy2)] {
                    self.stem(true, self.sby + y, dy);
                }
            }
            6 => return Err("Type 1: seac has no meaning in a CIDFont".into()),
            7 => {
                let [sbx, sby, wx, _wy] = self.args("sbw")?;
                self.side_bearing(sbx, sby, wx);
            }
            12 => {
                // div leaves its result on the stack.
                let b = self.stack.pop().ok_or("Type 1: div: stack underflow")?;
                let a = self.stack.pop().ok_or("Type 1: div: stack underflow")?;
                if b == 0.0 {
                    return Err("Type 1: div by zero".into());
                }
                self.stack.push(a / b);
            }
            16 => self.call_other_subr()?,
            17 => {
                let v = self
                    .ps_stack
                    .pop()
                    .ok_or("Type 1: pop with no OtherSubr result")?;
                self.stack.push(v);
            }
            33 => {
                let [x, y] = self.args("setcurrentpoint")?;
                // After flex this is where OtherSubr 0 already left the
                // current point. Anywhere else it moves the point without
                // drawing, which Type 2 can only express as a moveto.
                if (x, y) != (self.x, self.y) {
                    self.x = x;
                    self.y = y;
                    if !self.need_move && self.flex.is_none() {
                        self.push(Cmd::Move((x, y)));
                    }
                }
            }
            _ => return Err(format!("Type 1: unknown operator 12 {op}")),
        }
        Ok(())
    }

    /// `arg1 … argn n othersubr# callothersubr` ("Adobe Type 1 Font
    /// Format" chapter 8). Results go to the PostScript stack for `pop`.
    fn call_other_subr(&mut self) -> Result<(), String> {
        let num = self
            .stack
            .pop()
            .ok_or("Type 1: callothersubr: stack underflow")?;
        let n = self
            .stack
            .pop()
            .ok_or("Type 1: callothersubr: stack underflow")?;
        let start = (n >= 0.0)
            .then(|| self.stack.len().checked_sub(n as usize))
            .flatten()
            .ok_or("Type 1: callothersubr: stack underflow")?;
        let args = self.stack.split_off(start);

        match num as i64 {
            // End flex: args are the flex height and the final point; the
            // PostScript procedure leaves that point for `pop pop
            // setcurrentpoint`.
            0 => {
                let points = self.flex.take().ok_or("Type 1: flex end without start")?;
                // The first point collected is the reference point, used
                // only by Type 1 interpreters.
                let [_, p1, p2, p3, p4, p5, p6] = points[..] else {
                    return Err(format!("Type 1: flex with {} points", points.len()));
                };
                let depth = args.first().copied().unwrap_or(50.0);
                self.push(Cmd::Flex([p1, p2, p3, p4, p5, p6], depth));
                (self.x, self.y) = p6;
                self.ps_stack.push(p6.1);
                self.ps_stack.push(p6.0);
            }
            1 => self.flex = Some(Vec::with_capacity(7)),
            2 => {
                let points = self
                    .flex
                    .as_mut()
                    .ok_or("Type 1: flex point without start")?;
                points.push((self.x, self.y));
            }
            // Hint replacement: the new stems follow, from the subr whose
            // number `pop` hands to `callsubr`.
            3 => {
                self.hints.clear();
                self.hints_changed = true;
                self.ps_stack.extend(args.iter().rev());
            }
            // Any other OtherSubr is unknown here, so each `pop` receives
            // the next argument, arg1 first.
            _ => self.ps_stack.extend(args.iter().rev()),
        }
        Ok(())
    }

    fn side_bearing(&mut self, sbx: f64, sby: f64, wx: f64) {
        (self.sbx, self.sby) = (sbx, sby);
        (self.x, self.y) = (sbx, sby);
        self.width = wx;
    }

    fn stem(&mut self, horizontal: bool, pos: f64, width: f64) {
        let stem = Stem {
            horizontal,
            pos,
            width,
        };
        if !self.hints.contains(&stem) {
            self.hints.push(stem);
            self.hints_changed = true;
        }
    }

    fn rmoveto(&mut self, dx: f64, dy: f64) {
        self.x += dx;
        self.y += dy;
        // Inside flex, movetos only locate the points OtherSubr 2 collects.
        if self.flex.is_none() {
            self.push(Cmd::Move((self.x, self.y)));
            self.need_move = false;
        }
    }

    fn line(&mut self, dx: f64, dy: f64) {
        self.open_subpath();
        self.x += dx;
        self.y += dy;
        self.push(Cmd::Line((self.x, self.y)));
    }

    fn curve(&mut self, dx1: f64, dy1: f64, dx2: f64, dy2: f64, dx3: f64, dy3: f64) {
        self.open_subpath();
        let p1 = (self.x + dx1, self.y + dy1);
        let p2 = (p1.0 + dx2, p1.1 + dy2);
        let p3 = (p2.0 + dx3, p2.1 + dy3);
        (self.x, self.y) = p3;
        self.push(Cmd::Curve(p1, p2, p3));
    }

    /// A line or curve with no subpath open starts one at the current
    /// point, as it does in the Type 1 interpreter.
    fn open_subpath(&mut self) {
        if self.need_move {
            self.push(Cmd::Move((self.x, self.y)));
            self.need_move = false;
        }
    }

    /// Append a path command, first committing the hints in force if they
    /// changed since the last one.
    fn push(&mut self, cmd: Cmd) {
        if self.hints_changed {
            self.hints_changed = false;
            if self.groups.last() != Some(&self.hints) {
                self.groups.push(self.hints.clone());
                self.cmds.push(Cmd::Hints(self.groups.len() - 1));
            }
        }
        self.cmds.push(cmd);
    }
}

// ---------------------------------------------------------------------------
// Type 2 emission
// ---------------------------------------------------------------------------

const T2_HSTEM: u8 = 1;
const T2_VSTEM: u8 = 3;
const T2_VMOVETO: u8 = 4;
const T2_RLINETO: u8 = 5;
const T2_RRCURVETO: u8 = 8;
const T2_ENDCHAR: u8 = 14;
const T2_HSTEMHM: u8 = 18;
const T2_HINTMASK: u8 = 19;
const T2_RMOVETO: u8 = 21;
const T2_HMOVETO: u8 = 22;
const T2_VSTEMHM: u8 = 23;
const T2_FLEX: u8 = 35;

/// Serialise the collected hint groups and path as a Type 2 program,
/// without a width.
fn emit(groups: &[Vec<Stem>], cmds: &[Cmd]) -> Result<Vec<u8>, String> {
    let mut stems: Vec<Stem> = Vec::new();
    for stem in groups.iter().flatten() {
        if !stems.contains(stem) {
            stems.push(*stem);
        }
    }
    // Horizontal stems first, each direction in ascending order (TN#5177
    // §4.3). A glyph with more stems than Type 2 allows loses its hints
    // rather than its outline.
    stems.sort_by(|a, b| {
        (!a.horizontal, a.pos, a.width)
            .partial_cmp(&(!b.horizontal, b.pos, b.width))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if stems.len() > MAX_STEMS {
        stems.clear();
    }
    let masked = !stems.is_empty() && groups.len() > 1;

    let mut out = Vec::new();
    let (h_op, v_op) = if masked {
        (T2_HSTEMHM, T2_VSTEMHM)
    } else {
        (T2_HSTEM, T2_VSTEM)
    };
    for (horizontal, op) in [(true, h_op), (false, v_op)] {
        let dir: Vec<&Stem> = stems
            .iter()
            .filter(|s| s.horizontal == horizontal)
            .collect();
        for chunk in dir.chunks(MAX_ARGS / 2) {
            // Each operator's first edge is relative to 0, and each later
            // one to the previous stem's second edge.
            let mut edge = 0.0;
            for s in chunk {
                push_number(&mut out, quantize(s.pos - edge))?;
                push_number(&mut out, quantize(s.width))?;
                edge = quantize(s.pos) + quantize(s.width);
            }
            out.push(op);
        }
    }

    let mut path = PathWriter {
        out,
        at: (0.0, 0.0),
        pending: None,
        args: Vec::new(),
    };
    for cmd in cmds {
        match cmd {
            Cmd::Hints(group) => {
                if masked {
                    path.flush()?;
                    path.out.push(T2_HINTMASK);
                    path.out.extend(hint_mask(&stems, &groups[*group]));
                }
            }
            Cmd::Move(p) => {
                path.flush()?;
                let (dx, dy) = path.delta(*p);
                if dy == 0.0 {
                    path.op(&[dx], T2_HMOVETO)?;
                } else if dx == 0.0 {
                    path.op(&[dy], T2_VMOVETO)?;
                } else {
                    path.op(&[dx, dy], T2_RMOVETO)?;
                }
            }
            Cmd::Line(p) => {
                let (dx, dy) = path.delta(*p);
                path.batch(T2_RLINETO, &[dx, dy])?;
            }
            Cmd::Curve(p1, p2, p3) => {
                let (a, b) = path.delta(*p1);
                let (c, d) = path.delta(*p2);
                let (e, f) = path.delta(*p3);
                path.batch(T2_RRCURVETO, &[a, b, c, d, e, f])?;
            }
            Cmd::Flex(points, depth) => {
                path.flush()?;
                let mut args = Vec::with_capacity(13);
                for p in points {
                    let (dx, dy) = path.delta(*p);
                    args.extend([dx, dy]);
                }
                args.push(quantize(*depth));
                for v in args {
                    push_number(&mut path.out, v)?;
                }
                path.out.extend([12, T2_FLEX]);
            }
        }
    }
    path.flush()?;
    path.out.push(T2_ENDCHAR);
    Ok(path.out)
}

/// The `hintmask` bytes selecting `group` out of the declared `stems`:
/// one bit per stem, most significant bit first (TN#5177 §4.3).
fn hint_mask(stems: &[Stem], group: &[Stem]) -> Vec<u8> {
    let mut mask = vec![0u8; stems.len().div_ceil(8)];
    for (i, stem) in stems.iter().enumerate() {
        if group.contains(stem) {
            mask[i / 8] |= 0x80 >> (i % 8);
        }
    }
    mask
}

/// Path operators under construction, batching consecutive lines or
/// curves into one operator.
struct PathWriter {
    out: Vec<u8>,
    /// Where the Type 2 interpreter's current point will be.
    at: Pt,
    /// The operator the operands in `args` belong to.
    pending: Option<u8>,
    args: Vec<f64>,
}

impl PathWriter {
    /// The delta from the current point to `p`, as the Type 2 interpreter
    /// will add it up, and advance the current point by it.
    fn delta(&mut self, p: Pt) -> Pt {
        let d = (quantize(p.0 - self.at.0), quantize(p.1 - self.at.1));
        self.at = (self.at.0 + d.0, self.at.1 + d.1);
        d
    }

    fn batch(&mut self, op: u8, args: &[f64]) -> Result<(), String> {
        if self.pending != Some(op) || self.args.len() + args.len() > MAX_ARGS {
            self.flush()?;
        }
        self.pending = Some(op);
        self.args.extend_from_slice(args);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        if let Some(op) = self.pending.take() {
            let args = std::mem::take(&mut self.args);
            self.op(&args, op)?;
        }
        Ok(())
    }

    fn op(&mut self, args: &[f64], op: u8) -> Result<(), String> {
        for &v in args {
            push_number(&mut self.out, v)?;
        }
        self.out.push(op);
        Ok(())
    }
}

/// Round to the 16.16 fixed-point grid Type 2 numbers live on, snapping
/// values within rounding error of an integer to it.
fn quantize(v: f64) -> f64 {
    let r = v.round();
    if (v - r).abs() < 1e-9 {
        r
    } else {
        (v * 65536.0).round() / 65536.0
    }
}

/// Append a Type 2 number (TN#5177 §3.2): an integer in its shortest form,
/// or a 16.16 fixed-point value.
fn push_number(out: &mut Vec<u8>, v: f64) -> Result<(), String> {
    if v.fract() == 0.0 {
        match v as i32 {
            i @ -107..=107 => out.push((i + 139) as u8),
            i @ 108..=1131 => {
                let i = i - 108;
                out.extend([(i / 256 + 247) as u8, (i % 256) as u8]);
            }
            i @ -1131..=-108 => {
                let i = -i - 108;
                out.extend([(i / 256 + 251) as u8, (i % 256) as u8]);
            }
            i @ -32768..=32767 if f64::from(i) == v => {
                out.push(28);
                out.extend((i as i16).to_be_bytes());
            }
            _ => return Err(format!("Type 2: {v} is out of range")),
        }
        return Ok(());
    }
    if !(-32768.0..32768.0).contains(&v) {
        return Err(format!("Type 2: {v} is out of range"));
    }
    out.push(255);
    out.extend(((v * 65536.0).round() as i32).to_be_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::charstring::execute_charstring;
    use crate::type2_charstring::execute_type2_charstring;
    use crate::{PathSegment, PsPath};

    /// An unencrypted Type 1 charstring from numbers and operators;
    /// operators are written as `-(1000 + op)`, escaped ones as
    /// `-(1200 + op)`.
    fn t1(parts: &[i32]) -> Vec<u8> {
        let mut out = Vec::new();
        for &p in parts {
            if p <= -1200 {
                out.extend([12, (-p - 1200) as u8]);
            } else if p <= -1000 {
                out.push((-p - 1000) as u8);
            } else if (-107..=107).contains(&p) {
                out.push((p + 139) as u8);
            } else {
                out.push(255);
                out.extend(p.to_be_bytes());
            }
        }
        out
    }

    const HSTEM: i32 = -1001;
    const VSTEM: i32 = -1003;
    const RLINETO: i32 = -1005;
    const HLINETO: i32 = -1006;
    const VLINETO: i32 = -1007;
    const RRCURVETO: i32 = -1008;
    const CLOSEPATH: i32 = -1009;
    const CALLSUBR: i32 = -1010;
    const RETURN: i32 = -1011;
    const HSBW: i32 = -1013;
    const ENDCHAR: i32 = -1014;
    const RMOVETO: i32 = -1021;
    const HMOVETO: i32 = -1022;
    const DOTSECTION: i32 = -1200;
    const VSTEM3: i32 = -1201;
    const SBW: i32 = -1207;
    const DIV: i32 = -1212;
    const CALLOTHERSUBR: i32 = -1216;
    const POP: i32 = -1217;
    const SETCURRENTPOINT: i32 = -1233;

    /// The standard Subrs 0–3 ("Adobe Type 1 Font Format" §8.4).
    fn flex_subrs() -> Vec<Vec<u8>> {
        vec![
            t1(&[3, 0, CALLOTHERSUBR, POP, POP, SETCURRENTPOINT, RETURN]),
            t1(&[0, 1, CALLOTHERSUBR, RETURN]),
            t1(&[0, 2, CALLOTHERSUBR, RETURN]),
            t1(&[RETURN]),
        ]
    }

    /// Run the conversion and the Type 1 original, and check that the Type 2
    /// result draws the same outline with the same width.
    fn same_outline(t1: &[u8], subrs: &[Vec<u8>]) -> Type2Glyph {
        let glyph = convert_charstring(t1, subrs, usize::MAX).unwrap();
        let original = execute_charstring(t1, subrs, usize::MAX, false).unwrap();
        let cs = glyph.charstring(0.0, 0.0).unwrap();
        let converted = execute_type2_charstring(&cs, &[], &[], 0.0, 0.0, false).unwrap();
        assert_eq!(converted.width_x, original.width_x);
        assert_eq!(
            normalise(&converted.path),
            normalise(&original.path),
            "\nconverted {cs:?}"
        );
        glyph
    }

    /// The path as subpaths of drawn points, ignoring how each interpreter
    /// marks a subpath closed.
    fn normalise(path: &PsPath) -> Vec<Vec<(i64, i64)>> {
        let key = |x: f64, y: f64| ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64);
        let mut subpaths: Vec<Vec<(i64, i64)>> = Vec::new();
        for seg in &path.segments {
            match *seg {
                PathSegment::MoveTo(x, y) => subpaths.push(vec![key(x, y)]),
                PathSegment::LineTo(x, y) => subpaths.last_mut().unwrap().push(key(x, y)),
                PathSegment::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } => subpaths
                    .last_mut()
                    .unwrap()
                    .extend([key(x1, y1), key(x2, y2), key(x3, y3)]),
                PathSegment::ClosePath => {}
            }
        }
        // A subpath with nothing drawn draws nothing.
        subpaths.retain(|s| s.len() > 1);
        subpaths
    }

    fn subpaths_of(path: &PsPath) -> Vec<Vec<(i64, i64)>> {
        normalise(path)
    }

    /// The Type 2 operators in a charstring, ignoring operands.
    fn ops(cs: &[u8]) -> Vec<u16> {
        let mut ops = Vec::new();
        let mut i = 0;
        let mut stems = 0usize;
        let mut args = 0usize;
        while i < cs.len() {
            let b = cs[i];
            i += 1;
            match b {
                28 => i += 2,
                32..=246 => {}
                247..=254 => i += 1,
                255 => i += 4,
                12 => {
                    ops.push(1200 + u16::from(cs[i]));
                    i += 1;
                }
                _ => {
                    if matches!(b, 1 | 3 | 18 | 23) {
                        stems += args / 2;
                    }
                    if matches!(b, 19 | 20) {
                        stems += args / 2;
                        i += stems.div_ceil(8);
                    }
                    ops.push(u16::from(b));
                }
            }
            args = if matches!(b, 28 | 32..=255) {
                args + 1
            } else {
                0
            };
        }
        ops
    }

    #[test]
    fn side_bearing_folds_into_the_first_moveto() {
        // The block "C" of "Adobe Type 1 Font Format" §6.6.
        let c = t1(&[
            50, 800, HSBW, 0, 100, VSTEM, 0, 100, HSTEM, 600, 100, HSTEM, 0, HMOVETO, 700, HLINETO,
            100, VLINETO, -600, HLINETO, 500, VLINETO, 600, HLINETO, 100, VLINETO, -700, HLINETO,
            CLOSEPATH, ENDCHAR,
        ]);
        let glyph = same_outline(&c, &[]);
        assert_eq!(glyph.width, 800.0);
        // Stems relative to the side bearing: hstems 0–100 and 600–700,
        // vstem at x 50–150; then the move to (50, 0).
        let cs = glyph.charstring(800.0, 0.0).unwrap();
        let mut expected = Vec::new();
        for v in [0, 100, 500, 100] {
            push_number(&mut expected, f64::from(v)).unwrap();
        }
        expected.push(T2_HSTEM);
        for v in [50, 100] {
            push_number(&mut expected, f64::from(v)).unwrap();
        }
        expected.push(T2_VSTEM);
        push_number(&mut expected, 50.0).unwrap();
        expected.push(T2_HMOVETO);
        assert_eq!(cs[..expected.len()], expected);
        assert_eq!(ops(&cs), [1, 3, 22, 5, 14]);
    }

    #[test]
    fn widths_are_encoded_against_the_font_dict() {
        let glyph = convert_charstring(&t1(&[0, 600, HSBW, ENDCHAR]), &[], usize::MAX).unwrap();
        assert_eq!(glyph.charstring(600.0, 0.0).unwrap(), [T2_ENDCHAR]);
        let cs = glyph.charstring(500.0, 550.0).unwrap();
        assert_eq!(cs, [50 + 139, T2_ENDCHAR]);
        let r = execute_type2_charstring(&cs, &[], &[], 500.0, 550.0, true).unwrap();
        assert_eq!(r.width_x, 600.0);
    }

    #[test]
    fn sbw_offsets_both_coordinates() {
        let glyph = same_outline(
            &t1(&[
                10, 20, 500, 0, SBW, 5, 5, RMOVETO, 30, 0, RLINETO, CLOSEPATH, ENDCHAR,
            ]),
            &[],
        );
        assert_eq!(glyph.width, 500.0);
    }

    #[test]
    fn subroutines_are_inlined() {
        let subrs = vec![
            t1(&[RETURN]),
            t1(&[RETURN]),
            t1(&[RETURN]),
            t1(&[RETURN]),
            t1(&[100, 0, RLINETO, 5, CALLSUBR, RETURN]),
            t1(&[0, 100, RLINETO, RETURN]),
        ];
        let glyph = same_outline(
            &t1(&[0, 500, HSBW, 0, 0, RMOVETO, 4, CALLSUBR, CLOSEPATH, ENDCHAR]),
            &subrs,
        );
        assert!(!glyph.program.contains(&10), "no callsubr");
    }

    #[test]
    fn a_line_after_closepath_starts_a_new_subpath() {
        // Type 1 closepath keeps the current point; the second triangle
        // starts where the first one's last point was.
        let glyph = same_outline(
            &t1(&[
                0, 500, HSBW, 0, 0, RMOVETO, 100, 0, RLINETO, 0, 100, RLINETO, CLOSEPATH, 50, 0,
                RLINETO, 0, 50, RLINETO, CLOSEPATH, ENDCHAR,
            ]),
            &[],
        );
        let cs = glyph.charstring(500.0, 0.0).unwrap();
        assert_eq!(ops(&cs), [22, 5, 22, 5, 14]);
    }

    #[test]
    fn div_results_do_not_drift() {
        // Three thirds must land exactly back on x = 100 + 1.
        let third = [1, 3, DIV];
        let mut parts = vec![0, 500, HSBW, 100, 0, RMOVETO];
        for _ in 0..3 {
            parts.extend(third);
            parts.extend([0, RLINETO]);
        }
        parts.extend([0, 50, RLINETO, CLOSEPATH, ENDCHAR]);
        let glyph = same_outline(&t1(&parts), &[]);
        let cs = glyph.charstring(500.0, 0.0).unwrap();
        let r = execute_type2_charstring(&cs, &[], &[], 500.0, 0.0, false).unwrap();
        let PathSegment::LineTo(x, _) = r.path.segments[3] else {
            panic!("{:?}", r.path.segments);
        };
        assert!((x - 101.0).abs() < 1.0 / 65536.0, "{x}");
    }

    #[test]
    fn flex_becomes_the_type_2_flex_operator() {
        // The example of "Adobe Type 1 Font Format" §8.3, starting at
        // (100, -10).
        let mut parts = vec![0, 300, HSBW, 100, -10, RMOVETO, 1, CALLSUBR];
        for (dx, dy) in [
            (50, 0),
            (-35, 0),
            (10, 10),
            (25, 0),
            (25, 0),
            (10, -10),
            (15, 0),
        ] {
            parts.extend([dx, dy, RMOVETO, 2, CALLSUBR]);
        }
        parts.extend([
            50, 200, -10, 0, CALLSUBR, 0, 20, RLINETO, CLOSEPATH, ENDCHAR,
        ]);
        let glyph = same_outline(&t1(&parts), &flex_subrs());
        let cs = glyph.charstring(300.0, 0.0).unwrap();
        assert_eq!(ops(&cs), [21, 1235, 5, 14]);
        // flex operands: the six deltas and the flex height.
        let mut expected = Vec::new();
        for v in [15, 0, 10, 10, 25, 0, 25, 0, 10, -10, 15, 0, 50] {
            push_number(&mut expected, f64::from(v)).unwrap();
        }
        expected.extend([12, T2_FLEX]);
        let at = cs.windows(expected.len()).position(|w| w == expected);
        assert!(at.is_some(), "{cs:?}");
    }

    #[test]
    fn hint_replacement_becomes_hintmask() {
        // Example 1 of "Adobe Type 1 Font Format" §8.1: the "E" whose
        // serif hints replace its stem hints part-way.
        let mut subrs = flex_subrs();
        subrs.push(t1(&[0, 26, HSTEM, 674, 26, HSTEM, 86, 97, VSTEM, RETURN]));
        let e = t1(&[
            40,
            575,
            HSBW,
            0,
            32,
            HSTEM,
            350,
            32,
            HSTEM,
            668,
            32,
            HSTEM,
            421,
            36,
            VSTEM,
            359,
            26,
            VSTEM,
            86,
            97,
            VSTEM,
            0,
            HMOVETO,
            500,
            HLINETO,
            30,
            VLINETO,
            4,
            1,
            3,
            CALLOTHERSUBR,
            POP,
            CALLSUBR,
            -500,
            HLINETO,
            CLOSEPATH,
            ENDCHAR,
        ]);
        let glyph = same_outline(&e, &subrs);
        let cs = glyph.charstring(575.0, 0.0).unwrap();
        assert_eq!(ops(&cs), [18, 23, 19, 22, 5, 19, 5, 14]);

        // Stems sorted: h 0/26, 0/32, 350/32, 668/32, 674/26, then
        // v 126/97, 399/26, 461/36 (all offset by sbx 40).
        let masks: Vec<u8> = cs
            .windows(2)
            .filter(|w| w[0] == T2_HINTMASK)
            .map(|w| w[1])
            .collect();
        assert_eq!(masks, [0b0111_0111, 0b1000_1100]);
    }

    #[test]
    fn hints_declared_by_replacement_before_the_path_are_one_group() {
        // Fonts often put even their first hints in a subr, called through
        // OtherSubr 3 before any path.
        let mut subrs = flex_subrs();
        subrs.push(t1(&[0, 50, HSTEM, RETURN]));
        let glyph = same_outline(
            &t1(&[
                0,
                500,
                HSBW,
                4,
                1,
                3,
                CALLOTHERSUBR,
                POP,
                CALLSUBR,
                0,
                0,
                RMOVETO,
                100,
                0,
                RLINETO,
                CLOSEPATH,
                ENDCHAR,
            ]),
            &subrs,
        );
        let cs = glyph.charstring(500.0, 0.0).unwrap();
        assert_eq!(ops(&cs), [1, 22, 5, 14]);
    }

    #[test]
    fn stem3_and_dotsection() {
        let glyph = same_outline(
            &t1(&[
                0, 500, HSBW, 10, 20, 100, 20, 190, 20, VSTEM3, 0, 0, RMOVETO, DOTSECTION, 100, 0,
                RLINETO, CLOSEPATH, DOTSECTION, ENDCHAR,
            ]),
            &[],
        );
        let cs = glyph.charstring(500.0, 0.0).unwrap();
        assert_eq!(ops(&cs), [3, 22, 5, 14]);
    }

    #[test]
    fn many_segments_respect_the_argument_limit() {
        let mut parts = vec![0, 500, HSBW, 0, 0, RMOVETO];
        for i in 0..40 {
            parts.extend([1 + i % 3, 2, RLINETO]);
        }
        for _ in 0..10 {
            parts.extend([1, 2, 3, 4, 5, 6, RRCURVETO]);
        }
        parts.extend([CLOSEPATH, ENDCHAR]);
        let glyph = same_outline(&t1(&parts), &[]);
        let cs = glyph.charstring(0.0, 0.0).unwrap();
        // 40 lines: 23 + 17 pairs; 10 curves: 7 + 3.
        assert_eq!(ops(&cs), [22, 5, 5, 8, 8, 14]);
    }

    #[test]
    fn a_transform_moves_outline_width_and_the_hints_it_keeps() {
        let program = t1(&[
            10, 500, HSBW, 0, 100, HSTEM, 0, -21, HSTEM, 0, 40, VSTEM, 0, 0, RMOVETO, 40, 0,
            RLINETO, 0, 100, RLINETO, CLOSEPATH, ENDCHAR,
        ]);
        let plain = convert_charstring(&program, &[], usize::MAX).unwrap();
        let original = execute_type2_charstring(
            &plain.charstring(0.0, 0.0).unwrap(),
            &[],
            &[],
            0.0,
            0.0,
            false,
        )
        .unwrap();

        // Scale by 2 and shift: every hint survives.
        let m = Matrix::new(2.0, 0.0, 0.0, 2.0, 5.0, 7.0);
        let glyph = convert_charstring_transformed(&program, &[], usize::MAX, &m).unwrap();
        assert_eq!(glyph.width, 1000.0);
        let cs = glyph.charstring(0.0, 0.0).unwrap();
        let r = execute_type2_charstring(&cs, &[], &[], 0.0, 0.0, false).unwrap();
        assert_eq!(
            subpaths_of(&r.path),
            subpaths_of(&original.path.transform(&m))
        );
        // hstems: the edge hint's edge, at y -21, moves to 2 * -21 + 7;
        // the stem 0/100 becomes 7/200; the vstem 10/40 becomes 25/80.
        let mut expected = Vec::new();
        for v in [2.0 * -21.0 + 7.0 + 21.0, -21.0] {
            push_number(&mut expected, v).unwrap();
        }
        let edge = 2.0 * -21.0 + 7.0;
        for v in [7.0 - edge, 200.0] {
            push_number(&mut expected, v).unwrap();
        }
        expected.push(T2_HSTEM);
        for v in [25.0, 80.0] {
            push_number(&mut expected, v).unwrap();
        }
        expected.push(T2_VSTEM);
        let mut width = Vec::new();
        push_number(&mut width, 1000.0).unwrap();
        assert_eq!(cs[width.len()..width.len() + expected.len()], expected);

        // An oblique: vertical stems no longer line up and are dropped.
        let m = Matrix::new(1.0, 0.0, 0.176, 1.0, 0.0, 0.0);
        let glyph = convert_charstring_transformed(&program, &[], usize::MAX, &m).unwrap();
        let cs = glyph.charstring(0.0, 0.0).unwrap();
        assert_eq!(ops(&cs), [1, 22, 5, 14]);
        let r = execute_type2_charstring(&cs, &[], &[], 0.0, 0.0, false).unwrap();
        assert_eq!(
            subpaths_of(&r.path),
            subpaths_of(&original.path.transform(&m))
        );
    }

    #[test]
    fn seac_and_bad_programs_are_errors() {
        let seac = t1(&[0, 500, HSBW, 0, 0, 0, 65, 194, -1206]);
        assert!(convert_charstring(&seac, &[], usize::MAX).is_err());
        let missing_subr = t1(&[0, 500, HSBW, 9, CALLSUBR, ENDCHAR]);
        assert!(convert_charstring(&missing_subr, &[], usize::MAX).is_err());
        let recursive = vec![t1(&[0, CALLSUBR, RETURN])];
        assert!(convert_charstring(&t1(&[0, CALLSUBR]), &recursive, usize::MAX).is_err());
        let underflow = t1(&[0, RLINETO]);
        assert!(convert_charstring(&underflow, &[], usize::MAX).is_err());
    }

    #[test]
    fn encrypted_charstrings_are_decrypted() {
        let plain = t1(&[
            0, 500, HSBW, 0, 0, RMOVETO, 10, 10, RLINETO, CLOSEPATH, ENDCHAR,
        ]);
        // Charstring encryption, r = 4330, with four leading bytes.
        let mut r: u16 = 4330;
        let enc: Vec<u8> = [0u8; 4]
            .iter()
            .chain(&plain)
            .map(|&p| {
                let c = p ^ (r >> 8) as u8;
                r = (u16::from(c).wrapping_add(r))
                    .wrapping_mul(52845)
                    .wrapping_add(22719);
                c
            })
            .collect();
        let a = convert_charstring(&enc, &[], 4).unwrap();
        let b = convert_charstring(&plain, &[], usize::MAX).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn choose_widths_prefers_the_common_width_and_one_byte_deltas() {
        assert_eq!(choose_widths(&[]), (0.0, 0.0));
        let (default, nominal) = choose_widths(&[1000.0, 1000.0, 1000.0, 500.0, 520.0, 610.0]);
        assert_eq!(default, 1000.0);
        for w in [500.0, 520.0, 610.0] {
            assert!((w - nominal).abs() <= 107.0, "{w} vs {nominal}");
        }
    }

    #[test]
    fn numbers_use_the_type_2_encodings() {
        let enc = |v: f64| {
            let mut out = Vec::new();
            push_number(&mut out, v).unwrap();
            out
        };
        assert_eq!(enc(0.0), [139]);
        assert_eq!(enc(1000.0), [0xfa, 0x7c]);
        assert_eq!(enc(-1000.0), [0xfe, 0x7c]);
        assert_eq!(enc(10000.0), [28, 0x27, 0x10]);
        assert_eq!(enc(0.5), [255, 0, 0, 0x80, 0]);
        assert_eq!(enc(-1.5), [255, 0xff, 0xfe, 0x80, 0]);
        assert!(push_number(&mut Vec::new(), 40000.0).is_err());
    }
}
