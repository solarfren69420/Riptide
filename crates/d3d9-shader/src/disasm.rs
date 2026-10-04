//! Direct3D 9 shader bytecode (shader model 2 and 3), as embedded in H2Overdrive's `shad4`
//! effects: find the programs in a blob, read their constant tables (`CTAB`: which register holds
//! which named input) and disassemble them.
//!
//! A program is a stream of 32-bit tokens: a version token (`0xFFFE0300` vertex shader 3.0,
//! `0xFFFF0300` pixel shader 3.0), instructions, `0x0000FFFF` at the end. An instruction token
//! holds the opcode (bits 0-15) and, from SM2 on, its parameter count (bits 24-27); comments
//! (`0xFFFE`) carry their length in bits 16-30 and hold the constant table.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Program {
    /// Byte offset of the version token in the blob.
    pub offset: usize,
    pub pixel: bool,
    pub major: u32,
    pub minor: u32,
    pub tokens: Vec<u32>,
    /// Constant table: (register set, first register) -> (name, register count).
    pub constants: Vec<Constant>,
}

#[derive(Debug, Clone)]
pub struct Constant {
    pub name: String,
    /// 0 bool, 1 int4, 2 float4, 3 sampler.
    pub set: u16,
    pub index: u16,
    pub count: u16,
}

/// Every shader program in `blob` (found by their version tokens, read to their end tokens).
pub fn programs(blob: &[u8]) -> Vec<Program> {
    let words: Vec<u32> = blob.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let (kind, major, minor) = (w >> 16, (w >> 8) & 0xff, w & 0xff);
        if (kind == 0xFFFE || kind == 0xFFFF) && (2..=3).contains(&major) && minor <= 1 {
            if let Some(len) = program_len(&words[i..]) {
                let tokens = words[i..=i + len].to_vec();
                let constants = constant_table(&tokens);
                out.push(Program { offset: i * 4, pixel: kind == 0xFFFF, major, minor, tokens, constants });
                i += len + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn constant_table(tokens: &[u32]) -> Vec<Constant> {
    let mut i = 1;
    while i < tokens.len() {
        let t = tokens[i];
        if t & 0xffff == 0xFFFE {
            let len = ((t >> 16) & 0x7fff) as usize;
            let body: Vec<u8> = tokens[i + 1..(i + 1 + len).min(tokens.len())].iter().flat_map(|w| w.to_le_bytes()).collect();
            if body.starts_with(b"CTAB") {
                return parse_ctab(&body[4..]);
            }
            i += len + 1;
        } else {
            i += 1;
        }
    }
    Vec::new()
}

fn parse_ctab(b: &[u8]) -> Vec<Constant> {
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()));
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes(s.try_into().unwrap()));
    let cstr = |o: usize| b.get(o..).map(|s| String::from_utf8_lossy(&s[..s.iter().position(|c| *c == 0).unwrap_or(s.len())]).into_owned());
    let (Some(count), Some(info)) = (u32_at(12), u32_at(16)) else { return Vec::new() };
    (0..count as usize)
        .filter_map(|k| {
            let e = info as usize + k * 20;
            Some(Constant { name: cstr(u32_at(e)? as usize)?, set: u16_at(e + 4)?, index: u16_at(e + 6)?, count: u16_at(e + 8)? })
        })
        .collect()
}

const OPCODES: &[(u32, &str)] = &[
    (0, "nop"), (1, "mov"), (2, "add"), (3, "sub"), (4, "mad"), (5, "mul"), (6, "rcp"), (7, "rsq"), (8, "dp3"), (9, "dp4"),
    (10, "min"), (11, "max"), (12, "slt"), (13, "sge"), (14, "exp"), (15, "log"), (16, "lit"), (17, "dst"), (18, "lrp"),
    (19, "frc"), (20, "m4x4"), (21, "m4x3"), (22, "m3x4"), (23, "m3x3"), (24, "m3x2"), (25, "call"), (26, "callnz"),
    (27, "loop"), (28, "ret"), (29, "endloop"), (30, "label"), (31, "dcl"), (32, "pow"), (33, "crs"), (34, "sgn"),
    (35, "abs"), (36, "nrm"), (37, "sincos"), (38, "rep"), (39, "endrep"), (40, "if"), (41, "ifc"), (42, "else"),
    (43, "endif"), (44, "break"), (45, "breakc"), (46, "mova"), (47, "defb"), (48, "defi"), (64, "texcoord"),
    (65, "texkill"), (66, "texld"), (78, "expp"), (79, "logp"), (80, "cnd"), (81, "def"), (88, "cmp"), (89, "bem"),
    (90, "dp2add"), (91, "dsx"), (92, "dsy"), (93, "texldd"), (94, "setp"), (95, "texldl"), (96, "breakp"),
];

