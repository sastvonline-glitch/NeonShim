//! Loops whose number of passes the code fixes: a scalar counter that
//! starts at a constant, moves by a constant step each pass and is compared
//! with a constant where the loop decides to go round again. The console
//! runs such a loop to its end whatever the data, and so must the host: the
//! iteration limit of translated loops (`lower::PIXEL_LOOP_LIMIT`) is for loops
//! whose end the data decides.

use std::collections::{HashMap, HashSet};

use crate::gfx10::{Fields, Operand};
use crate::ir::{Exit, Program, Reg, Value};

/// Passes past which a counted loop is left to the stage's limit: more is
/// no loop a frame of the title runs per lane.
const MOST: u32 = 1 << 16;

/// How a counter value is made.
enum Def<'a> {
    /// A join: block and phi index.
    Phi(usize, usize),
    /// An instruction.
    Inst(&'a crate::ir::Inst),
}

/// The number of times loop `header` (a block with a back edge into it,
/// `rpo` its blocks' reverse-postorder numbers) goes round again, when its
/// code fixes it ([`self`]) at [`MOST`] or fewer; None otherwise. With one
/// back edge, from a latch whose branch on SCC either goes back or leaves
/// the loop, or from a latch that always goes back to a header whose
/// branch on SCC stays in the loop or leaves it.
pub(super) fn constant_passes(p: &Program, rpo: &[usize], header: usize) -> Option<u32> {
    let latches: Vec<usize> = p.blocks[header]
        .preds
        .iter()
        .copied()
        .filter(|&q| rpo[q] != usize::MAX && rpo[header] <= rpo[q])
        .collect();
    let [latch] = latches[..] else {
        return None;
    };
    let body = body(p, header, latch);
    // The block that decides, and which way of its branch stays in the loop.
    let (test, stay) = match p.blocks[latch].exit {
        Exit::Branch { taken, not_taken } if taken == header && !body.contains(&not_taken) => {
            (latch, true)
        }
        Exit::Branch { taken, not_taken } if not_taken == header && !body.contains(&taken) => {
            (latch, false)
        }
        Exit::Jump(_) => match p.blocks[header].exit {
            Exit::Branch { taken, not_taken }
                if body.contains(&taken) && !body.contains(&not_taken) =>
            {
                (header, true)
            }
            Exit::Branch { taken, not_taken }
                if body.contains(&not_taken) && !body.contains(&taken) =>
            {
                (header, false)
            }
            _ => return None,
        },
        _ => return None,
    };
    let defs = definitions(p);
    // The branch: on SCC, set (`s_cbranch_scc1`) or clear.
    let branch = p.blocks[test].insts.last()?;
    let set = match branch.name() {
        "s_cbranch_scc1" => true,
        "s_cbranch_scc0" => false,
        _ => return None,
    };
    let scc = branch.read(Reg::Scc)?;
    let Some(Def::Inst(cmp)) = defs.get(&scc) else {
        return None;
    };
    let (op, a, b) = compare(cmp)?;
    // One side a counter of the header's, the other a constant.
    let (counter, k, swapped) = match (a, b) {
        (Side::Value(v), Side::Const(k)) => (v, k, false),
        (Side::Const(k), Side::Value(v)) => (v, k, true),
        _ => return None,
    };
    let (phi, offset) = trace(p, &defs, counter)?;
    let Some(&Def::Phi(pb, pi)) = defs.get(&phi) else {
        return None;
    };
    if pb != header {
        return None;
    }
    // Its start from outside the loop, its step from the latch.
    let join = &p.blocks[header].phis[pi];
    let (mut start, mut step) = (None, None);
    for (&q, &arg) in p.blocks[header].preds.iter().zip(&join.args) {
        if rpo[q] == usize::MAX {
            continue;
        }
        if q == latch {
            let (from, by) = trace(p, &defs, arg)?;
            (from == phi).then_some(())?;
            step = Some(by);
        } else {
            let c = constant(&defs, arg)?;
            if start.is_some_and(|s| s != c) {
                return None;
            }
            start = Some(c);
        }
    }
    let (start, step) = (start?, step?);
    if step == 0 {
        return None;
    }
    // Pass n compares start + n step + offset; the loop goes round again
    // while the branch takes its staying side.
    let mut x = start.wrapping_add(offset);
    for n in 0..=MOST {
        let holds = if swapped {
            op.holds(k, x)
        } else {
            op.holds(x, k)
        };
        // The branch is taken when SCC is what it tests for.
        let taken = holds == set;
        if taken != stay {
            return Some(n);
        }
        x = x.wrapping_add(step);
    }
    None
}

