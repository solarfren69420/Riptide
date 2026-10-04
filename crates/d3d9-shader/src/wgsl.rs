//! Translate a Direct3D 9 shader program (SM2/SM3) to a WGSL module.
//!
//! Registers become `vec4` variables; float / int / bool constants come from one uniform block
//! (`k.c`, `k.i`, `k.b`) the host fills, except where the program `def`s them (inlined).
//! Samplers become texture + sampler pairs. Vertex inputs are located by their declaration
//! order, vertex outputs / pixel inputs by their usage (`texcoordN` at location N, `colorN` at
//! 8 + N). Write masks become per-component assignments; `_sat` a clamp; `ifc`/`breakc`,
//! `rep`/`loop` WGSL `if` / `for`. `texld` inside control flow samples level 0: WGSL forbids
//! implicit derivatives there.

use crate::disasm::{reg_type, Program, NO_DEST};
use std::collections::HashMap;
use std::fmt::Write;

/// Where the module's resources go.
#[derive(Clone, Debug)]
pub struct Options {
    /// Bind group expression, e.g. `"2"` or Bevy's `"#{MATERIAL_BIND_GROUP}"`.
    pub group: String,
    /// The constant block's binding; sampler `sN` takes `first_texture + 2N` (texture) and
    /// `first_texture + 2N + 1` (sampler).
    pub constants_binding: u32,
    pub first_texture: u32,
    /// Entry point name.
    pub entry: String,
    /// Text put before the module (imports: e.g. `#import bevy_pbr::mesh_view_bindings::{view, globals}`).
    pub prelude: String,
    /// Float constant registers the host computes in the shader instead (a `vec4<f32>` WGSL
    /// expression), e.g. a matrix row from the engine's camera, so one material serves every view.
    pub constants: HashMap<u32, String>,
    /// Samplers bound elsewhere: register -> (texture expression, sampler expression).
    pub samplers: HashMap<u32, (String, String)>,
    /// A WGSL function (defined in the prelude) applied to a sampler's 2D coordinates, e.g. to
    /// read a host texture flipped.
    pub coords: HashMap<u32, String>,
}

impl Default for Options {
    fn default() -> Self {
        Self { group: "0".into(), constants_binding: 0, first_texture: 1, entry: "main".into(), prelude: String::new(), constants: HashMap::new(), samplers: HashMap::new(), coords: HashMap::new() }
    }
}

/// The constant block every translated program shares (host side: 256 vec4 floats, 16 ivec4,
/// 16 bools padded to uvec4).
pub const CONSTANTS_STRUCT: &str = "struct D3d9Constants {\n    c: array<vec4<f32>, 256>,\n    i: array<vec4<i32>, 16>,\n    b: array<vec4<u32>, 16>,\n};\n";

struct Ctx<'a> {
    p: &'a Program,
    opts: &'a Options,
    fdef: HashMap<u32, [f32; 4]>,
    idef: HashMap<u32, [i32; 4]>,
    depth: usize,
    loops: Vec<Option<String>>,
}

fn component(i: u32) -> char {
    b"xyzw"[i as usize & 3] as char
}

