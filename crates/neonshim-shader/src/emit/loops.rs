//! The iteration limit of translated loops: the limits by stage, the count each loop header
//! keeps and the test of it on each back edge (`Lower::exit` lowers the branches), the loop
//! notes that count the lanes reaching a limit (`super::super::loop_notes`). A child module of
//! the lowering, so it shares the emitter's state.

use super::{Attr, Bin, LaneExit, Lower, Tag, Term, V};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    /// Tests: loop notes on this thread whatever [`super::super::loop_notes`] says.
    pub(in crate::emit) static TEST_NOTES: Cell<bool> = const { Cell::new(false) };
}

/// Whether this translation counts the lanes that reach a loop's limit
/// ([`super::super::loop_notes`]).
pub(super) fn notes_on() -> bool {
    #[cfg(test)]
    if TEST_NOTES.get() {
        return true;
    }
    super::super::loop_notes()
}

/// Iterations after which a loop of a program that tests rays against tree
/// nodes is left, unless [`super::super::loop_limits`] says otherwise.
///
/// A ray's walk of a well-formed tree visits some hundreds of nodes at
/// most. The title's shadow-mask rays ran to the compute limit of 4,096: the
/// trees of moving objects are built by the title's builder, which is off,
/// so a walk into one follows whatever its memory holds. At 4,096 the
/// shadow mask took 11.7 ms of the card a frame, at 1,024 2.4 ms (timing
/// runs of the opening of the gameplay, RTX 5090); a ray stopped either way
/// leaves the program as killed, as before.
pub(in crate::emit) const TRACE_LOOP_LIMIT: u32 = 1 << 10;

/// Iterations after which a loop of a pixel program whose end the data
/// decides is left (per entry into it), unless [`super::super::loop_limits`] says
/// otherwise ([`Lower::count_iteration`]). The console runs such a loop as
/// long as its data says; data the host has not mirrored yet (a null view
/// reads zeros) can make that four billion iterations, which hangs the host
/// GPU until the driver resets it. A lane that reaches the limit leaves the
/// loop and goes on with the program: where the loop branches back on a
/// condition, or, in a loop that goes round by jumps back, at its exit; only
/// on a way round that passes no exit does it leave the program as killed (a
/// pixel discards). A loop whose code fixes its count (`trip`) runs it all.
// shortcut: a genuine loop of more iterations than the limit is cut short too.
pub(in crate::emit) const PIXEL_LOOP_LIMIT: u32 = 1 << 7;
/// The same for vertex and mesh programs.
pub(in crate::emit) const VERTEX_LOOP_LIMIT: u32 = 1 << 8;
/// The same for compute programs that test no rays ([`TRACE_LOOP_LIMIT`]).
pub(in crate::emit) const COMPUTE_LOOP_LIMIT: u32 = 1 << 12;