/// Instructions whose first parameter is a source, not a destination.
pub(crate) const NO_DEST: &[u32] = &[25, 26, 27, 28, 29, 30, 38, 39, 40, 41, 42, 43, 44, 45, 65, 96];

pub fn opcode_name(op: u32) -> &'static str {
    OPCODES.iter().find(|(o, _)| *o == op).map_or("?", |(_, n)| n)
}

pub(crate) fn reg_type(t: u32) -> u32 {
    ((t >> 28) & 7) | ((t >> 8) & 0x18)
}

/// A register as text: `r3`, `c12`, `v0`, `s1`, `oC0` ...
pub fn reg_name(t: u32, pixel: bool) -> String {
    let n = t & 0x7ff;
    match reg_type(t) {
        0 => format!("r{n}"),
        1 => format!("v{n}"),
        2 => format!("c{n}"),
        3 => if pixel { format!("t{n}") } else { format!("a{n}") },
        4 => ["oPos", "oFog", "oPts"].get(n as usize).copied().unwrap_or("oR?").to_string(),
        5 => format!("oD{n}"),
        6 => format!("o{n}"),
        7 => format!("i{n}"),
        8 => format!("oC{n}"),
        9 => "oDepth".into(),
        10 => format!("s{n}"),
        14 => format!("b{n}"),
        15 => "aL".into(),
        17 => ["vPos", "vFace"].get(n as usize).copied().unwrap_or("misc?").to_string(),
        19 => format!("p{n}"),
        x => format!("reg{x}_{n}"),
    }
}

fn swizzle(t: u32) -> String {
    let s = (t >> 16) & 0xff;
    if s == 0xE4 {
        return String::new();
    }
    let c: Vec<char> = (0..4).map(|k| b"xyzw"[((s >> (2 * k)) & 3) as usize] as char).collect();
    if c.iter().all(|x| *x == c[0]) {
        format!(".{}", c[0])
    } else {
        format!(".{}", c.iter().collect::<String>())
    }
}

fn mask(t: u32) -> String {
    let m = (t >> 16) & 0xf;
    if m == 0xf {
        String::new()
    } else {
        format!(".{}", (0..4).filter(|k| m & (1 << k) != 0).map(|k| b"xyzw"[k] as char).collect::<String>())
    }
}

fn source(t: u32, rel: Option<u32>, pixel: bool) -> String {
    let base = reg_name(t, pixel);
    let base = match rel {
        Some(r) => format!("{}[{}{}+{}]", &base[..1], reg_name(r, pixel), swizzle(r), t & 0x7ff),
        None => base,
    };
    let s = format!("{base}{}", swizzle(t));
    match (t >> 24) & 0xf {
        0 => s,
        1 => format!("-{s}"),
        2 => format!("{s}_bias"),
        3 => format!("-{s}_bias"),
        4 => format!("{s}_bx2"),
        5 => format!("-{s}_bx2"),
        6 => format!("1-{s}"),
        7 => format!("{s}_x2"),
        8 => format!("-{s}_x2"),
        9 => format!("{s}_dz"),
        10 => format!("{s}_dw"),
        11 => format!("|{s}|"),
        12 => format!("-|{s}|"),
        13 => format!("!{s}"),
        _ => s,
    }
}

pub(crate) const USAGES: &[&str] = &["position", "blendweight", "blendindices", "normal", "psize", "texcoord", "tangent", "binormal", "tessfactor", "positiont", "color", "fog", "depth", "sample"];