impl Ctx<'_> {
    fn reg(&self, t: u32, rel: Option<u32>) -> String {
        let n = t & 0x7ff;
        let pixel = self.p.pixel;
        match reg_type(t) {
            0 => format!("r{n}"),
            1 => format!("v{n}"),
            2 => match rel {
                Some(r) => {
                    let idx = if reg_type(r) == 15 { "aL".to_string() } else { format!("a0.{}", component((r >> 16) & 3)) };
                    format!("k.c[{idx} + {n}]")
                }
                None => match (self.opts.constants.get(&n), self.fdef.get(&n)) {
                    (Some(e), _) => format!("({e})"),
                    (None, Some(v)) => format!("vec4<f32>({:?}, {:?}, {:?}, {:?})", v[0], v[1], v[2], v[3]),
                    (None, None) => format!("k.c[{n}]"),
                },
            },
            3 if !pixel => "vec4<f32>(a0)".into(),
            3 => format!("t{n}"),
            4 => "o_pos".into(),
            6 => format!("o{n}"),
            7 => match self.idef.get(&n) {
                Some(v) => format!("vec4<f32>(vec4<i32>({}, {}, {}, {}))", v[0], v[1], v[2], v[3]),
                None => format!("vec4<f32>(k.i[{n}])"),
            },
            8 => format!("oC{n}"),
            14 => format!("vec4<f32>(f32(k.b[{n}].x))"),
            15 => "vec4<f32>(f32(aL))".into(),
            17 => if n == 0 { "frag_pos".into() } else { "vec4<f32>(select(-1.0, 1.0, front_facing))".into() },
            x => format!("/*reg{x}*/vec4<f32>(0.0)"),
        }
    }

    fn src(&self, t: u32, rel: Option<u32>) -> String {
        let base = self.reg(t, rel);
        let s = (t >> 16) & 0xff;
        let sw: Vec<char> = (0..4).map(|k| component((s >> (2 * k)) & 3)).collect();
        let v = if s == 0xE4 {
            base
        } else if sw.iter().all(|c| *c == sw[0]) {
            format!("vec4<f32>(({base}).{})", sw[0])
        } else {
            format!("({base}).{}", sw.iter().collect::<String>())
        };
        match (t >> 24) & 0xf {
            0 => v,
            1 => format!("(-{v})"),
            2 => format!("({v} - vec4<f32>(0.5))"),
            3 => format!("(-({v} - vec4<f32>(0.5)))"),
            4 => format!("({v} * 2.0 - vec4<f32>(1.0))"),
            5 => format!("(-({v} * 2.0 - vec4<f32>(1.0)))"),
            6 => format!("(vec4<f32>(1.0) - {v})"),
            7 => format!("({v} * 2.0)"),
            8 => format!("(-({v} * 2.0))"),
            11 => format!("abs({v})"),
            12 => format!("(-abs({v}))"),
            _ => v,
        }
    }

    fn sampler_kind(&self, n: u32) -> u32 {
        sampler_kinds(self.p).get(&n).copied().unwrap_or(2)
    }
}

/// Sampler register -> texture type (2 2D, 3 cube, 4 volume), from the `dcl` instructions.
fn sampler_kinds(p: &Program) -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    walk(p, |op, _w, params| {
        if op == 31 && params.len() >= 2 && reg_type(params[1]) == 10 {
            out.insert(params[1] & 0x7ff, (params[0] >> 27) & 0xf);
        }
    });
    out
}

/// Every instruction of `p` (opcode, instruction token, parameter tokens).
fn walk(p: &Program, mut f: impl FnMut(u32, u32, &[u32])) {
    let t = &p.tokens;
    let mut i = 1;
    while i < t.len() {
        let w = t[i];
        let op = w & 0xffff;
        if op == 0xFFFF {
            break;
        }
        if op == 0xFFFE {
            i += ((w >> 16) & 0x7fff) as usize + 1;
            continue;
        }
        let len = ((w >> 24) & 0xf) as usize;
        f(op, w, &t[(i + 1).min(t.len())..(i + 1 + len).min(t.len())]);
        i += len + 1;
    }
}

/// Split parameter tokens into (token, relative-address token) pairs.
fn params_of(op: u32, params: &[u32]) -> Vec<(u32, Option<u32>)> {
    let mut out = Vec::new();
    let mut k = 0;
    while k < params.len() {
        let t = params[k];
        let is_dest = k == 0 && !NO_DEST.contains(&op);
        let rel = (t & 0x2000 != 0 && !is_dest).then(|| params.get(k + 1).copied()).flatten();
        out.push((t, rel));
        k += if rel.is_some() { 2 } else { 1 };
    }
    out
}

fn compare(control: u32) -> &'static str {
    match control & 7 {
        1 => ">",
        2 => "==",
        3 => ">=",
        4 => "<",
        5 => "!=",
        6 => "<=",
        _ => "!=",
    }
}

