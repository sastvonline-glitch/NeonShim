//! Diagnostics: the lanes of translated shaders that reached a loop's
//! iteration limit (`NEONSHIM_LOOP_NOTES=1`, see
//! `neonshim_shader::emit::loop_notes`).
//!
//! A program translated with loop notes has a root UAV
//! (`neonshim_shader::emit::SLOT_LOOPS`) where each lane that reaches a
//! loop's limit adds one to that loop's record. The host binds each program
//! a range of one shared buffer of its own, given out at its first draw, and
//! the frame line reads the ranges every five seconds and logs the loops
//! whose counts grew, by stage, hash and the loop's first code byte.

use std::sync::{Mutex, OnceLock};

use neonshim_shader::emit::{LOOP_NOTE_DWORDS, LOOP_NOTES};
use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

use crate::host::host;
use crate::pipeline::Shader;

/// Programs with a range of their own (1 MiB); later ones share the last.
const PROGRAMS: u32 = 4096;
/// Loop records in a range.
const LOOPS: usize = (LOOP_NOTES / LOOP_NOTE_DWORDS) as usize;

struct Buffer {
    _buffer: ID3D12Resource,
    data: *mut u32,
    gpu: u64,
}

// SAFETY: the mapping is of a buffer the struct owns, kept for the process;
// the GPU writes it and the CPU only reads it.
unsafe impl Send for Buffer {}
// SAFETY: as above.
unsafe impl Sync for Buffer {}

static BUFFER: OnceLock<Option<Buffer>> = OnceLock::new();

/// The programs given a range, in order (stage, hash), and per program and
/// loop the counts logged last (lanes that went on, lanes taken out).
static SEEN: Mutex<(Vec<(&'static str, String)>, Vec<[(u32, u32); LOOPS]>)> =
    Mutex::new((Vec::new(), Vec::new()));

fn buffer() -> Option<&'static Buffer> {
    BUFFER
        .get_or_init(|| {
            let bytes = u64::from(PROGRAMS) * u64::from(LOOP_NOTES) * 4;
            let (buffer, data) = host()?.shared_buffer(bytes)?;
            crate::host::name(&buffer, || "loop notes".to_owned());
            // SAFETY: a fresh mapping of `bytes` bytes.
            unsafe { std::ptr::write_bytes(data, 0, bytes as usize) };
            // SAFETY: a live buffer.
            let gpu = unsafe { buffer.GetGPUVirtualAddress() };
            tracing::info!(
                "loop notes: lanes that reach a translated loop's iteration limit are counted"
            );
            Some(Buffer {
                _buffer: buffer,
                data: data.cast(),
                gpu,
            })
        })
        .as_ref()
}

/// The GPU address of `shader`'s range, given out on first use; None
/// without the buffer (the draw must then be dropped).
pub(crate) fn address(shader: &Shader) -> Option<u64> {
    let b = buffer()?;
    let slot = *shader.loop_slot.get_or_init(|| {
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        seen.0.push((shader.stage, shader.hash.clone()));
        seen.1.push([(0, 0); LOOPS]);
        (seen.0.len() - 1) as u32
    });
    if slot >= PROGRAMS {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            tracing::warn!(
                PROGRAMS,
                "loop notes: more programs than ranges; later ones share the last"
            )
        });
    }
    Some(b.gpu + u64::from(slot.min(PROGRAMS - 1)) * u64::from(LOOP_NOTES) * 4)
}

/// Logs the loops whose counts grew since the last call, most first (at
/// most 40 lines), and how many programs have reached a limit so far.
pub(crate) fn log() {
    let Some(b) = BUFFER.get().and_then(Option::as_ref) else {
        return;
    };
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let (programs, logged) = &mut *seen;
    let mut grew = Vec::new();
    let mut reached = 0;
    for (slot, ((stage, hash), last)) in programs
        .iter()
        .zip(logged.iter_mut())
        .enumerate()
        .take(PROGRAMS as usize)
    {
        let mut any = false;
        for (k, last) in last.iter_mut().enumerate() {
            let at = slot * LOOP_NOTES as usize + k * LOOP_NOTE_DWORDS as usize;
            // SAFETY: inside the mapping; the GPU may be adding, a count
            // read early is read again next time.
            let [first, left, killed] =
                [0, 1, 2].map(|i| unsafe { b.data.add(at + i).read_volatile() });
            if first == 0 {
                continue;
            }
            any |= left > 0 || killed > 0;
            if (left, killed) != *last {
                let more =
                    u64::from(left - last.0.min(left)) + u64::from(killed - last.1.min(killed));
                grew.push((more, *stage, hash.clone(), first - 1, left, killed));
                *last = (left, killed);
            }
        }
        reached += usize::from(any);
    }
    if grew.is_empty() {
        return;
    }
    grew.sort_by_key(|g| std::cmp::Reverse(g.0));
    for (more, stage, hash, offset, left, killed) in grew.iter().take(40) {
        tracing::info!(
            "loop notes: {stage} {hash} loop at {offset:#x} reached its iteration limit in \
             {more} more lanes; since the start {left} lanes left the loop there and went on, \
             {killed} were taken out of the program"
        );
    }
    tracing::info!(
        "loop notes: {reached} of {} programs drawn with counted loops have reached a limit; \
         {} loops grew in the last lines",
        programs.len(),
        grew.len()
    );
}
