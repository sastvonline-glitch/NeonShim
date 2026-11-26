//! Legacy geometry programs ([`HwStage::Gs`]): what their LDS accesses and
//! messages mean.
//!
//! The console runs a geometry program in one hardware stage with the vertex
//! program before it (the ES half, the `Es` program of the vertex
//! container). The ES half writes each vertex's outputs to LDS; the geometry
//! program reads its primitive's vertices from there, at the offsets that
//! arrive in v0 (vertices 0 and 1, 16 bits each) and v1 (2 and 3), in dwords:
//! dword 4r + c of a vertex is register r, channel c of the input signature;
//! v2 holds the primitive id, v3 the invocation id (instancing; refused), v4
//! the offsets of vertices 4 and 5 (adjacency; refused). It writes the
//! vertices it emits to a slot of its own in
//! the GS-VS ring, also in LDS: the slot of lane n starts at n times the slot
//! size past a base the wave shares, vertex after vertex, dword 4r + c of a
//! vertex register r, channel c of the output signature
//! ([`GeometryIo`]). `s_sendmsg` GS messages emit the vertex written last and
//! cut the strip; GS_DONE ends the program.
//!
//! The host runs the ES half as the vertex program it is (the container's VS
//! program, its other compilation) and this program as a geometry shader of
//! one invocation per primitive: a ring read is a load of one input vertex's
//! element, a ring write an output of the vertex it lies in, and the n-th
//! emit sends the n-th vertex. The addresses are only followed to tell which
//! vertex and dword an access reaches: the lane's slot is the invocation's
//! own, and the base its wave shares (read from the driver's table,
//! `resource::DRIVER`) the same for every lane.

use std::collections::HashMap;

use crate::ShaderError;
use crate::gfx10::{Fields, Operand};
use crate::ir::{Inst, Program, Reg, Value};
use crate::native::GeometryIo;
use crate::resource::{Site, inline};

/// A geometry program's ring accesses and messages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rings {
    pub max_vertices: u32,
    pub vertex_dwords: u32,
    /// `ds_read*` of input vertices: per result dword, the vertex and the
    /// dword in it.
    pub reads: Vec<(Site, Vec<(u8, u32)>)>,
    /// `ds_write*` to the invocation's slot: per data dword, the value, the
    /// vertex it lies in and the dword in that vertex.
    pub writes: Vec<(Site, Vec<(Value, u32, u32)>)>,
    /// `s_sendmsg` of stream 0.
    pub messages: Vec<(Site, Message)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Emit,
    Cut,
    /// Emit, then cut.
    EmitCut,
}

/// What an address adds up to: `scale` times a term, plus a value every
/// lane shares and a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Linear {
    term: Option<(Term, u32)>,
    shared: bool,
    constant: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Term {
    /// The lane's index in the wave.
    Lane,
    /// An input vertex's offset in LDS, in dwords.
    Vertex(u8),
    /// A VGPR holding the offsets of vertices 2k (low half) and 2k + 1 (high
    /// half), shifted left by `shift`.
    Packed { k: u8, shift: u32 },
}

const SHARED: Linear = Linear {
    term: None,
    shared: true,
    constant: 0,
};

fn constant(c: u32) -> Linear {
    Linear {
        term: None,
        shared: false,
        constant: c,
    }
}

