//! The paired form's guarded writes read without their guard where only lanes inside it read
//! them (a child module of `lower`, sharing the emitter's state).
//!
//! In the paired form every per-lane branch is the wave's (`structure::structure_wide`), so
//! EXEC stays a guard on each vector instruction: a write under a guard is
//! `select(guard, new, old)`, the lanes outside it keeping what the register held. A sixth of
//! the paired programs' host code was such selects (301,243 of the 69 paired programs'
//! 1,925,551 instructions, against 29,850 in the forms of the same programs that run a lane
//! per thread), each keeping the register's old value alive until the select.
//!
//! The lanes outside the guard need the old value only where they read the register. A lane
//! that reads it under a guard that implies the writer's (the same guard, most often:
//! straight-line code under one EXEC) ran the write, so it can take the new value as computed,
//! without the select. What a lane outside the reader's guard computes from it is discarded by
//! the reader's own guard (its select, or the branch around a store), as it is now from the
//! old value; a load it makes reads somewhere else than the old value would have had it read,
//! which every load already allows (lanes outside a guard make their loads too). Only reads
//! of the lane's own registers are taken so ([`Lower::raw_use_for`]): not a DPP source, a
//! lane read or write, a swizzle, a permute (they read other lanes, which may be outside the
//! guard), and not the old value a write keeps (`write_placed`, the half a 16-bit write keeps,
//! an atomic's result outside its guard), which keep reading the selected value. The selects
//! no reader needs any more are left to the clean-up's dead-code pass.
//!
//! Why the implication of the guards as functions over lane-mask atoms
//! ([`crate::structure::Bools::meet`]) is the implication in every lane: the reader reads the
//! writer's SSA value, so the writer's block dominates the reader's, and the reader reads what
//! the writer's latest run wrote. The atoms are SSA values too; those of the writer's guard
//! dominate the writer, so between the writer's latest run and the reader none of them is
//! defined again: a loop around the writer brings the reader only through the writer again
//! (it dominates the reader), and a loop around the reader alone does not hold the writer or
//! the atoms it was guarded by. Both guards are thus evaluated on the same atom values.
//! Measured on the title's 69 paired programs: host instructions 2,302,837 → 1,616,958, the
//! guard selects 360,668 → 124,509 (within one IR block only: 1,703,952 and 144,885).
//! `NEONSHIM_WAVE_RAW=0` turns this off.

use std::collections::HashMap;

use super::{Lower, V, Why};
use crate::gfx10::Fields;
use crate::ir::{Inst, Value};
use crate::structure::Bool;

/// What a guarded write computed before its guard: the value, the float it was made from
/// (a float result), and the guard.
#[derive(Debug, Clone, Copy)]
pub(super) struct Raw {
    int: V,
    float: Option<V>,
    guard: Bool,
}

pub(super) type Raws = HashMap<Value, Raw>;

/// Whether guarded writes are read without their guard (`NEONSHIM_WAVE_RAW=0`: no).
fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NEONSHIM_WAVE_RAW").is_none_or(|v| v != "0"))
}

impl Lower<'_> {
    /// The guard (and its value in the lane being lowered) under which instruction `i` of IR
    /// block `b` reads its sources, if they may be taken without their writers' guards: the
    /// paired form, a guarded instruction that reads only its own lane's registers.
    pub(super) fn raw_use_for(&mut self, b: usize, i: usize, inst: &Inst) -> Result<Option<(Bool, V)>, Why> {
        if self.twin.is_none() || !on() {
            return Ok(None);
        }
        let Some(g) = self.x.st.guards[b][i] else {
            return Ok(None);
        };
        let name = inst.name();
        let own = match &inst.fields {
            Fields::Valu(v) => {
                v.dpp.is_none()
                    && !name.contains("lane")
                    && !name.starts_with("v_movrel")
                    && !matches!(name, "v_swap_b32")
            }
            Fields::Ds { .. } => !name.starts_with("ds_swizzle") && !name.contains("permute"),
            Fields::Image { .. } => !name.starts_with("image_bvh"),
            Fields::Buffer { .. } | Fields::Flat { .. } => true,
            _ => false,
        };
        if !own {
            return Ok(None);
        }
        let v = self.boolean(g, self.cur)?;
        Ok(Some((g, v)))
    }

    /// A guarded write of `x` (the selected value is the register's new value `new`): kept for
    /// the reads of [`Self::source`] if `guard` is the guard of the instruction being lowered.
    pub(super) fn note_raw(&mut self, new: Value, x: V, guard: V) {
        if let Some((g, gv)) = self.raw_use
            && gv == guard
        {
            self.raw.insert(new, Raw { int: x, float: None, guard: g });
        }
    }

    /// The float `f` that a guarded write noted by [`Self::note_raw`] was made from.
    pub(super) fn note_raw_float(&mut self, new: Value, f: V) {
        if let Some(r) = self.raw.get_mut(&new) {
            r.float = Some(f);
        }
    }

    /// A register's value as a source of the instruction being lowered: the value its writer
    /// computed before its guard where the reader's guard implies the writer's
    /// ([`Self::raw_use_for`]), else the value the register holds.
    pub(super) fn source(&mut self, v: Value) -> Result<V, Why> {
        match self.raw_of(v) {
            Some(r) => Ok(r.int),
            None => self.i32_of(v),
        }
    }

    /// [`Self::source`] as a float, `None` where the register's value is read.
    pub(super) fn raw_float(&mut self, v: Value) -> Option<V> {
        let r = self.raw_of(v)?;
        Some(r.float.unwrap_or_else(|| self.as_float(r.int)))
    }

    fn raw_of(&mut self, v: Value) -> Option<Raw> {
        let (use_guard, _) = self.raw_use?;
        let r = *self.raw.get(&v)?;
        // (no lane runs the reader without the writer: the reader's guard and the negation of
        // the writer's meet in no lane)
        let implied = use_guard == r.guard
            || r.guard == Bool::TRUE
            || !self.x.st.bools.meet_seen(&[(use_guard, true), (r.guard, false)], &mut self.raw_meets);
        implied.then_some(r)
    }
}
