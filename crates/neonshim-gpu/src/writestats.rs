//! Which pipelines' shader writes the layer copies back into title memory,
//! and how much (`NEONSHIM_WRITE_STATS=1`; off by default): the bytes each
//! pipeline declares written a frame, top ten in the log every five seconds.
//!
//! Copy-back is a large part of a frame (about 60 of 240 ms at a busy
//! place), and much of what is copied back may be effects the CPU never
//! reads. This says which pipelines those bytes come from, by their
//! shaders' hashes, before anything is changed.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::object::This;

thread_local! {
    /// The pipeline the recording thread binds now.
    static CURRENT: Cell<usize> = const { Cell::new(0) };
}

/// (bytes, declarations) a pipeline's shaders were declared to write.
static WRITES: Mutex<Option<HashMap<usize, (u64, u64)>>> = Mutex::new(None);
static LAST: AtomicU64 = AtomicU64::new(0);
static FRAME: AtomicU64 = AtomicU64::new(0);

pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NEONSHIM_WRITE_STATS").is_some_and(|v| v == "1"))
}

/// The recording thread binds `pipeline`.
pub(crate) fn binding(pipeline: This) {
    if on() {
        CURRENT.with(|c| c.set(pipeline as usize));
    }
}

/// The pipeline bound on this thread declared `len` bytes written.
pub(crate) fn wrote(len: u64) {
    if !on() {
        return;
    }
    let p = CURRENT.with(Cell::get);
    {
        let mut w = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let e = w.get_or_insert_with(HashMap::new).entry(p).or_default();
        e.0 += len;
        e.1 += 1;
    }
    let now = crate::host::now_ms();
    let last = LAST.load(Ordering::Relaxed);
    if now < last + 5000 || LAST.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    let frame = crate::frame::current();
    let frames = frame.saturating_sub(FRAME.swap(frame, Ordering::Relaxed)).max(1) as f64;
    let mut all: Vec<(usize, (u64, u64))> = WRITES.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default().into_iter().collect();
    all.sort_by_key(|a| std::cmp::Reverse(a.1.0));
    let total: u64 = all.iter().map(|a| a.1.0).sum();
    tracing::info!(
        mb_a_frame = format_args!("{:.1}", total as f64 / frames / 1048576.0),
        pipelines = all.len(),
        "write stats: shader writes declared (copied back) per frame, top pipelines below"
    );
    for (p, (bytes, n)) in all.into_iter().take(10) {
        tracing::info!(
            mb_a_frame = format_args!("{:.2}", bytes as f64 / frames / 1048576.0),
            declarations = format_args!("{:.1}", n as f64 / frames),
            pipeline = %crate::pipeline::describe(p as This),
            "write stats"
        );
    }
}