/// The ring accesses and messages of `p`, a geometry program with output
/// ring `gs`. Refused: an LDS access that is not one of the two rings, a
/// ring write outside the invocation's slot, a stream other than 0, a read
/// of the invocation id.
pub fn raise(p: &Program, gs: &GeometryIo) -> Result<Rings, ShaderError> {
    let unsupported = |offset, what| ShaderError::Unsupported { offset, what };
    if operand_input(p, Reg::V(3)).is_some() {
        return Err(unsupported(0, "a geometry program's invocation id"));
    }
    let mut defs: HashMap<Value, &Inst> = HashMap::new();
    for inst in p.blocks.iter().flat_map(|b| &b.insts) {
        for &(_, v) in &inst.writes {
            defs.insert(v, inst);
        }
    }
    let inputs: HashMap<Value, Reg> = p.inputs.iter().map(|&(r, v)| (v, r)).collect();
    let a = Addresses { p, defs, inputs };
    let slot_bytes = 4 * gs.max_vertices * gs.vertex_dwords;
    let mut rings = Rings {
        max_vertices: gs.max_vertices,
        vertex_dwords: gs.vertex_dwords,
        ..Rings::default()
    };
    for (b, block) in p.blocks.iter().enumerate() {
        for (i, inst) in block.insts.iter().enumerate() {
            let at = inst.machine.offset;
            let name = inst.name();
            match &inst.fields {
                Fields::Ds {
                    dst,
                    addr,
                    data,
                    offset0,
                    offset1,
                    gds,
                } => {
                    let Some(shape) = Shape::of(name).filter(|_| !gds) else {
                        return Err(unsupported(at, "an LDS access of a geometry program"));
                    };
                    let address = match addr {
                        Some(Operand::Vector { reg, .. }) => {
                            inst.read(Reg::V(*reg)).and_then(|v| a.linear(v, 0))
                        }
                        _ => None,
                    };
                    let Some(address) = address else {
                        return Err(unsupported(at, "a geometry program's LDS address"));
                    };
                    // byte offset of each element the instruction moves
                    let element = 4 * shape.dwords;
                    let starts: Vec<u32> = if shape.pair {
                        let unit = element * if shape.wide { 64 } else { 1 };
                        vec![u32::from(*offset0) * unit, u32::from(*offset1) * unit]
                    } else {
                        vec![u32::from(*offset0) | u32::from(*offset1) << 8]
                    };
                    let bytes = starts
                        .iter()
                        .flat_map(|&s| (0..shape.dwords).map(move |j| s + 4 * j))
                        .map(|o| address.constant.wrapping_add(o));
                    if shape.read {
                        let Some((Term::Vertex(k), 4)) = address.term.filter(|_| !address.shared)
                        else {
                            return Err(unsupported(
                                at,
                                "a geometry program's LDS read of no input vertex",
                            ));
                        };
                        if dst.is_none() || bytes.clone().any(|o| !o.is_multiple_of(4)) {
                            return Err(unsupported(
                                at,
                                "a geometry program's LDS read of no input vertex",
                            ));
                        }
                        rings
                            .reads
                            .push(((b, i), bytes.map(|o| (k, o / 4)).collect()));
                    } else {
                        if address.term != Some((Term::Lane, slot_bytes)) {
                            return Err(unsupported(
                                at,
                                "a geometry program's LDS write outside its ring slot",
                            ));
                        }
                        let mut writes = Vec::new();
                        for (n, o) in bytes.enumerate() {
                            let (operand, j) = (n as u32 / shape.dwords, n as u32 % shape.dwords);
                            let value = match data.get(operand as usize) {
                                Some(Operand::Vector { reg, .. }) => {
                                    inst.read(Reg::V(reg + j as u8))
                                }
                                _ => None,
                            };
                            let (Some(value), true) =
                                (value, o.is_multiple_of(4) && o < slot_bytes)
                            else {
                                return Err(unsupported(
                                    at,
                                    "a geometry program's LDS write outside its ring slot",
                                ));
                            };
                            let dword = o / 4;
                            writes.push((
                                value,
                                dword / gs.vertex_dwords,
                                dword % gs.vertex_dwords,
                            ));
                        }
                        rings.writes.push(((b, i), writes));
                    }
                }
                Fields::Sopp { imm } if matches!(name, "s_sendmsg" | "s_sendmsghalt") => {
                    let (msg, op, stream) = (imm & 0xF, imm >> 4 & 7, imm >> 8 & 3);
                    const MSG_GS: u16 = 2;
                    if msg != MSG_GS || op == 0 {
                        continue;
                    }
                    if stream != 0 {
                        return Err(unsupported(at, "a geometry program's stream other than 0"));
                    }
                    let message = match op {
                        1 => Message::Cut,
                        2 => Message::Emit,
                        3 => Message::EmitCut,
                        _ => return Err(unsupported(at, "a geometry program's message")),
                    };
                    rings.messages.push(((b, i), message));
                }
                _ => {}
            }
        }
    }
    Ok(rings)
}

/// The value `reg` holds on entry if an instruction of `p` reads it as an
/// operand (a VALU write keeps the old value of lanes it skips, a read of
/// no use here).
pub fn operand_input(p: &Program, reg: Reg) -> Option<Value> {
    let &(_, v) = p.inputs.iter().find(|i| i.0 == reg)?;
    p.blocks
        .iter()
        .flat_map(|b| &b.insts)
        .any(|i| i.read(reg) == Some(v) && crate::ir::operand_reads(p, i).contains(&reg))
        .then_some(v)
}

