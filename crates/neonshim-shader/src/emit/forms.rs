//! Which form a console wave64 compute program took on a host of 32-lane waves, for the
//! census (`neonshim translate` with `NEONSHIM_SHADER_CENSUS`) and the reports of the forms'
//! counts: the lowering names its form ([`super::lower::Lowered::form`]), and each program
//! emitted on this thread since the last [`super::emit`] is remembered by its DXIL, so the
//! form of the one [`super::emit`] returned is known whatever forms were tried before it.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};

/// One host wave for each console wave, two console lanes a thread (`lower::with_paired_lanes`).
pub const PAIRED: &str = "paired";
/// Each host wave a wave of its own where that is exact (`lower::with_local_waves`).
pub const LOCAL: &str = "local";
/// Each host wave a wave of its own, every mask and lane read per host wave
/// (`lower::with_local_waves_any`): approximate.
pub const LOCAL_ANY: &str = "local-any";
/// Two host waves of 32 for each console wave that exchange through LDS and group barriers.
pub const EXCHANGE: &str = "exchange";
/// Two host waves of 32 for each console wave, each on its own, with nothing to exchange.
pub const HALVES: &str = "halves";
/// The console's 64-lane waves asked of the host (refused by a host of 32-lane waves).
pub const WAVE64: &str = "wave64";

thread_local! {
    /// (hash of the DXIL, form) of the programs emitted since the last [`clear`].
    static MADE: RefCell<Vec<(u64, &'static str)>> = const { RefCell::new(Vec::new()) };
}

fn key(dxil: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dxil.hash(&mut h);
    h.finish()
}

/// Forgets the programs emitted before (a new program's emission starts).
pub(super) fn clear() {
    MADE.with_borrow_mut(Vec::clear);
}

/// A program was emitted in `form`.
pub(super) fn note(dxil: &[u8], form: &'static str) {
    if form.is_empty() {
        return;
    }
    let k = key(dxil);
    MADE.with_borrow_mut(|m| m.push((k, form)));
}

/// The form of the console wave64 compute program whose DXIL is `dxil`, emitted on this
/// thread by the last [`super::emit`]: one of the constants above, or empty (not such a
/// program, or emitted elsewhere).
pub fn form_of(dxil: &[u8]) -> &'static str {
    let k = key(dxil);
    MADE.with_borrow(|m| m.iter().rev().find(|e| e.0 == k).map_or("", |e| e.1))
}
