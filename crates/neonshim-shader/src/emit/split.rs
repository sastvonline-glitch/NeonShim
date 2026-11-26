//! A console wave of 64 lanes as two host waves of 32 that never meet, where that is exact:
//! the cheaper forms tried before the forms that exchange the halves' values through group
//! barriers ([`super::forms::EXCHANGE`]) or run both halves in one host wave
//! ([`super::forms::PAIRED`]).

use std::collections::{HashMap, HashSet};

use super::{HostShader, emit_program, forms, idiom, lower};
use crate::dxbc::Container;
use crate::gfx10::{Fields, Operand};
use crate::ir::{Inst, Program, Reg, Value};
use crate::native::NativeProgram;
use crate::structure::{Bool, Meets, Structured};
use crate::{ShaderError, io, resource, structure};

type Emitted = Result<(HostShader, Option<u32>), ShaderError>;

/// How a VGPR value is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    /// As its own lane's source.
    Source,
    /// Only as the old value of the register an instruction overwrites under its guard: the
    /// lanes outside the guard keep it.
    Keep,
    /// As a source and as the old value of the register an instruction writes (an
    /// accumulator, a write of part of the register, the register among its sources).
    Both,
    /// At a join, from an earlier block (not around a loop): the join's value.
    Join(Value),
    /// By the first lane that runs the reader under its guard (`v_readfirstlane_b32`). A host
    /// wave with no lane of the guard reads a lane outside it, where the console reads the
    /// other half's first lane: the idiom's first-lane reads, whose results only the lanes of
    /// the guard act on in the allocation it proves ([`idiom::wave_local`]'s shortcut).
    First,
    /// By a whole-wave section's select of the lanes that ran (or did not run) before it, in
    /// the lanes where the mask is (is not) set, whatever their guard.
    Masked(Bool, bool),
    /// From other lanes (DPP, a lane read or write, a swizzle, a permute, a whole-wave
    /// section's other steps), or at a loop's join.
    Other,
}

/// Whether `inst` overwrites the whole of register `r` in each lane of its guard without
/// reading it: a plain vector operation whose sources do not name `r`.
fn overwrites(inst: &Inst, r: u8) -> bool {
    let Fields::Valu(v) = &inst.fields else { return false };
    let name = inst.name();
    let names = |op: &Operand| matches!(*op, Operand::Vector { reg, dwords } if reg <= r && r < reg + dwords.max(1));
    v.dpp.is_none()
        && v.sdwa.is_none()
        && v.opsel & 8 == 0
        && !name.contains("16")
        && !name.contains("mac")
        && !name.contains("mix")
        && !name.contains("dot")
        && !name.contains("lane")
        && !name.starts_with("v_movrel")
        && !name.starts_with("v_swap")
        && !v.src.iter().any(names)
}

/// Steps [`read_inside_only`] takes from one write before it gives up (and takes the write's
/// old lanes as read).
const MAX_STEPS: usize = 4096;