/// What a `ds_*` instruction moves.
#[derive(Debug, Clone, Copy)]
struct Shape {
    read: bool,
    /// Dwords of each element.
    dwords: u32,
    /// Two elements, at offset0 and offset1 (in elements).
    pair: bool,
    /// The pair's offsets count 64 elements each.
    wide: bool,
}

impl Shape {
    fn of(name: &str) -> Option<Shape> {
        let (read, rest) = if let Some(r) = name.strip_prefix("ds_read") {
            (true, r)
        } else {
            (false, name.strip_prefix("ds_write")?)
        };
        let (pair, wide, ty) = if let Some(t) = rest.strip_prefix("2st64_") {
            (true, true, t)
        } else if let Some(t) = rest.strip_prefix("2_") {
            (true, false, t)
        } else {
            (false, false, rest.strip_prefix('_')?)
        };
        let dwords = match ty {
            "b32" => 1,
            "b64" => 2,
            "b96" if !pair => 3,
            "b128" if !pair => 4,
            _ => return None,
        };
        Some(Shape {
            read,
            dwords,
            pair,
            wide,
        })
    }
}

/// Follows the VGPR arithmetic of ring addresses.
struct Addresses<'a> {
    p: &'a Program,
    defs: HashMap<Value, &'a Inst>,
    inputs: HashMap<Value, Reg>,
}

/// How deep [`Addresses::linear`] follows definitions.
const MAX_DEPTH: u32 = 16;