/// The blocks of the loop of `header` closed by `latch`: those that reach the
/// latch without passing the header.
fn body(p: &Program, header: usize, latch: usize) -> HashSet<usize> {
    let mut body = HashSet::from([header, latch]);
    let mut work = vec![latch];
    while let Some(b) = work.pop() {
        if b == header {
            continue;
        }
        for &q in &p.blocks[b].preds {
            if body.insert(q) {
                work.push(q);
            }
        }
    }
    body
}

/// Where each value is made.
fn definitions(p: &Program) -> HashMap<Value, Def<'_>> {
    let mut defs = HashMap::new();
    for (b, block) in p.blocks.iter().enumerate() {
        for (i, phi) in block.phis.iter().enumerate() {
            defs.insert(phi.value, Def::Phi(b, i));
        }
        for inst in &block.insts {
            for &(_, v) in &inst.writes {
                defs.insert(v, Def::Inst(inst));
            }
        }
    }
    defs
}

/// A compare's operand: a scalar value, or a constant.
enum Side {
    Value(Value),
    Const(u32),
}

#[derive(Clone, Copy)]
enum Op {
    Eq,
    Ne,
    Gt(bool),
    Ge(bool),
    Lt(bool),
    Le(bool),
}

impl Op {
    /// `a op b` (the flag: signed).
    fn holds(self, a: u32, b: u32) -> bool {
        let ord = |signed: bool| {
            if signed {
                (a as i32).cmp(&(b as i32))
            } else {
                a.cmp(&b)
            }
        };
        match self {
            Op::Eq => a == b,
            Op::Ne => a != b,
            Op::Gt(s) => ord(s).is_gt(),
            Op::Ge(s) => ord(s).is_ge(),
            Op::Lt(s) => ord(s).is_lt(),
            Op::Le(s) => ord(s).is_le(),
        }
    }
}

/// A 32-bit scalar compare (`s_cmp_*`, `s_cmpk_*`): its test and operands.
fn compare(inst: &crate::ir::Inst) -> Option<(Op, Side, Side)> {
    let name = inst.name();
    let (rest, k) = match (name.strip_prefix("s_cmpk_"), name.strip_prefix("s_cmp_")) {
        (Some(rest), _) => (rest, true),
        (None, Some(rest)) => (rest, false),
        _ => return None,
    };
    let (test, ty) = rest.split_once('_')?;
    let signed = match ty {
        "i32" => true,
        "u32" => false,
        _ => return None,
    };
    let op = match test {
        "eq" => Op::Eq,
        "lg" => Op::Ne,
        "gt" => Op::Gt(signed),
        "ge" => Op::Ge(signed),
        "lt" => Op::Lt(signed),
        "le" => Op::Le(signed),
        _ => return None,
    };
    let Fields::Sop { src, imm, .. } = &inst.fields else {
        return None;
    };
    let side = |op: &Operand| match *op {
        Operand::Scalar { code, dwords: 1 } => inst.read(Reg::S(code)).map(Side::Value),
        Operand::Const(c) => crate::resource::inline(c).map(Side::Const),
        Operand::Literal(l) => Some(Side::Const(l)),
        _ => None,
    };
    if k {
        // SOPK: the register against the 16-bit immediate, sign-extended
        // for the signed tests.
        let imm = (*imm)?;
        let b = if signed {
            imm as i16 as i32 as u32
        } else {
            u32::from(imm)
        };
        Some((op, side(src.first()?)?, Side::Const(b)))
    } else {
        Some((op, side(src.first()?)?, side(src.get(1)?)?))
    }
}