/// The VGPR values written under a guard whose old lanes no lane reads: where the lanes outside
/// the guard keep what the register held (a wave-wide count, say), no reader takes it there.
/// For [`idiom::wave_local`], which otherwise takes such a write as mixing that old value in.
///
/// A guarded write holds the old value in the lanes outside its guard and outside the guards
/// of the writes it overwrote in a row: a chain of guards, which a write of the whole register
/// in each lane of its guard adds its guard to, and a write of part of it (or one that also
/// reads it) keeps as it is. Followed from the write: a reader that reads it as its own
/// lane's source, under a guard that some lane entering the reader's block meets outside
/// every guard of the chain, reads the old lanes; so does a reader of other lanes and a loop's
/// join. A later guarded write that keeps it, and a join from an earlier block, carry it on
/// with the chain as it stands there.
///
/// A reader reads the writer's SSA value (or a join's that took it), so the writer dominates
/// it, and the lane-mask atoms of the guards have the same values at both (`guarded`'s
/// argument for the paired form): the guards as functions
/// ([`crate::structure::Bools::meet`]) say what holds lane by lane.
pub(super) fn read_inside_only(p: &Program, st: &Structured) -> HashSet<Value> {
    let order = crate::ir::reverse_postorder(&p.blocks);
    let mut rpo = vec![usize::MAX; p.blocks.len()];
    for (n, &b) in order.iter().enumerate() {
        rpo[b] = n;
    }
    // readers of each value: (block, instruction or none for a join, how, what the reader
    // writes to the same register)
    let mut uses: HashMap<Value, Vec<(usize, Option<usize>, Use, Option<Value>)>> = HashMap::new();
    for (b, block) in p.blocks.iter().enumerate() {
        for phi in &block.phis {
            for (&a, &q) in phi.args.iter().zip(&block.preds) {
                let how = if rpo[q] < rpo[b] { Use::Join(phi.value) } else { Use::Other };
                uses.entry(a).or_default().push((b, None, how, None));
            }
        }
        let sections = super::wave::sections(p, b);
        for (i, inst) in block.insts.iter().enumerate() {
            let name = inst.name();
            let section = sections.iter().any(|(&s, sec)| s < i && i < sec.end);
            let other_lanes = match &inst.fields {
                Fields::Valu(v) => v.dpp.is_some() || name.contains("lane") && name != "v_readfirstlane_b32",
                Fields::Ds { .. } => name.starts_with("ds_swizzle") || name.contains("permute"),
                _ => false,
            } || section;
            // a section's select of the lanes that ran before it (`v_cndmask_b32 d, a, b, m`:
            // b where m is set, a elsewhere)
            let picked = match &inst.fields {
                Fields::Valu(v) if section && name == "v_cndmask_b32" && v.dpp.is_none() && v.sdwa.is_none() => {
                    match v.src.get(2) {
                        Some(&Operand::Scalar { code, .. }) => {
                            inst.read(Reg::S(code)).and_then(|m| st.masks.get(&m).copied()).map(|m| (m, v.src.clone()))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            for &(r, v) in &inst.reads {
                let again = inst.writes.iter().find(|w| w.0 == r).map(|w| w.1);
                let from = |k: usize| matches!(picked.as_ref().and_then(|(_, src)| src.get(k)), Some(&Operand::Vector { reg, .. }) if Reg::V(reg) == r);
                let how = match (r, again) {
                    _ if picked.is_some() && from(1) && !from(0) => Use::Masked(picked.as_ref().expect("picked").0, true),
                    _ if picked.is_some() && from(0) && !from(1) => Use::Masked(picked.as_ref().expect("picked").0, false),
                    _ if other_lanes => Use::Other,
                    _ if name == "v_readfirstlane_b32" => Use::First,
                    (Reg::V(n), Some(_)) if overwrites(inst, n) => Use::Keep,
                    (Reg::V(_), Some(_)) => Use::Both,
                    _ => Use::Source,
                };
                uses.entry(v).or_default().push((b, Some(i), how, again));
            }
        }
    }
    // each guarded write's chain of guards, in program order
    let mut chain: HashMap<Value, Vec<Bool>> = HashMap::new();
    let mut writes = Vec::new();
    for &b in &order {
        for (i, inst) in p.blocks[b].insts.iter().enumerate() {
            let Some(g) = st.guards[b][i] else { continue };
            for &(r, w) in &inst.writes {
                let Reg::V(n) = r else { continue };
                let old = inst.read(r).and_then(|o| chain.get(&o)).cloned().unwrap_or_default();
                chain.insert(w, if overwrites(inst, n) { [vec![g], old].concat() } else { old });
                writes.push(w);
            }
        }
    }
    let mut meets = Meets::default();
    let mut out = HashSet::new();
    for w in writes {
        // (value, chain) still to follow
        let mut work = vec![(w, chain[&w].clone())];
        let (mut steps, mut shows) = (0, false);
        'follow: while let Some((v, c)) = work.pop() {
            for &(rb, ri, how, again) in uses.get(&v).into_iter().flatten() {
                steps += 1;
                if steps > MAX_STEPS {
                    shows = true;
                    break 'follow;
                }
                let guard = ri.and_then(|i| st.guards[rb][i]);
                // some lane entering the reader's block meets `cond` outside every guard of
                // the chain
                let mut outside = |cond: Option<(Bool, bool)>| {
                    if let Some((g, true)) = cond
                        && c.contains(&g)
                    {
                        return false;
                    }
                    let mut all = vec![(st.reach[rb], true)];
                    all.extend(cond);
                    all.extend(c.iter().map(|&g| (g, false)));
                    st.bools.meet_seen(&all, &mut meets)
                };
                let own = guard.map(|g| (g, true));
                // (an unguarded write overwrites every lane that runs it)
                let carried = |c: Vec<Bool>| match (again, guard) {
                    (Some(w2), Some(_)) => Some((w2, c)),
                    _ => None,
                };
                match how {
                    Use::Other => shows = true,
                    Use::Source => shows = outside(own),
                    Use::First => shows = outside(own),
                    Use::Masked(m, set) => shows = outside(Some((m, set))),
                    Use::Keep => work.extend(carried([vec![guard.unwrap_or(Bool::TRUE)], c.clone()].concat())),
                    Use::Both => {
                        shows = outside(own);
                        work.extend(carried(c.clone()));
                    }
                    Use::Join(j) => work.push((j, c.clone())),
                }
                if shows {
                    break 'follow;
                }
            }
        }
        if !shows {
            out.insert(w);
        }
    }
    out
}

/// Whether these forms are tried (`NEONSHIM_WAVE_SPLIT=0`: no, the forms as before).
pub(super) fn on() -> bool {
    std::env::var_os("NEONSHIM_WAVE_SPLIT").is_none_or(|v| v != "0")
}

/// `r`, the program `p` in the exchange form (two host waves of 32 for each console wave, which
/// exchange first lanes, masks, sums and lane reads through LDS between two group barriers), or
/// instead each host wave on its own ([`lower::with_local_waves`]) where every wave-wide value
/// of `p` is used as the allocation idiom uses it ([`idiom::wave_local`]): the same values for
/// every lane, the slots a wave reserves in another order, and none of the barriers (each
/// exchange is two: a group of 64 threads stood at four a lane read).
///
/// The title's `CS_Primary_RegisterAllocationRequests`, a reservation by each wave's first lane
/// of its count of requests (a whole-wave sum of what only that lane holds, then each lane at
/// the base plus its rank), took 3.7 ms of the card per dispatch in the opening cutscene in
/// the exchange form, which was tried first: the idiom form was only tried for programs no
/// form in two host waves took.
///
/// Diagnostics: `NEONSHIM_WAVE_PAIRED_OVER_EXCHANGE=1` gives the remaining exchange-form programs
/// the paired form where it translates them, to time one against the other in a run.
pub(super) fn local_if_exact(
    c: &Container,
    native: &NativeProgram,
    p: &Program,
    st: &structure::Structured,
    io: &io::Io,
    res: &resource::Resources,
    r: Emitted,
) -> Emitted {
    let exchange = matches!(&r, Ok((shader, Some(32))) if forms::form_of(&shader.dxil) == forms::EXCHANGE);
    if !exchange || !on() {
        return r;
    }
    let why = std::env::var_os("NEONSHIM_WAVE_WHY").is_some();
    let idiom = idiom::wave_local(p, st);
    if let (Err((offset, what)), true) = (&idiom, why) {
        eprintln!("WAVE64-SPLIT the exchange kept: not the allocation idiom at {offset:#x}: {what}");
    }
    if idiom.is_ok() {
        let local = lower::with_local_waves(|| emit_program(c, native, p, st, io, res));
        if why {
            match &local {
                Ok((_, wave)) => eprintln!("WAVE64-SPLIT local waves (idiom) instead of the exchange: wave {wave:?}"),
                Err(e) => eprintln!("WAVE64-SPLIT local waves (idiom) instead of the exchange: {e}"),
            }
        }
        if let Ok((shader, Some(32))) = local
            && forms::form_of(&shader.dxil) == forms::LOCAL
        {
            return Ok((shader, Some(32)));
        }
    }
    if cfg!(feature = "diagnostics")
        && std::env::var_os("NEONSHIM_WAVE_PAIRED_OVER_EXCHANGE").is_some_and(|v| v == "1")
        && super::pairs_on()
    {
        let paired = super::paired(c, native, p, io, res);
        if why {
            match &paired {
                Ok((_, wave)) => eprintln!("WAVE64-SPLIT paired lanes instead of the exchange: wave {wave:?}"),
                Err(e) => eprintln!("WAVE64-SPLIT paired lanes instead of the exchange: {e}"),
            }
        }
        if paired.is_ok() {
            return paired;
        }
    }
    r
}