impl Addresses<'_> {
    /// `v` as a [`Linear`], if it is one.
    fn linear(&self, v: Value, depth: u32) -> Option<Linear> {
        if depth > MAX_DEPTH {
            return None;
        }
        if let Some(&reg) = self.inputs.get(&v) {
            return match reg {
                Reg::V(k @ (0 | 1)) => Some(Linear {
                    term: Some((Term::Packed { k, shift: 0 }, 1)),
                    shared: false,
                    constant: 0,
                }),
                Reg::S(_) => Some(SHARED),
                _ => None,
            };
        }
        let inst = self.defs.get(&v)?;
        if matches!(inst.fields, Fields::Sop { .. } | Fields::Smem { .. }) {
            // a scalar is the same in every lane
            return Some(self.scalar(v, 0).map_or(SHARED, constant));
        }
        let Fields::Valu(f) = &inst.fields else {
            return None;
        };
        if f.abs != 0 || f.neg != 0 || f.clamp || f.omod != 0 || f.dpp.is_some() {
            return None;
        }
        let sel = |k: usize| f.sdwa.map_or(6, |s| s.src_sel[k]);
        if f.sdwa.is_some_and(|s| s.dst_sel != 6) {
            return None;
        }
        let src = |k: usize| self.operand(inst, f.src.get(k)?, depth + 1);
        let konst = |k: usize| match *f.src.get(k)? {
            Operand::Const(c) => inline(c),
            Operand::Literal(x) => Some(x),
            Operand::Scalar { code, .. } => self.scalar(inst.read(Reg::S(code))?, 0),
            _ => None,
        };
        match inst.name() {
            "v_mov_b32" if sel(0) == 6 => src(0),
            "v_mbcnt_lo_u32_b32" if konst(0) == Some(u32::MAX) && konst(1) == Some(0) => {
                (!self.p.wave64).then_some(Linear {
                    term: Some((Term::Lane, 1)),
                    shared: false,
                    constant: 0,
                })
            }
            "v_mbcnt_hi_u32_b32" if konst(0) == Some(u32::MAX) => {
                let Some(&Operand::Vector { reg, .. }) = f.src.get(1) else {
                    return None;
                };
                let lo = self.defs.get(&inst.read(Reg::V(reg))?)?;
                let Fields::Valu(g) = &lo.fields else {
                    return None;
                };
                let lane = lo.name() == "v_mbcnt_lo_u32_b32"
                    && matches!(g.src[..], [Operand::Const(193), Operand::Const(128)]);
                lane.then_some(Linear {
                    term: Some((Term::Lane, 1)),
                    shared: false,
                    constant: 0,
                })
            }
            "v_mad_u32_u24" | "v_mul_u32_u24" | "v_mul_lo_u32" => {
                let product = match (konst(0), konst(1)) {
                    (Some(k), _) => scale(src(1)?, k)?,
                    (_, Some(k)) => scale(src(0)?, k)?,
                    _ => return None,
                };
                if inst.name() == "v_mad_u32_u24" {
                    add(product, src(2)?)
                } else {
                    Some(product)
                }
            }
            "v_lshlrev_b32" => {
                let shift = konst(0)? & 31;
                let x = match (sel(1), src(1)?) {
                    // a word of a register holding two vertex offsets
                    (
                        4 | 5,
                        Linear {
                            term: Some((Term::Packed { k, shift: 0 }, 1)),
                            shared: false,
                            constant: 0,
                        },
                    ) => vertex(2 * k + u8::from(sel(1) == 5)),
                    (6, x) => x,
                    _ => return None,
                };
                scale(x, 1 << shift)
            }
            "v_lshl_add_u32" => add(scale(src(0)?, 1 << (konst(1)? & 31))?, src(2)?),
            "v_add_nc_u32" | "v_add_u32" | "v_add_co_u32" if sel(0) == 6 && sel(1) == 6 => {
                add(src(0)?, src(1)?)
            }
            "v_and_b32" if sel(0) == 6 && sel(1) == 6 => {
                let (mask, x) = match (konst(0), konst(1)) {
                    (Some(m), _) => (m, src(1)?),
                    (_, Some(m)) => (m, src(0)?),
                    _ => return None,
                };
                match x {
                    // the low offset: the high half shifted past the mask
                    Linear {
                        term: Some((Term::Packed { k, shift }, 1)),
                        shared: false,
                        constant: 0,
                    } if shift < 16 && mask == 0xFFFF << shift => scale(vertex(2 * k), 1 << shift),
                    _ => None,
                }
            }
            "v_bfe_u32" if sel(0) == 6 => match (src(0)?, konst(1)?, konst(2)?) {
                (
                    Linear {
                        term: Some((Term::Packed { k, shift: 0 }, 1)),
                        shared: false,
                        constant: 0,
                    },
                    o @ (0 | 16),
                    16,
                ) => Some(vertex(2 * k + u8::from(o == 16))),
                _ => None,
            },
            "v_lshrrev_b32" if sel(1) == 6 => match (konst(0)?, src(1)?) {
                (
                    16,
                    Linear {
                        term: Some((Term::Packed { k, shift: 0 }, 1)),
                        shared: false,
                        constant: 0,
                    },
                ) => Some(vertex(2 * k + 1)),
                _ => None,
            },
            _ => None,
        }
    }

    /// A VALU operand of `inst`.
    fn operand(&self, inst: &Inst, op: &Operand, depth: u32) -> Option<Linear> {
        match *op {
            Operand::Const(c) => inline(c).map(constant),
            Operand::Literal(x) => Some(constant(x)),
            Operand::Scalar { code, .. } => {
                let v = inst.read(Reg::S(code))?;
                Some(self.scalar(v, 0).map_or(SHARED, constant))
            }
            Operand::Vector { reg, .. } => self.linear(inst.read(Reg::V(reg))?, depth),
            Operand::Attr { .. } => None,
        }
    }

    /// The constant a scalar value holds, through moves.
    fn scalar(&self, v: Value, depth: u32) -> Option<u32> {
        if depth > MAX_DEPTH {
            return None;
        }
        let inst = self.defs.get(&v)?;
        let Fields::Sop { src, .. } = &inst.fields else {
            return None;
        };
        if inst.name() != "s_mov_b32" {
            return None;
        }
        match *src.first()? {
            Operand::Const(c) => inline(c),
            Operand::Literal(x) => Some(x),
            Operand::Scalar { code, .. } => self.scalar(inst.read(Reg::S(code))?, depth + 1),
            _ => None,
        }
    }
}

fn vertex(k: u8) -> Linear {
    Linear {
        term: Some((Term::Vertex(k), 1)),
        shared: false,
        constant: 0,
    }
}

fn scale(x: Linear, k: u32) -> Option<Linear> {
    if k == 1 {
        return Some(x);
    }
    // a register of two offsets only as a whole, shifted
    let term = match x.term {
        Some((Term::Packed { k: pk, shift }, 1)) if k.is_power_of_two() && k > 1 => Some((
            Term::Packed {
                k: pk,
                shift: shift + k.trailing_zeros(),
            },
            1,
        )),
        Some((Term::Packed { .. }, _)) => return None,
        Some((t, s)) => Some((t, s.checked_mul(k)?)),
        None => None,
    };
    Some(Linear {
        term,
        shared: x.shared,
        constant: x.constant.wrapping_mul(k),
    })
}