/// The constant `v` holds, through moves and joins of one value.
fn constant(defs: &HashMap<Value, Def<'_>>, v: Value) -> Option<u32> {
    let mut v = v;
    for _ in 0..64 {
        match defs.get(&v)? {
            Def::Inst(inst) if inst.name() == "s_mov_b32" => {
                let Fields::Sop { src, .. } = &inst.fields else {
                    return None;
                };
                match *src.first()? {
                    Operand::Const(c) => return crate::resource::inline(c),
                    Operand::Literal(l) => return Some(l),
                    Operand::Scalar { code, dwords: 1 } => v = inst.read(Reg::S(code))?,
                    _ => return None,
                }
            }
            Def::Inst(inst) if inst.name() == "s_movk_i32" => {
                let Fields::Sop { imm, .. } = &inst.fields else {
                    return None;
                };
                return Some((*imm)? as i16 as i32 as u32);
            }
            _ => return None,
        }
    }
    None
}

/// `v` as a header's counter plus a constant: the value it started from
/// (a join that is not one of a single value) and what moves, adds and
/// subtractions of constants added to it.
fn trace(p: &Program, defs: &HashMap<Value, Def<'_>>, v: Value) -> Option<(Value, u32)> {
    let (mut v, mut sum) = (v, 0u32);
    for _ in 0..256 {
        match defs.get(&v)? {
            Def::Phi(b, i) => {
                // A join of one value (and of itself) is that value.
                let mut others = p.blocks[*b].phis[*i]
                    .args
                    .iter()
                    .copied()
                    .filter(|&a| a != v);
                match others.next() {
                    Some(one) if others.all(|a| a == one) => v = one,
                    _ => return Some((v, sum)),
                }
            }
            Def::Inst(inst) => {
                let Fields::Sop { src, imm, .. } = &inst.fields else {
                    return None;
                };
                let reg = |op: &Operand| match *op {
                    Operand::Scalar { code, dwords: 1 } => inst.read(Reg::S(code)),
                    _ => None,
                };
                let konst = |op: &Operand| match *op {
                    Operand::Const(c) => crate::resource::inline(c),
                    Operand::Literal(l) => Some(l),
                    _ => None,
                };
                match inst.name() {
                    "s_mov_b32" => v = reg(src.first()?)?,
                    "s_addk_i32" => {
                        // SOPK: the destination is the source.
                        let dst = inst.writes.iter().find(|w| matches!(w.0, Reg::S(_)))?.0;
                        v = inst.read(dst)?;
                        sum = sum.wrapping_add((*imm)? as i16 as i32 as u32);
                    }
                    name @ ("s_add_u32" | "s_add_i32" | "s_sub_u32" | "s_sub_i32") => {
                        let (a, b) = (src.first()?, src.get(1)?);
                        let sub = name.starts_with("s_sub");
                        let (from, k) = match (reg(a), konst(b), konst(a), reg(b)) {
                            (Some(x), Some(k), _, _) => (x, k),
                            // (`k - x` is no step of `x`.)
                            (_, _, Some(k), Some(x)) if !sub => (x, k),
                            _ => return None,
                        };
                        v = from;
                        sum = if sub {
                            sum.wrapping_sub(k)
                        } else {
                            sum.wrapping_add(k)
                        };
                    }
                    _ => return None,
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::constant_passes;

    const S_ENDPGM: u32 = 0xBF81_0000;

    /// The passes of each loop of `words` (wave32), by header in code order.
    fn passes(words: &[u32]) -> Vec<Option<u32>> {
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let p = crate::ir::build(&code, false, 16).unwrap();
        let order = crate::ir::reverse_postorder(&p.blocks);
        let mut rpo = vec![usize::MAX; p.blocks.len()];
        for (n, &b) in order.iter().enumerate() {
            rpo[b] = n;
        }
        let mut headers: Vec<usize> = (0..p.blocks.len())
            .filter(|&b| rpo[b] != usize::MAX)
            .filter(|&b| {
                p.blocks[b]
                    .preds
                    .iter()
                    .any(|&q| rpo[q] != usize::MAX && rpo[b] <= rpo[q])
            })
            .collect();
        headers.sort_by_key(|&h| p.blocks[h].offset);
        headers
            .iter()
            .map(|&h| constant_passes(&p, &rpo, h))
            .collect()
    }

    #[test]
    fn a_counter_compared_at_the_bottom_goes_round_until_it_meets_its_constant() {
        // s6 from 0 by 1 while it is not 5: five passes, four of them again.
        let words = [
            0xBE86_0380, // s_mov_b32 s6, 0
            0x8106_8106, // s_add_i32 s6, s6, 1
            0xBF07_8506, // s_cmp_lg_u32 s6, 5
            0xBF85_FFFD, // s_cbranch_scc1 (back)
            S_ENDPGM,
        ];
        assert_eq!(passes(&words), [Some(4)]);
    }

    #[test]
    fn the_environment_brdf_count_of_four_samples_a_pass_is_found() {
        // The title's tables: s2 from 0 by 4 (four adds of 1) until it is
        // 1,024 (`s_cmpk_eq_i32`, back on SCC clear): 256 passes.
        let words = [
            0xBE82_0380, // s_mov_b32 s2, 0
            0x8102_8102, // s_add_i32 s2, s2, 1
            0x8102_8102,
            0x8102_8102,
            0x8102_8102,
            0xB182_0400, // s_cmpk_eq_i32 s2, 0x400
            0xBF84_FFFA, // s_cbranch_scc0 (back)
            S_ENDPGM,
        ];
        assert_eq!(passes(&words), [Some(255)]);
    }

    #[test]
    fn a_counter_tested_at_the_top_counts_the_passes_through_the_body() {
        // while (s6 < 10) s6 += 2, from 1: s6 is 1, 3, 5, 7, 9 in the body.
        let words = [
            0xBE86_0381, // s_mov_b32 s6, 1
            0xBF0A_8A06, // s_cmp_lt_u32 s6, 10
            0xBF84_0002, // s_cbranch_scc0 (out)
            0x8106_8206, // s_add_i32 s6, s6, 2
            0xBF82_FFFC, // s_branch (back)
            S_ENDPGM,
        ];
        assert_eq!(passes(&words), [Some(5)]);
    }

    #[test]
    fn a_count_the_data_bounds_or_that_never_meets_its_constant_is_not_fixed() {
        // Until s6 is s0 (a user SGPR): the data decides.
        let by_data = [
            0xBE86_0380, // s_mov_b32 s6, 0
            0x8106_8106, // s_add_i32 s6, s6, 1
            0xBF07_0006, // s_cmp_lg_u32 s6, s0
            0xBF85_FFFD, // s_cbranch_scc1 (back)
            S_ENDPGM,
        ];
        assert_eq!(passes(&by_data), [None]);
        // From 1 by 2 until it is 64: odd values never are (not within the
        // passes a fixed count may have).
        let never = [
            0xBE86_0381, // s_mov_b32 s6, 1
            0x8106_8206, // s_add_i32 s6, s6, 2
            0xBF07_C006, // s_cmp_lg_u32 s6, 64
            0xBF85_FFFD, // s_cbranch_scc1 (back)
            S_ENDPGM,
        ];
        assert_eq!(passes(&never), [None]);
    }
}