/// The WGSL module for `p`.
pub fn translate(p: &Program, opts: &Options) -> String {
    let mut ctx = Ctx { p, opts, fdef: HashMap::new(), idef: HashMap::new(), depth: 0, loops: Vec::new() };
    let mut inputs: Vec<(u32, u32, u32)> = Vec::new(); // (register, usage, usage index)
    let mut outputs: Vec<(u32, u32, u32)> = Vec::new();
    let mut temps = std::collections::BTreeSet::new();
    let (mut uses_a0, mut uses_al, mut uses_frag, mut uses_face) = (false, false, false, false);
    walk(p, |op, _w, params| {
        match op {
            31 if params.len() >= 2 => {
                let (u, r) = (params[0], params[1]);
                match reg_type(r) {
                    1 => inputs.push((r & 0x7ff, u & 0x1f, (u >> 16) & 0xf)),
                    6 => outputs.push((r & 0x7ff, u & 0x1f, (u >> 16) & 0xf)),
                    _ => {}
                }
            }
            81 if params.len() >= 5 => {
                ctx.fdef.insert(params[0] & 0x7ff, [0, 1, 2, 3].map(|k| f32::from_bits(params[1 + k])));
            }
            48 if params.len() >= 5 => {
                ctx.idef.insert(params[0] & 0x7ff, [0, 1, 2, 3].map(|k| params[1 + k] as i32));
            }
            _ => {}
        }
        for (t, rel) in params_of(op, params) {
            match reg_type(t) {
                0 => {
                    temps.insert(t & 0x7ff);
                }
                3 if !p.pixel => uses_a0 = true,
                15 => uses_al = true,
                17 => {
                    if t & 0x7ff == 0 {
                        uses_frag = true
                    } else {
                        uses_face = true
                    }
                }
                _ => {}
            }
            if let Some(r) = rel {
                if reg_type(r) == 15 {
                    uses_al = true
                } else {
                    uses_a0 = true
                }
            }
        }
    });
    let samplers = sampler_kinds(p);
    let mut m = opts.prelude.clone();
    if !m.is_empty() && !m.ends_with('\n') {
        m.push('\n');
    }
    let g = &opts.group;
    m.push_str(CONSTANTS_STRUCT);
    let _ = writeln!(m, "@group({g}) @binding({}) var<uniform> k: D3d9Constants;", opts.constants_binding);
    let mut sorted: Vec<_> = samplers.iter().collect();
    sorted.sort();
    for (n, kind) in sorted {
        if opts.samplers.contains_key(n) {
            continue;
        }
        let ty = match kind {
            3 => "texture_cube<f32>",
            4 => "texture_3d<f32>",
            _ => "texture_2d<f32>",
        };
        let _ = writeln!(m, "@group({g}) @binding({}) var tex{n}: {ty};", opts.first_texture + 2 * n);
        let _ = writeln!(m, "@group({g}) @binding({}) var samp{n}: sampler;", opts.first_texture + 2 * n + 1);
    }
    // Vertex output / pixel input slots by usage, the same on both sides: texcoordN at N, colorN at
    // 8 + N, normalN at 10 + N, then tangent, binormal, fog, psize, blend data, extra positions.
    let location = |usage: u32, index: u32| match usage {
        5 => index,
        10 => 8 + index,
        3 => 10 + index,
        6 => 12,
        7 => 13,
        11 => 14,
        4 => 15,
        1 => 16 + index,
        2 => 18 + index,
        u => 20 + u * 4 + index,
    };
    if p.pixel {
        m.push_str("struct PsIn {\n    @builtin(position) frag_pos: vec4<f32>,\n");
        if uses_face {
            m.push_str("    @builtin(front_facing) front_facing: bool,\n");
        }
        for (r, u, i) in &inputs {
            let _ = writeln!(m, "    @location({}) v{r}: vec4<f32>,", location(*u, *i));
        }
        m.push_str("};\n");
        let _ = writeln!(m, "@fragment\nfn {}(input: PsIn) -> @location(0) vec4<f32> {{", opts.entry);
        for (r, _, _) in &inputs {
            let _ = writeln!(m, "    let v{r} = input.v{r};");
        }
        if uses_frag {
            m.push_str("    let frag_pos = input.frag_pos;\n");
        }
        if uses_face {
            m.push_str("    let front_facing = input.front_facing;\n");
        }
        m.push_str("    var oC0 = vec4<f32>(0.0);\n");
    } else {
        m.push_str("struct VsIn {\n");
        for (k, (r, _, _)) in inputs.iter().enumerate() {
            let _ = writeln!(m, "    @location({k}) v{r}: vec4<f32>,");
        }
        m.push_str("};\nstruct VsOut {\n    @builtin(position) pos: vec4<f32>,\n");
        for (r, u, i) in &outputs {
            if *u != 0 {
                let _ = writeln!(m, "    @location({}) o{r}: vec4<f32>,", location(*u, *i));
            }
        }
        m.push_str("};\n");
        let _ = writeln!(m, "@vertex\nfn {}(input: VsIn) -> VsOut {{", opts.entry);
        for (r, _, _) in &inputs {
            let _ = writeln!(m, "    let v{r} = input.v{r};");
        }
        m.push_str("    var o_pos = vec4<f32>(0.0);\n");
        for (r, _, _) in &outputs {
            let _ = writeln!(m, "    var o{r} = vec4<f32>(0.0);");
        }
    }
    for t in &temps {
        let _ = writeln!(m, "    var r{t} = vec4<f32>(0.0);");
    }
    if uses_a0 {
        m.push_str("    var a0 = vec4<i32>(0);\n");
    }
    if uses_al {
        m.push_str("    var aL: i32 = 0;\n");
    }
    // Vertex outputs declared as `position` go to o_pos.
    let position_out: Vec<u32> = outputs.iter().filter(|(_, u, _)| *u == 0).map(|(r, _, _)| *r).collect();
    let ret = |p: &Program| -> String {
        if p.pixel {
            "return oC0;".into()
        } else {
            let mut s = String::from("var out: VsOut; out.pos = ");
            s += &position_out.first().map_or("o_pos".to_string(), |r| format!("o{r}"));
            s += ";";
            for (r, u, _) in &outputs {
                if *u != 0 {
                    s += &format!(" out.o{r} = o{r};");
                }
            }
            s + " return out;"
        }
    };
    let mut body = String::new();
    walk(p, |op, w, params| {
        let ps = params_of(op, params);
        let indent = "    ".repeat(ctx.depth + 1);
        let s = |k: usize| ps.get(k).map(|(t, rel)| ctx.src(*t, *rel)).unwrap_or_else(|| "vec4<f32>(0.0)".into());
        let in_flow = ctx.depth > 0;
        let value: Option<String> = match op {
            0 | 31 | 81 | 47 | 48 => None,
            1 => Some(s(1)),
            2 => Some(format!("{} + {}", s(1), s(2))),
            3 => Some(format!("{} - {}", s(1), s(2))),
            4 => Some(format!("{} * {} + {}", s(1), s(2), s(3))),
            5 => Some(format!("{} * {}", s(1), s(2))),
            6 => Some(format!("vec4<f32>(1.0 / ({}).x)", s(1))),
            7 => Some(format!("vec4<f32>(inverseSqrt(abs(({}).x)))", s(1))),
            8 => Some(format!("vec4<f32>(dot(({}).xyz, ({}).xyz))", s(1), s(2))),
            9 => Some(format!("vec4<f32>(dot({}, {}))", s(1), s(2))),
            10 => Some(format!("min({}, {})", s(1), s(2))),
            11 => Some(format!("max({}, {})", s(1), s(2))),
            12 => Some(format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {} < {})", s(1), s(2))),
            13 => Some(format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {} >= {})", s(1), s(2))),
            14 => Some(format!("vec4<f32>(exp2(({}).x))", s(1))),
            15 => Some(format!("vec4<f32>(log2(abs(({}).x)))", s(1))),
            18 => Some(format!("mix({}, {}, {})", s(3), s(2), s(1))),
            19 => Some(format!("fract({})", s(1))),
            32 => Some(format!("vec4<f32>(pow(abs(({}).x), ({}).x))", s(1), s(2))),
            33 => Some(format!("vec4<f32>(cross(({}).xyz, ({}).xyz), 0.0)", s(1), s(2))),
            34 => Some(format!("sign({})", s(1))),
            35 => Some(format!("abs({})", s(1))),
            36 => Some(format!("vec4<f32>(normalize(({0}).xyz), ({0}).w * inverseSqrt(dot(({0}).xyz, ({0}).xyz)))", s(1))),
            37 => Some(format!("vec4<f32>(cos(({0}).x), sin(({0}).x), 0.0, 0.0)", s(1))),
            46 => {
                let (dt, _) = ps[0];
                let m = (dt >> 16) & 0xf;
                for k in 0..4 {
                    if m & (1 << k) != 0 {
                        let c = component(k);
                        let _ = writeln!(body, "{indent}a0.{c} = i32(round(({}).{c}));", s(1));
                    }
                }
                None
            }
            88 => Some(format!("select({}, {}, {} >= vec4<f32>(0.0))", s(3), s(2), s(1))),
            90 => Some(format!("vec4<f32>(dot(({}).xy, ({}).xy) + ({}).x)", s(1), s(2), s(3))),
            91 => Some(format!("dpdx({})", s(1))),
            92 => Some(format!("dpdy({})", s(1))),
            20..=24 => {
                // mNxM: rows from consecutive constant registers after src1.
                let (t1, rel1) = ps[2];
                let rows = match op {
                    20 | 22 => 4,
                    21 | 23 => 3,
                    _ => 2,
                };
                let dot = if op == 20 || op == 21 { "dot({a}, {b})" } else { "dot(({a}).xyz, ({b}).xyz)" };
                let comps: Vec<String> = (0..4)
                    .map(|k| {
                        if k < rows {
                            let row = ctx.src(t1 + k as u32, rel1);
                            dot.replace("{a}", &s(1)).replace("{b}", &row)
                        } else {
                            "0.0".into()
                        }
                    })
                    .collect();
                Some(format!("vec4<f32>({})", comps.join(", ")))
            }
            66 | 95 => {
                let (st, _) = ps[2];
                let n = st & 0x7ff;
                let coords = ctx.src(ps[1].0, ps[1].1);
                let kind = ctx.sampler_kind(n);
                let c = if kind == 3 || kind == 4 { format!("({coords}).xyz") } else { format!("({coords}).xy") };
                let projected = op == 66 && (w >> 16) & 0xff == 1;
                let c = if projected { format!("{c} / ({coords}).w") } else { c };
                let c = match ctx.opts.coords.get(&n) {
                    Some(f) if kind == 2 => format!("{f}({c})"),
                    _ => c,
                };
                let (tex, samp) = ctx.opts.samplers.get(&n).cloned().unwrap_or_else(|| (format!("tex{n}"), format!("samp{n}")));
                let sample = if op == 95 {
                    format!("textureSampleLevel({tex}, {samp}, {c}, ({coords}).w)")
                } else if !p.pixel || in_flow {
                    format!("textureSampleLevel({tex}, {samp}, {c}, 0.0)")
                } else {
                    format!("textureSample({tex}, {samp}, {c})")
                };
                Some(sample)
            }
            65 => {
                let _ = writeln!(body, "{indent}if (any(({}).xyz < vec3<f32>(0.0))) {{ discard; }}", s(0));
                None
            }
            40 => {
                let _ = writeln!(body, "{indent}if ({}.x != 0.0) {{", s(0));
                ctx.depth += 1;
                None
            }
            41 => {
                let _ = writeln!(body, "{indent}if (({}).x {} ({}).x) {{", s(0), compare(w >> 16), s(1));
                ctx.depth += 1;
                None
            }
            42 => {
                let outer = "    ".repeat(ctx.depth);
                let _ = writeln!(body, "{outer}}} else {{");
                None
            }
            43 => {
                ctx.depth = ctx.depth.saturating_sub(1);
                let _ = writeln!(body, "{}}}", "    ".repeat(ctx.depth + 1));
                None
            }
            38 => {
                let n = ps[0].0 & 0x7ff;
                let count = ctx.idef.get(&n).map_or(format!("k.i[{n}].x"), |v| v[0].to_string());
                let d = ctx.loops.len();
                let _ = writeln!(body, "{indent}for (var rep{d}: i32 = 0; rep{d} < {count}; rep{d}++) {{");
                ctx.loops.push(None);
                ctx.depth += 1;
                None
            }
            27 => {
                // loop aL, iN: aL starts at i.y and steps by i.z, i.x times.
                let n = ps.get(1).map_or(0, |(t, _)| t & 0x7ff);
                let (count, start, step) = match ctx.idef.get(&n) {
                    Some(v) => (v[0].to_string(), v[1].to_string(), v[2].to_string()),
                    None => (format!("k.i[{n}].x"), format!("k.i[{n}].y"), format!("k.i[{n}].z")),
                };
                let d = ctx.loops.len();
                let _ = writeln!(body, "{indent}aL = {start};\n{indent}for (var lp{d}: i32 = 0; lp{d} < {count}; lp{d}++) {{");
                ctx.loops.push(Some(step));
                ctx.depth += 1;
                None
            }
            39 | 29 => {
                let step = ctx.loops.pop().flatten();
                if let Some(st) = step {
                    let _ = writeln!(body, "{}aL += {st};", "    ".repeat(ctx.depth + 1));
                }
                ctx.depth = ctx.depth.saturating_sub(1);
                let _ = writeln!(body, "{}}}", "    ".repeat(ctx.depth + 1));
                None
            }
            44 => {
                let _ = writeln!(body, "{indent}break;");
                None
            }
            45 => {
                let _ = writeln!(body, "{indent}if (({}).x {} ({}).x) {{ break; }}", s(0), compare(w >> 16), s(1));
                None
            }
            28 => {
                let _ = writeln!(body, "{indent}{}", ret(p));
                None
            }
            other => {
                let _ = writeln!(body, "{indent}// unsupported opcode {other}");
                None
            }
        };
        if let Some(v) = value {
            let (dt, _) = ps[0];
            let dest = ctx.reg(dt, None);
            let sat = (dt >> 20) & 1 != 0;
            let v = if sat { format!("clamp({v}, vec4<f32>(0.0), vec4<f32>(1.0))") } else { v };
            let m = (dt >> 16) & 0xf;
            if m == 0xf {
                let _ = writeln!(body, "{indent}{dest} = {v};");
            } else {
                let _ = writeln!(body, "{indent}{{ let t = {v};");
                for k in 0..4 {
                    if m & (1 << k) != 0 {
                        let c = component(k);
                        let _ = writeln!(body, "{indent}  {dest}.{c} = t.{c};");
                    }
                }
                let _ = writeln!(body, "{indent}}}");
            }
        }
    });
    m.push_str(&body);
    let _ = writeln!(m, "    {}\n}}", ret(p));
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disasm::programs;

    // Token builders: register type `ty` (0 r, 1 v, 2 c, 8 oC, 10 s), number `n`.
    fn reg(ty: u32, n: u32) -> u32 {
        0x8000_0000 | ((ty & 7) << 28) | ((ty & 0x18) << 8) | n
    }
    fn dst(ty: u32, n: u32, mask: u32, sat: bool) -> u32 {
        reg(ty, n) | (mask << 16) | ((sat as u32) << 20)
    }
    fn src(ty: u32, n: u32, swz: u32, modifier: u32) -> u32 {
        reg(ty, n) | (swz << 16) | (modifier << 24)
    }
    fn ins(op: u32, params: &[u32]) -> Vec<u32> {
        let mut v = vec![op | ((params.len() as u32) << 24)];
        v.extend_from_slice(params);
        v
    }
    const XYZW: u32 = 0xE4;
    const XXXX: u32 = 0x00;

    fn module(pixel: bool, body: &[Vec<u32>]) -> String {
        let mut t = vec![if pixel { 0xFFFF_0300 } else { 0xFFFE_0300 }];
        // dcl_texcoord0 v0 (pixel) so inputs exist.
        t.extend(ins(31, &[0x8000_0005, dst(1, 0, 0xf, false)]));
        for b in body {
            t.extend(b);
        }
        t.push(0x0000_FFFF);
        let blob: Vec<u8> = t.iter().flat_map(|w| w.to_le_bytes()).collect();
        let p = &programs(&blob)[0];
        let src = translate(p, &Options::default());
        let m = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{}\n{src}", e.emit_to_string(&src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&m)
            .unwrap_or_else(|e| panic!("{}\n{src}", e.emit_to_string(&src)));
        src
    }

    #[test]
    fn masks_swizzles_modifiers_and_saturate() {
        // mad_sat r0.xy, v0, c0.x, -c1  then  mov oC0, r0
        let s = module(
            true,
            &[
                ins(4, &[dst(0, 0, 0b0011, true), src(1, 0, XYZW, 0), src(2, 0, XXXX, 0), src(2, 1, XYZW, 1)]),
                ins(1, &[dst(8, 0, 0xf, false), src(0, 0, XYZW, 0)]),
            ],
        );
        assert!(s.contains("clamp(v0 * vec4<f32>((k.c[0]).x) + (-k.c[1]), vec4<f32>(0.0), vec4<f32>(1.0))"), "{s}");
        assert!(s.contains("r0.x = t.x;") && s.contains("r0.y = t.y;") && !s.contains("r0.z = t.z;"), "{s}");
    }

    #[test]
    fn lrp_and_cmp_argument_order() {
        // lrp r0, c0, c1, c2 = c0 * (c1 - c2) + c2;  cmp r1, c0, c1, c2 = c0 >= 0 ? c1 : c2
        let s = module(
            true,
            &[
                ins(18, &[dst(0, 0, 0xf, false), src(2, 0, XYZW, 0), src(2, 1, XYZW, 0), src(2, 2, XYZW, 0)]),
                ins(88, &[dst(0, 1, 0xf, false), src(2, 0, XYZW, 0), src(2, 1, XYZW, 0), src(2, 2, XYZW, 0)]),
                ins(2, &[dst(8, 0, 0xf, false), src(0, 0, XYZW, 0), src(0, 1, XYZW, 0)]),
            ],
        );
        assert!(s.contains("r0 = mix(k.c[2], k.c[1], k.c[0]);"), "{s}");
        assert!(s.contains("r1 = select(k.c[2], k.c[1], k.c[0] >= vec4<f32>(0.0));"), "{s}");
    }

    #[test]
    fn def_constants_are_inlined() {
        // def c5, 1, 2, 3, 4;  mov oC0, c5.y
        let s = module(
            true,
            &[ins(81, &[dst(2, 5, 0xf, false), 1f32.to_bits(), 2f32.to_bits(), 3f32.to_bits(), 4f32.to_bits()]), ins(1, &[dst(8, 0, 0xf, false), src(2, 5, 0x55, 0)])],
        );
        assert!(s.contains("oC0 = vec4<f32>((vec4<f32>(1.0, 2.0, 3.0, 4.0)).y);"), "{s}");
    }
}