/// Assembly text for `p`, one instruction per line; constant registers are annotated with their
/// names from the constant table.
pub fn disassemble(p: &Program) -> String {
    let mut names: HashMap<(u16, u16), String> = HashMap::new();
    for c in &p.constants {
        for k in 0..c.count.max(1) {
            let name = if c.count > 1 { format!("{}[{k}]", c.name) } else { c.name.clone() };
            names.insert((c.set, c.index + k), name);
        }
    }
    let mut out = format!("{}_{}_{}\n", if p.pixel { "ps" } else { "vs" }, p.major, p.minor);
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
        let params = &t[(i + 1).min(t.len())..(i + 1 + len).min(t.len())];
        let mut line = opcode_name(op).to_string();
        if op == 66 && p.major >= 2 && (w >> 16) & 0xff == 1 {
            line.push('p');
        }
        match op {
            31 => {
                // dcl: usage token, then the register.
                let (u, r) = (params.first().copied().unwrap_or(0), params.get(1).copied().unwrap_or(0));
                if reg_type(r) == 10 {
                    line = format!("dcl_{} {}", ["?", "?", "2d", "cube", "volume"].get(((u >> 27) & 0xf) as usize).unwrap_or(&"?"), reg_name(r, p.pixel));
                } else {
                    let usage = USAGES.get((u & 0x1f) as usize).unwrap_or(&"?");
                    line = format!("dcl_{usage}{} {}{}", (u >> 16) & 0xf, reg_name(r, p.pixel), mask(r));
                }
            }
            81 => {
                let v: Vec<String> = params[1..].iter().map(|x| format!("{}", f32::from_bits(*x))).collect();
                line = format!("def {}, {}", reg_name(params[0], p.pixel), v.join(", "));
            }
            47 | 48 => {
                let v: Vec<String> = params[1..].iter().map(|x| (*x as i32).to_string()).collect();
                line = format!("{} {}, {}", opcode_name(op), reg_name(params[0], p.pixel), v.join(", "));
            }
            _ => {
                let mut args = Vec::new();
                let mut k = 0;
                let mut first = true;
                while k < params.len() {
                    let tok = params[k];
                    let rel = (tok & 0x2000 != 0 && !first || tok & 0x2000 != 0 && NO_DEST.contains(&op)).then(|| params.get(k + 1).copied()).flatten();
                    if first && !NO_DEST.contains(&op) {
                        let sat = if (tok >> 20) & 1 != 0 { "_sat" } else { "" };
                        line.push_str(sat);
                        args.push(format!("{}{}", reg_name(tok, p.pixel), mask(tok)));
                    } else {
                        let mut s = source(tok, rel, p.pixel);
                        if reg_type(tok) == 2 {
                            if let Some(n) = names.get(&(2, (tok & 0x7ff) as u16)) {
                                s = format!("{s}/*{n}*/");
                            }
                        } else if reg_type(tok) == 10 {
                            if let Some(n) = names.get(&(3, (tok & 0x7ff) as u16)) {
                                s = format!("{s}/*{n}*/");
                            }
                        } else if reg_type(tok) == 14 {
                            if let Some(n) = names.get(&(0, (tok & 0x7ff) as u16)) {
                                s = format!("{s}/*{n}*/");
                            }
                        } else if reg_type(tok) == 7 {
                            if let Some(n) = names.get(&(1, (tok & 0x7ff) as u16)) {
                                s = format!("{s}/*{n}*/");
                            }
                        }
                        args.push(s);
                    }
                    k += if rel.is_some() { 2 } else { 1 };
                    first = false;
                }
                if !args.is_empty() {
                    line = format!("{line} {}", args.join(", "));
                }
            }
        }
        out.push_str("    ");
        out.push_str(&line);
        out.push('\n');
        i += len + 1;
    }
    out
}

/// Index of the end token of the program starting at `w[0]`, walking comments (by their length:
/// the preshader comment holds its own end token) and instructions (by their parameter count).
fn program_len(w: &[u32]) -> Option<usize> {
    let mut i = 1;
    while i < w.len() {
        let t = w[i];
        match t & 0xffff {
            0xFFFF => return Some(i),
            0xFFFE => i += ((t >> 16) & 0x7fff) as usize + 1,
            0xFFFD => i += 1,
            _ => {
                if t & 0x8000_0000 != 0 {
                    // Not an instruction token: this was not a program after all.
                    return None;
                }
                i += ((t >> 24) & 0xf) as usize + 1;
            }
        }
    }
    None
}

/// The constant table as text, one `set index count name` line per constant.
pub fn constant_listing(p: &Program) -> String {
    let set = ["bool", "int", "float", "sampler"];
    p.constants.iter().map(|c| format!("{:<7} {:>3} x{:<2} {}\n", set.get(c.set as usize).unwrap_or(&"?"), c.index, c.count, c.name)).collect()
}
