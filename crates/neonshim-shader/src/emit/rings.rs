//! A legacy geometry program's rings as a geometry shader's inputs and
//! outputs ([`crate::geometry`]): a read of an input vertex in LDS loads that
//! vertex's input element, a write to the invocation's slot of the GS-VS
//! ring is kept as the value of that output of that vertex, and the n-th
//! emit stores vertex n's outputs and emits it (D3D12 takes a vertex's
//! outputs as they stand at its emit).
//!
//! The values a vertex's outputs hold are the IR values the program wrote,
//! lowered where the emit is: the program's writes and messages all lie in
//! one block, on every lane (one invocation per primitive), so each value is
//! defined before the emit that sends it.

use super::{Attr, Fields, Inst, Lower, Reg, V, Why};
use crate::geometry::Message;
use crate::ir::Value;

/// The vertices of the invocation so far.
#[derive(Debug, Default)]
pub(super) struct Emitted {
    /// Per vertex, per dword of the ring: the value written last.
    vertices: Vec<Vec<Option<Value>>>,
    /// Vertices emitted.
    count: u32,
    /// The block of the ring writes and messages.
    block: Option<usize>,
}

impl Lower<'_> {
    /// `ds_*` instruction `i` of block `b` of a geometry program: a ring read
    /// or write ([`crate::geometry::Rings`]).
    pub(super) fn ring(&mut self, inst: &Inst, b: usize, i: usize) -> Result<(), Why> {
        let x = self.x;
        let rings =
            x.io.geometry
                .as_ref()
                .ok_or("an LDS access of a geometry program")?;
        if let Some((_, dwords)) = rings.reads.iter().find(|r| r.0 == (b, i)) {
            let Fields::Ds {
                dst: Some(super::Operand::Vector { reg, .. }),
                ..
            } = &inst.fields
            else {
                return Err("a geometry program's LDS read");
            };
            let guard = self.guard(b, i)?;
            for (k, &(vertex, dword)) in dwords.iter().enumerate() {
                let x = self.ring_input(vertex, dword)?;
                self.write(inst, Reg::V(reg + k as u8), x, guard);
            }
            return Ok(());
        }
        let Some((_, values)) = rings.writes.iter().find(|w| w.0 == (b, i)) else {
            return Err("an LDS access of a geometry program");
        };
        self.one_block(b, i)?;
        let (max, dwords) = (rings.max_vertices as usize, rings.vertex_dwords as usize);
        let vertices = &mut self.emitted.vertices;
        if vertices.is_empty() {
            *vertices = vec![vec![None; dwords]; max];
        }
        for &(value, vertex, dword) in values {
            let slot = vertices
                .get_mut(vertex as usize)
                .and_then(|v| v.get_mut(dword as usize))
                .ok_or("a geometry program's LDS write outside its ring slot")?;
            *slot = Some(value);
        }
        Ok(())
    }

    /// `s_sendmsg` instruction `i` of block `b` of a geometry program.
    pub(super) fn ring_message(&mut self, b: usize, i: usize) -> Result<(), Why> {
        let x = self.x;
        let rings =
            x.io.geometry
                .as_ref()
                .ok_or("a geometry program's message")?;
        let Some(&(_, message)) = rings.messages.iter().find(|m| m.0 == (b, i)) else {
            // GS_DONE, a GS_NOP, another message: nothing for the host
            return Ok(());
        };
        self.one_block(b, i)?;
        let stream = self.s.m.i8(0);
        let void = self.s.t.void;
        if matches!(message, Message::Emit | Message::EmitCut) {
            let n = self.emitted.count as usize;
            let written: Vec<(u32, Value)> = self
                .emitted
                .vertices
                .get(n)
                .map(|v| {
                    v.iter()
                        .enumerate()
                        .filter_map(|(d, x)| Some((d as u32, (*x)?)))
                        .collect()
                })
                .unwrap_or_default();
            if n >= rings.max_vertices as usize {
                return Err("a geometry program emitting past its vertex maximum");
            }
            for (dword, value) in written {
                self.ring_output(dword, value)?;
            }
            self.s
                .op(self.cur, "emitStream", Attr::NoUnwind, void, 97, &[stream]);
            self.emitted.count += 1;
        }
        if matches!(message, Message::Cut | Message::EmitCut) {
            self.s
                .op(self.cur, "cutStream", Attr::NoUnwind, void, 98, &[stream]);
        }
        Ok(())
    }

    /// Ring writes and messages lie in one block, on every lane.
    fn one_block(&mut self, b: usize, i: usize) -> Result<(), Why> {
        if *self.emitted.block.get_or_insert(b) != b || self.x.st.guards[b][i].is_some() {
            return Err("a geometry program emitting in branches");
        }
        Ok(())
    }

    /// Dword `dword` of input vertex `vertex`: channel `dword % 4` of the
    /// input signature's register `dword / 4`.
    fn ring_input(&mut self, vertex: u8, dword: u32) -> Result<V, Why> {
        let (row, channel) = (dword / 4, (dword % 4) as u8);
        let id = self
            .s
            .inputs
            .iter()
            .position(|e| e.row == Some(row) && channel >= e.col && channel < e.col + e.cols)
            .ok_or("a geometry program's input the signature lacks")?;
        let e = &mut self.s.inputs[id];
        if e.half {
            return Err("a 16-bit geometry input");
        }
        let col = channel - e.col;
        e.usage |= 1 << col;
        let float = e.comp == 9;
        let (suffix, ty) = if float {
            (".f32", self.s.t.f32)
        } else {
            (".i32", self.s.t.i32)
        };
        let args = [
            self.s.m.i32(self.sig_id(true, id)),
            self.s.m.i32(0),
            self.s.m.i8(col),
            self.s.m.i32(u32::from(vertex)),
        ];
        let v = self.s.op(
            self.cur,
            &format!("loadInput{suffix}"),
            Attr::ReadNone,
            ty,
            4,
            &args,
        );
        Ok(if float { self.as_int(v) } else { v })
    }

    /// Stores `value` as dword `dword` of the vertex being emitted: channel
    /// `dword % 4` of the output signature's register `dword / 4` (a dword
    /// no element covers carries nothing).
    fn ring_output(&mut self, dword: u32, value: Value) -> Result<(), Why> {
        let (row, channel) = (dword / 4, (dword % 4) as u8);
        let Some(id) = self
            .s
            .outputs
            .iter()
            .position(|e| e.row == Some(row) && channel >= e.col && channel < e.col + e.cols)
        else {
            return Ok(());
        };
        if self.s.outputs[id].half {
            return Err("a 16-bit geometry output");
        }
        let x = self.i32_of(value)?;
        let float = self.s.outputs[id].comp == 9;
        let col = channel - self.s.outputs[id].col;
        self.s.outputs[id].usage |= 1 << col;
        let (suffix, x) = if float {
            let f = match self.floats.get(&value) {
                Some(&f) => f,
                None => self.as_float(x),
            };
            (".f32", f)
        } else {
            (".i32", x)
        };
        let args = [
            self.s.m.i32(self.sig_id(false, id)),
            self.s.m.i32(0),
            self.s.m.i8(col),
            x,
        ];
        let void = self.s.t.void;
        self.s.op(
            self.cur,
            &format!("storeOutput{suffix}"),
            Attr::NoUnwind,
            void,
            5,
            &args,
        );
        Ok(())
    }
}