#[cfg(test)]
mod override_tests {
    use super::*;
    use crate::disasm::programs;

    #[test]
    fn constants_and_samplers_can_come_from_the_host() {
        // ps_3_0: dcl_texcoord0 v0; dcl_2d s1; texld r0, v0, s1; mul oC0, r0, c3
        let mut t = vec![0xFFFF_0300u32];
        t.extend([31 | (2 << 24), 0x8000_0005, 0x800F_0000 | (1 << 28)]);
        t.extend([31 | (2 << 24), 0x9000_0000, 0xA00F_0801]);
        t.extend([66 | (3 << 24), 0x800F_0000, 0x90E4_0000, 0xA0E4_0801]);
        t.extend([5 | (3 << 24), 0x800F_0800, 0x80E4_0000, 0xA0E4_0003]);
        t.push(0x0000_FFFF);
        let blob: Vec<u8> = t.iter().flat_map(|w| w.to_le_bytes()).collect();
        let p = &programs(&blob)[0];
        let mut o = Options::default();
        o.prelude = "@group(1) @binding(0) var host_tex: texture_2d<f32>;\n@group(1) @binding(1) var host_samp: sampler;".into();
        o.constants.insert(3, "vec4<f32>(0.5)".into());
        o.samplers.insert(1, ("host_tex".into(), "host_samp".into()));
        let s = translate(p, &o);
        assert!(s.contains("textureSample(host_tex, host_samp, (v0).xy)"), "{s}");
        assert!(s.contains("(vec4<f32>(0.5))"), "{s}");
        assert!(!s.contains("var tex1"), "{s}");
        let m = naga::front::wgsl::parse_str(&s).unwrap_or_else(|e| panic!("{}\n{s}", e.emit_to_string(&s)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&m).unwrap();
    }
}
