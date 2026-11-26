//! The D3D12 Agility SDK for this crate's test executable.
//!
//! Same exports as `apps/neonshim/src/agility.rs`: without them the GPU
//! tests get Windows 10's in-box runtime, which stops at shader model 6.5,
//! and skip. `build.rs` exports both; `cargo xtask agility` puts the runtime
//! in `target/<profile>/deps/D3D12/`.
#![allow(non_upper_case_globals)]

/// Keep in step with `apps/neonshim/src/agility.rs`.
#[unsafe(no_mangle)]
pub static D3D12SDKVersion: u32 = 620;

#[repr(transparent)]
pub struct SdkPath(*const std::ffi::c_char);

// SAFETY: points at a static, immutable C string.
unsafe impl Sync for SdkPath {}

#[unsafe(no_mangle)]
pub static D3D12SDKPath: SdkPath = SdkPath(c".\\D3D12\\".as_ptr());