fn add(a: Linear, b: Linear) -> Option<Linear> {
    let term = match (a.term, b.term) {
        (Some(_), Some(_)) => return None,
        (t, None) | (None, t) => t,
    };
    Some(Linear {
        term,
        shared: a.shared || b.shared,
        constant: a.constant.wrapping_add(b.constant),
    })
}

/// A geometry program of the console's form, written for the tests (wave64):
/// triangles of a position (register 0) and a float4 (register 1) in, the
/// same three vertices out with the float4's x and y swapped, 8 dwords a
/// vertex.
#[cfg(test)]
#[rustfmt::skip]
pub(crate) const SWAP_XY: [u32; 67] = [
    0x9400_FF03, 0x0008_0008, // s_bfe_i32 s0, s3, lit(0x80008)
    0x92FE_8000, // s_bfm_b64 exec, s0, 0
    0xBF06_C000, // s_cmp_eq_u32 s0, 64
    0x85FE_7EC1, // s_cselect_b64 exec, -1, exec
    0x8701_FF09, 0x0000_00FF, // s_and_b32 s1, s9, lit(0xff)
    0xBE80_0308, // s_mov_b32 s0, s8
    0xF400_0000, 0xFA00_00B8, // s_load_dword s0, s[0:1], 0xb8
    0xD765_0002, 0x0001_00C1, // v_mbcnt_lo_u32_b32 v2, -1, 0
    0xD766_0002, 0x0002_04C1, // v_mbcnt_hi_u32_b32 v2, -1, v2
    0x340C_0082, // v_lshlrev_b32 v6, 2, v0
    0x3606_0CFF, 0x0003_FFFC, // v_and_b32 v3, lit(0x3fffc), v6
    0xBE81_0382, // s_mov_b32 s1, 2
    0x3408_00F9, 0x0586_0601, // v_lshlrev_b32 v4, s1, v0 src1_sel:WORD_1
    0x340E_0282, // v_lshlrev_b32 v7, 2, v1
    0x360A_0EFF, 0x0003_FFFC, // v_and_b32 v5, lit(0x3fffc), v7
    0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
    0x8F00_8800, // s_lshl_b32 s0, s0, 8
    0x8100_0200, // s_add_i32 s0, s0, s2
    0xD543_0008, 0x0002_04FF, 0x0000_0060, // v_mad_u32_u24 v8, lit(96), v2, s0
    0xDBFC_0000, 0x0A00_0003, // ds_read_b128 v[10:13], v3
    0xDBFC_0010, 0x0E00_0003, // ds_read_b128 v[14:17], v3 offset:16
    0xDBFC_0000, 0x1200_0004, // ds_read_b128 v[18:21], v4
    0xDBFC_0010, 0x1600_0004, // ds_read_b128 v[22:25], v4 offset:16
    0xDBFC_0000, 0x1A00_0005, // ds_read_b128 v[26:29], v5
    0xDBFC_0010, 0x1E00_0005, // ds_read_b128 v[30:33], v5 offset:16
    0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
    0xDB7C_0000, 0x0000_0A08, // ds_write_b128 v8, v[10:13]
    0xD838_0504, 0x000E_0F08, // ds_write2_b32 v8, v15, v14 offset0:4 offset1:5
    0xD838_0706, 0x0011_1008, // ds_write2_b32 v8, v16, v17 offset0:6 offset1:7
    0xBF90_0022, // s_sendmsg sendmsg(MSG_GS, GS_OP_EMIT, 0)
    0xDB7C_0020, 0x0000_1208, // ds_write_b128 v8, v[18:21] offset:32
    0xD838_0D0C, 0x0016_1708, // ds_write2_b32 v8, v23, v22 offset0:12 offset1:13
    0xD838_0F0E, 0x0019_1808, // ds_write2_b32 v8, v24, v25 offset0:14 offset1:15
    0xBF90_0022, // s_sendmsg sendmsg(MSG_GS, GS_OP_EMIT, 0)
    0xDB7C_0040, 0x0000_1A08, // ds_write_b128 v8, v[26:29] offset:64
    0xD838_1514, 0x001E_1F08, // ds_write2_b32 v8, v31, v30 offset0:20 offset1:21
    0xD838_1716, 0x0021_2008, // ds_write2_b32 v8, v32, v33 offset0:22 offset1:23
    0xBF90_0022, // s_sendmsg sendmsg(MSG_GS, GS_OP_EMIT, 0)
    0xBF90_0012, // s_sendmsg sendmsg(MSG_GS, GS_OP_CUT, 0)
    0xBF8C_0000, // s_waitcnt 0
    0xBF90_0003, // s_sendmsg sendmsg(MSG_GS_DONE, GS_OP_NOP)
    0xBF81_0000, // s_endpgm
];