impl Lower<'_> {
    /// A forward edge into loop header `to` (from DXIL block `via`) starts
    /// its count at 0.
    pub(super) fn enter_loop(&mut self, _from: usize, to: usize, via: usize) {
        if let Some((_, incoming)) = self.counters.get_mut(&to) {
            let z = self.s.m.i32(0);
            incoming.push((z, via));
        }
    }

    /// A back edge to header `to` (a back edge keeps its block, no
    /// critical-edge block): the count goes up; the result compares it with
    /// the stage's limit by `p` (below: go on; at or above: stop).
    pub(super) fn count_iteration(&mut self, to: usize, p: u8) -> V {
        let c = self.cur;
        let (phi, _) = self.counters[&to];
        let one = self.s.m.i32(1);
        let next = self.s.m.bin(c, Bin::Add, phi, one);
        self.s.m.tag(phi, Tag::Counter);
        self.s.m.tag(next, Tag::Counter);
        let limit = self.s.m.i32(self.limit_of(to));
        self.counters
            .get_mut(&to)
            .expect("header")
            .1
            .push((next, c));
        let over = self.s.m.cmp(c, p, next, limit);
        self.s.m.tag(over, Tag::Counter);
        over
    }

    /// Where a loop that goes round by jumps back alone can leave (IR block
    /// `b`'s branch to `taken` or `not_taken`, one inside the loop, one out):
    /// the loop's header, and whether the way inside is `taken`. A lane at
    /// the limit leaves there and goes on with the program ([`Self::limit_test`])
    /// rather than at its jump back, where it could only leave the program.
    pub(super) fn jump_loop_exit(
        &self,
        b: usize,
        taken: usize,
        not_taken: usize,
    ) -> Option<(usize, bool)> {
        let (h, body) = self.jump_loops.iter().find(|(_, body)| body[b])?;
        match (body[taken], body[not_taken]) {
            (true, false) => Some((*h, true)),
            (false, true) => Some((*h, false)),
            _ => None,
        }
    }

    /// The counted loops that go round by jumps back alone, innermost first,
    /// with their blocks ([`Self::jump_loop_exit`]).
    pub(super) fn find_jump_loops(&mut self) {
        let p = self.x.p;
        let reached = |l: &Self, q: usize| l.block[q].is_some();
        for &h in self.counters.keys() {
            let latches: Vec<usize> = p.blocks[h]
                .preds
                .iter()
                .copied()
                .filter(|&q| reached(self, q) && self.rpo[h] <= self.rpo[q])
                .collect();
            if !latches
                .iter()
                .all(|&q| matches!(self.x.st.exits[q], LaneExit::Jump(_)))
            {
                continue;
            }
            let mut body = vec![false; p.blocks.len()];
            body[h] = true;
            let mut stack = latches;
            while let Some(q) = stack.pop() {
                if !std::mem::replace(&mut body[q], true) {
                    stack.extend(
                        p.blocks[q]
                            .preds
                            .iter()
                            .copied()
                            .filter(|&x| reached(self, x)),
                    );
                }
            }
            self.jump_loops.push((h, body));
        }
        self.jump_loops
            .sort_by_key(|(h, body)| (body.iter().filter(|&&x| x).count(), *h));
    }

    /// The count of loop `h` this pass plus one compared with its limit by `p`
    /// (below: go on; at or above: stop), as at its back edges, without
    /// counting.
    pub(super) fn limit_test(&mut self, h: usize, p: u8) -> V {
        let c = self.cur;
        let (phi, _) = self.counters[&h];
        let one = self.s.m.i32(1);
        let next = self.s.m.bin(c, Bin::Add, phi, one);
        self.s.m.tag(next, Tag::Counter);
        let limit = self.s.m.i32(self.limit_of(h));
        let r = self.s.m.cmp(c, p, next, limit);
        self.s.m.tag(r, Tag::Counter);
        r
    }

    /// The limit of the loop of header `to`.
    fn limit_of(&self, to: usize) -> u32 {
        // Per stage unless the switch names one: a loop that never meets its
        // exit (a count read from a buffer the host has not got yet, say)
        // runs to the limit in every lane. A million iterations a pixel
        // hung the GPU for seconds a draw until Windows reset it
        // (PS_Textured_Eight, 2026-11-24, run 12).
        // shortcut: a pixel loop past 128 iterations, a vertex or mesh loop
        // past 256 or a compute loop past 4,096 is cut short. (They were
        // 1,024, 4,096 and 65,536: the opening cutscene then lost the card
        // 14-25 frames in, in a minimap or shadow mesh draw; with every
        // loop cut at 64 it ran 90 frames. Raised to those again on
        // 2026-11-25, after the fixes of loops that ran long for other reasons
        // (helper lanes, fixed counts, exchanges in loops), the opening
        // cutscene ran 9 minutes, until Windows reset the card as a
        // remote-desktop session ended (Parsec; no work of ours). Loop notes
        // found no loop that needs more than these limits, only loops
        // without an end, which a higher limit only runs longer:
        // PS_Textured_Eight's search of its shadow cascades, whose count it
        // reads as 0 (b37565055f2f7d3b, 1af4a50c7fb40c97: 10,441 pixels at
        // 1,024), and the projected-decal resolve's walk of its decal bits
        // (c4183d3cbee28e8e, in the approximate one-host-wave form: 10,381
        // lanes at 65,536). Raise them once those end.)
        let limits = super::super::loop_limits();
        let limit = if self.s.pixel {
            limits.pixel
        } else if self.s.mesh.is_some() || self.s.threads.is_none() {
            limits.vertex
        } else if self.traces {
            limits.trace
        } else {
            limits.compute
        };
        // A loop whose code fixes how often it goes round runs that often, as
        // on the console, whatever the data: the limit is for loops the data
        // ends. (The title's environment BRDF tables integrate 1,024 samples a
        // texel, two or four a pass: at the pixel limit of 128 passes each of
        // the 65,536 texels of the three tables stopped at a half or a quarter
        // of its samples, in all six loops, counted with `NEONSHIM_LOOP_NOTES=1`
        // in the opening cutscene.)
        match self.passes.get(&to) {
            Some(&n) => limit.max(n.saturating_add(1)),
            None => limit,
        }
    }

    /// The block a lane stopped by its loop's limit leaves through.
    pub(super) fn abort_block(&mut self) -> usize {
        if let Some(a) = self.abort {
            return a;
        }
        let a = self.s.m.new_block();
        if self.s.pixel {
            let t = self.s.m.bool(true);
            self.s
                .op(a, "discard", Attr::NoUnwind, self.s.t.void, 82, &[t]);
        }
        self.s.m.terminate(a, Term::Ret);
        self.abort = Some(a);
        a
    }

    /// With loop notes ([`super::super::loop_notes`]): a new block that counts a lane
    /// that reached loop `header`'s iteration limit in the program's notes
    /// (`shader::SLOT_LOOPS`; `killed`: the lane leaves the program, else it
    /// leaves the loop and goes on), then branches to `then`.
    pub(super) fn note_block(&mut self, header: usize, killed: bool, then: usize) -> usize {
        use super::super::shader::{LOOP_NOTE_DWORDS, LOOP_NOTES, SLOT_LOOPS};
        let last = LOOP_NOTES / LOOP_NOTE_DWORDS - 1;
        let k = self
            .notes
            .as_ref()
            .and_then(|n| n.get(&header))
            .copied()
            .unwrap_or(last);
        let n = self.s.m.new_block();
        let h = self.s.root_uav_handle(SLOT_LOOPS);
        let (i32t, void) = (self.s.t.i32, self.s.t.void);
        let iu = self.s.m.undef(i32t);
        let record = k * LOOP_NOTE_DWORDS * 4;
        // The loop's first code byte plus 1 (0: no record), then the count.
        let at = self.s.m.i32(record);
        let first = self.s.m.i32(self.x.p.blocks[header].offset as u32 + 1);
        let (mask, align) = (self.s.m.i8(1), self.s.m.i32(4));
        let args = [h, at, iu, first, iu, iu, iu, mask, align];
        self.s
            .op(n, "rawBufferStore.i32", Attr::NoUnwind, void, 140, &args);
        let (add, one) = (self.s.m.i32(0), self.s.m.i32(1));
        let count = self.s.m.i32(record + if killed { 8 } else { 4 });
        self.s.op(
            n,
            "atomicBinOp.i32",
            Attr::NoUnwind,
            i32t,
            78,
            &[h, add, count, iu, iu, one],
        );
        self.s.m.terminate(n, Term::Br(then));
        n
    }

    /// With loop notes: the exit edge of IR block `b`'s branch to `to` (DXIL
    /// block `via`), whose other edge stays in the counted loop of `header`
    /// (its back edge, or the way on to a jump back), made to pass a test of
    /// `wanted` (the branch's own condition, before the limit) that counts the
    /// lanes leaving at the limit ([`Self::note_block`]): those that wanted to
    /// stay when staying is the branch's taken side (`back_taken`), those that
    /// did not otherwise. The block the branch now goes to, and the one `to`
    /// is now entered from.
    pub(super) fn note_exit(
        &mut self,
        b: usize,
        to: usize,
        via: usize,
        header: usize,
        wanted: V,
        back_taken: bool,
    ) -> (usize, usize) {
        let (test, join) = (self.s.m.new_block(), self.s.m.new_block());
        let note = self.note_block(header, false, join);
        let term = if back_taken {
            Term::CondBr(wanted, note, join)
        } else {
            Term::CondBr(wanted, join, note)
        };
        self.s.m.terminate(test, term);
        self.s.m.terminate(join, Term::Br(via));
        if Some(via) == self.block[to] {
            // `to`'s joins take this edge's values from here on.
            self.edges.insert((b, to), join);
            (test, join)
        } else {
            (test, via)
        }
    }
}