/// [`SWAP_XY`]'s ring: three vertices of two registers.
#[cfg(test)]
pub(crate) const SWAP_XY_RING: GeometryIo = GeometryIo {
    max_vertices: 3,
    vertex_dwords: 8,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{HwStage, NativeProgram, StageIo};

    fn program(words: &[u32]) -> Program {
        let native = NativeProgram {
            stage: HwStage::Gs,
            wave64: true,
            vgprs: 40,
            layout: vec![],
            used: vec![],
            io: StageIo::Gs(SWAP_XY_RING),
            code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
        };
        crate::spill::build(&native).unwrap()
    }

    /// Each read names its vertex (v0's low and high halves, v1's low half)
    /// and dwords; each write the vertex and dword of the slot it reaches,
    /// with the value it writes; the messages emit three vertices and cut.
    #[test]
    fn the_rings_of_a_geometry_program() {
        let p = program(&SWAP_XY);
        let rings = raise(&p, &SWAP_XY_RING).unwrap();
        let inst = |s: Site| &p.blocks[s.0].insts[s.1];
        let four = |v: u8, from: u32| (from..from + 4).map(|d| (v, d)).collect::<Vec<_>>();
        let reads: Vec<(usize, Vec<(u8, u32)>)> = rings
            .reads
            .iter()
            .map(|(s, d)| (inst(*s).machine.offset, d.clone()))
            .collect();
        assert_eq!(
            reads,
            [
                (0x74, four(0, 0)),
                (0x7C, four(0, 4)),
                (0x84, four(1, 0)),
                (0x8C, four(1, 4)),
                (0x94, four(2, 0)),
                (0x9C, four(2, 4)),
            ]
        );
        let writes: Vec<(usize, Vec<(u32, u32)>)> = rings
            .writes
            .iter()
            .map(|(s, w)| {
                (
                    inst(*s).machine.offset,
                    w.iter().map(|x| (x.1, x.2)).collect(),
                )
            })
            .collect();
        let at = |v: u32, dwords: std::ops::Range<u32>| dwords.map(|d| (v, d)).collect::<Vec<_>>();
        assert_eq!(
            writes,
            [
                (0xA8, at(0, 0..4)),
                (0xB0, at(0, 4..6)),
                (0xB8, at(0, 6..8)),
                (0xC4, at(1, 0..4)),
                (0xCC, at(1, 4..6)),
                (0xD4, at(1, 6..8)),
                (0xE0, at(2, 0..4)),
                (0xE8, at(2, 4..6)),
                (0xF0, at(2, 6..8)),
            ]
        );
        // Vertex 0's dword 4 is what the read of its float4 put in v15 (its y).
        let read = rings.reads[1].0;
        assert_eq!(Some(rings.writes[1].1[0].0), inst(read).write(Reg::V(15)));
        let messages: Vec<(usize, Message)> = rings
            .messages
            .iter()
            .map(|(s, m)| (inst(*s).machine.offset, *m))
            .collect();
        assert_eq!(
            messages,
            [
                (0xC0, Message::Emit),
                (0xDC, Message::Emit),
                (0xF8, Message::Emit),
                (0xFC, Message::Cut)
            ]
        );
    }

    /// Refused: a write past the invocation's slot (a slot of 64 bytes where
    /// the ring's has 96), a read at an address no input vertex gives, a
    /// stream other than 0, the invocation id read.
    #[test]
    fn ring_accesses_of_no_vertex_are_refused() {
        let refused = |at: usize, word: u32| {
            let mut words = SWAP_XY;
            words[at] = word;
            raise(&program(&words), &SWAP_XY_RING).is_err()
        };
        assert!(refused(28, 0x0000_0040), "slot of 64 bytes");
        assert!(refused(30, 0x0A00_0002), "read at the lane's index");
        assert!(refused(48, 0xBF90_0122), "stream 1");
        assert!(refused(14, 0x340C_0682), "invocation id (v3)");
        assert!(!refused(65, 0xBF90_0003), "unchanged");
    }
}
