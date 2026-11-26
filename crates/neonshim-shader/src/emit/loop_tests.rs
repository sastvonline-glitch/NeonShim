//! End-to-end tests of translated loops: the iteration limit, the loop notes
//! (`shader::SLOT_LOOPS`) and loops over the wave's lanes in pixel programs.
//! Synthetic GFX10 programs lowered to DXIL, validated by `dxil.dll` when
//! present, and run on the host GPU and WARP.

use super::bitcode::{Attr, Term};
use super::container::SigElement;
use super::host::{Dispatch, Draw, Host, Root, View};
use super::lower::{self, COMPUTE_LOOP_LIMIT, TEST_NOTES};
use super::shader::Shader;
use super::valu_tests::{ONE_AT_A_TIME, build_cs, s_mov_b32, sgpr, sop2, vop1};
use super::{HostShader, LOOP_NOTE_DWORDS, LOOP_NOTES, SLOT_LOOPS, validator};
use crate::native::{HwStage, NativeProgram, RootType, StageIo};
use crate::{io, ir, resource, structure};

/// `s_add_u32 s6, s6, 1`.
const S_ADD_S6_1: u32 = sop2(0x00, 6, 6, 0x81);
/// `s_cmp_lg_u32 s6, s0`.
const S_CMP_LG_S6_S0: u32 = 0xBF07_0006;
/// `s_cmp_eq_u32 s6, s0`.
const S_CMP_EQ_S6_S0: u32 = 0xBF06_0006;

/// What the 32 threads of `body` (with [`build_cs`]'s prologue of 7 dwords)
/// stored, and the program's loop notes, on each device that ran it, with
/// the root constants all 0.
fn run(body: &[u32]) -> Vec<(&'static str, u32, Vec<u32>)> {
    TEST_NOTES.set(true);
    let shader = build_cs(body, 32, 0);
    TEST_NOTES.set(false);
    assert_eq!(shader.root_uavs, [SLOT_LOOPS]);
    run_shader(&shader)
}

fn run_shader(shader: &HostShader) -> Vec<(&'static str, u32, Vec<u32>)> {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = host
            .compute_pipeline_bounded(shader)
            .unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        // The root constants and the table's heap index: all 0.
        for u in &shader.user_data {
            assert!(
                matches!(
                    (u.root, u.slot),
                    (RootType::Constants, 0) | (RootType::Table, 1)
                ),
                "{u:?}"
            );
        }
        let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        // Root parameter 0 is the user data, 1 the notes (the only root view).
        let words = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![
                        (0, Root::Constants(ud)),
                        (1, Root::Uav(vec![0; LOOP_NOTES as usize])),
                    ],
                    views: &[View::RawUav(1)],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        out.push((label, words[0][0], words[1].clone()));
    }
    assert!(!out.is_empty(), "no SM 6.6 device");
    out
}

/// The record of loop `k` in a program's notes.
fn record(notes: &[u32], k: usize) -> &[u32] {
    let n = LOOP_NOTE_DWORDS as usize;
    &notes[k * n..(k + 1) * n]
}

#[test]
fn a_loop_whose_branch_back_holds_leaves_at_the_limit_and_goes_on() {
    // s6 counts up from 0 while it differs from s0 (0): the console would
    // run it four billion times. The loop starts at byte 32.
    let body = [
        s_mov_b32(6, 0x80),
        S_ADD_S6_1,
        S_CMP_LG_S6_S0,
        0xBF85_FFFD,             // s_cbranch_scc1 (back to the s_add)
        vop1(0x001, 0, sgpr(6)), // v_mov_b32 v0, s6
    ];
    for (label, stored, notes) in run(&body) {
        // The lanes went on after the last allowed pass and stored s6.
        assert_eq!(stored, COMPUTE_LOOP_LIMIT, "{label}");
        // Each of the 32 counted as leaving the loop and going on.
        assert_eq!(record(&notes, 0), [32 + 1, 32, 0, 0], "{label}");
        assert!(
            notes[LOOP_NOTE_DWORDS as usize..].iter().all(|&w| w == 0),
            "{label}"
        );
    }
}

#[test]
fn a_loop_closed_by_a_jump_is_left_at_its_exit_at_the_limit() {
    // The same count, left by a forward branch when s6 reaches s0 (0) and
    // closed by `s_branch`: at the limit the lane leaves by that branch and
    // goes on, as at a branch back.
    let body = [
        s_mov_b32(6, 0x80),
        S_ADD_S6_1,
        S_CMP_EQ_S6_S0,
        0xBF85_0001,             // s_cbranch_scc1 (past the s_branch)
        0xBF82_FFFC,             // s_branch (back to the s_add)
        vop1(0x001, 0, sgpr(6)), // v_mov_b32 v0, s6
    ];
    for (label, stored, notes) in run(&body) {
        assert_eq!(stored, COMPUTE_LOOP_LIMIT, "{label}");
        assert_eq!(record(&notes, 0), [32 + 1, 32, 0, 0], "{label}");
    }
}

#[test]
fn a_way_round_that_passes_no_exit_takes_the_lane_out_at_the_limit() {
    // The exit test is skipped on the way round (s6 differs from s0 = 0, so
    // the branch over it is always taken): at the limit the jump back takes
    // the lane out of the program.
    let body = [
        s_mov_b32(6, 0x80),
        S_ADD_S6_1,
        S_CMP_LG_S6_S0,
        0xBF85_0002, // s_cbranch_scc1 (over the exit test, to the s_branch)
        S_CMP_EQ_S6_S0,
        0xBF85_0001,             // s_cbranch_scc1 (out, past the s_branch)
        0xBF82_FFFA,             // s_branch (back to the s_add)
        vop1(0x001, 0, sgpr(6)), // v_mov_b32 v0, s6
    ];
    for (label, stored, notes) in run(&body) {
        // Nothing stored: every lane was taken out.
        assert_eq!(stored, 0, "{label}");
        assert_eq!(record(&notes, 0), [32 + 1, 0, 32, 0], "{label}");
    }
}

#[test]
fn a_loop_that_ends_leaves_no_note() {
    // s6 counts to 5 (s0 + 5 written as an inline constant compare).
    let body = [
        s_mov_b32(6, 0x80),
        S_ADD_S6_1,
        0xBF07_8506, // s_cmp_lg_u32 s6, 5
        0xBF85_FFFD, // s_cbranch_scc1
        vop1(0x001, 0, sgpr(6)),
    ];
    for (label, stored, notes) in run(&body) {
        assert_eq!(stored, 5, "{label}");
        assert!(notes.iter().all(|&w| w == 0), "{label}: {notes:?}");
    }
}

#[test]
fn a_loop_whose_code_fixes_its_count_runs_to_its_end_past_the_limit() {
    // s6 counts from 0 while it differs from 5,000 (a literal): more passes
    // than the compute limit, all of them the console's.
    let body = [
        s_mov_b32(6, 0x80),
        S_ADD_S6_1,
        0xBF07_FF06, // s_cmp_lg_u32 s6, lit(5000)
        5000,
        0xBF85_FFFC,             // s_cbranch_scc1 (back to the s_add)
        vop1(0x001, 0, sgpr(6)), // v_mov_b32 v0, s6
    ];
    const { assert!(5000 > COMPUTE_LOOP_LIMIT) };
    for (label, stored, notes) in run(&body) {
        assert_eq!(stored, 5000, "{label}");
        assert!(notes.iter().all(|&w| w == 0), "{label}: {notes:?}");
    }
}

/// A signature element of four floats in row 0.
fn element(name: &str, kind: u8, interp: u8, usage: u8) -> SigElement {
    SigElement {
        name: name.into(),
        index: 0,
        kind,
        comp: 9,
        interp,
        row: Some(0),
        col: 0,
        cols: 4,
        usage,
        half: false,
    }
}

fn host_shader(stage: HwStage, dxil: Vec<u8>) -> HostShader {
    HostShader {
        stage,
        dxil,
        user_data: Vec::new(),
        samplers: Vec::new(),
        static_samplers: Vec::new(),
        root_cbvs: Vec::new(),
        root_srvs: Vec::new(),
        root_uavs: Vec::new(),
        written_root_uavs: Vec::new(),
        writes_guest_memory: false,
        root_indexed_views: Vec::new(),
        half_views: Vec::new(),
    }
}

/// A vertex shader that passes `POSITION` through as `SV_Position`.
fn position_vs() -> HostShader {
    let mut vs = Shader::new(false);
    vs.inputs = vec![element("POSITION", 0, 0, 15)];
    vs.outputs = vec![element("SV_Position", 3, 4, 15)];
    let b = vs.entry;
    for col in 0..4 {
        let args = [vs.m.i32(0), vs.m.i32(0), vs.m.i8(col), vs.m.undef(vs.t.i32)];
        let v = vs.op(b, "loadInput.f32", Attr::ReadNone, vs.t.f32, 4, &args);
        let args = [vs.m.i32(0), vs.m.i32(0), vs.m.i8(col), v];
        vs.op(b, "storeOutput.f32", Attr::NoUnwind, vs.t.void, 5, &args);
    }
    vs.m.terminate(b, Term::Ret);
    host_shader(HwStage::Vs, vs.finish())
}

#[test]
fn a_vertex_program_with_loop_notes_declares_its_uav() {
    // A vertex or mesh program with loop notes binds the notes as a root UAV:
    // it must declare UAVs at a stage other than pixel and compute, or the
    // validator refuses it ("Flags must match usage", 62 of the 801 vertex
    // and mesh programs of a game run translated with notes).
    let mut vs = Shader::new(false);
    vs.inputs = vec![element("POSITION", 0, 0, 15)];
    vs.outputs = vec![element("SV_Position", 3, 4, 15)];
    let b = vs.entry;
    let h = vs.root_uav_handle(SLOT_LOOPS);
    let (i32t, void) = (vs.t.i32, vs.t.void);
    let iu = vs.m.undef(i32t);
    let (at, first, mask, align) = (vs.m.i32(0), vs.m.i32(1), vs.m.i8(1), vs.m.i32(4));
    let args = [h, at, iu, first, iu, iu, iu, mask, align];
    vs.op(b, "rawBufferStore.i32", Attr::NoUnwind, void, 140, &args);
    for col in 0..4 {
        let args = [vs.m.i32(0), vs.m.i32(0), vs.m.i8(col), vs.m.undef(vs.t.i32)];
        let v = vs.op(b, "loadInput.f32", Attr::ReadNone, vs.t.f32, 4, &args);
        let args = [vs.m.i32(0), vs.m.i32(0), vs.m.i8(col), v];
        vs.op(b, "storeOutput.f32", Attr::NoUnwind, vs.t.void, 5, &args);
    }
    vs.m.terminate(b, Term::Ret);
    let mut dxil = vs.finish();
    let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref())
    else {
        eprintln!("no dxil.dll; skipped");
        return;
    };
    v.validate(&mut dxil).expect("DXIL validation failed");
}

/// A wave32 pixel program that reads the pixel's position (x in v0, y in
/// v1) and exports target 0.
fn pixel(words: &[u32]) -> HostShader {
    let native = NativeProgram {
        stage: HwStage::Ps,
        wave64: false,
        vgprs: 16,
        layout: Vec::new(),
        used: Vec::new(),
        io: StageIo::Ps {
            inputs: Vec::new(),
            input_addr: 1 << 8 | 1 << 9,
            db_shader_control: 0,
            patches: Vec::new(),
        },
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let p = ir::build(&native.code, false, 16).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, &native).expect("io::raise");
    let lowered = lower::lower(&lower::Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: vec![element("SV_Position", 3, 4, 3)],
        outputs: vec![element("SV_Target", 16, 0, 15)],
        prims: Vec::new(),
        mesh_stride: 0,
    })
    .expect("lower");
    host_shader(HwStage::Ps, lowered.dxil)
}

/// The console's waterfall over a value each lane has its own of (here the
/// pixel's number, `x + 8 y`), counting the passes each lane is served in,
/// and the count of the lane across the quad's x (`quad_perm:[1,0,3,2]`),
/// exported as (own, across, own, across).
const WATERFALL_OVER_PIXELS: [u32; 21] = [
    0xBEFE_097E, // s_wqm_b32 exec_lo, exec_lo
    0x7E04_0F00, // v_cvt_u32_f32 v2, v0
    0x7E06_0F01, // v_cvt_u32_f32 v3, v1
    0x3406_0683, // v_lshlrev_b32 v3, 3, v3
    0x4A04_0702, // v_add_nc_u32 v2, v2, v3
    0x7E08_0280, // v_mov_b32 v4, 0
    0xBE84_037E, // s_mov_b32 s4, exec_lo
    0xBE82_037E, // s_mov_b32 s2, exec_lo
    // the loop (byte 32): the first lane's value, the lanes that hold it
    0x7E06_0502, // v_readfirstlane_b32 s3, v2
    0x7DA4_0403, // v_cmpx_eq_u32 exec_lo, s3, v2
    0x4A08_0881, // v_add_nc_u32 v4, 1, v4
    0xBE82_4702, // s_andn2_wrexec_b32 s2, s2
    0xBF89_FFFB, // s_cbranch_execnz (the loop)
    0xBEFE_0304, // s_mov_b32 exec_lo, s4
    0x7E0A_02FA, // v_mov_b32_dpp v5, v4 quad_perm:[1,0,3,2]
    0xFF00_B104,
    0x7E08_0D04, // v_cvt_f32_u32 v4, v4
    0x7E0A_0D05, // v_cvt_f32_u32 v5, v5
    0xF800_180F, // exp mrt0, v4, v5, v4, v5 done vm
    0x0504_0504,
    0xBF81_0000, // s_endpgm
];

#[test]
fn a_helper_lane_is_served_by_a_waterfall_loop_in_a_pass_of_its_own() {
    // The console runs a pixel program's helper lanes as lanes of EXEC
    // (`s_wqm`): every lane of a quad, helper or not, is served once. The
    // host's wave operations leave helper lanes out; a helper lane whose
    // value no other lane held never met it in a first lane.
    let ps = pixel(&WATERFALL_OVER_PIXELS);
    let vs = position_vs();
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut ps.dxil.clone())
            .expect("DXIL validation failed");
        v.validate(&mut vs.dxil.clone())
            .expect("DXIL validation failed");
    }
    // The half of an 8x8 target below its diagonal (pixels with x < y; the
    // diagonal's own are outside, by the top-left rule): in each 2x2 quad
    // on the diagonal, pixel (2i, 2i + 1) is drawn and the three others are
    // helper lanes, the one across x among them.
    let vertices = [
        -1.0, -1.0, 0.5, 1.0, 1.0, -1.0, 0.5, 1.0, -1.0, 1.0, 0.5, 1.0,
    ];
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut ran = 0;
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = host
            .pipeline(&vs, &ps)
            .unwrap_or_else(|e| panic!("{label}: pipeline failed: {e}"));
        let texels = host
            .draw(
                &root,
                &pso,
                &Draw {
                    roots: Vec::new(),
                    textures: &[],
                    samplers: &[],
                    vertices: &vertices,
                    stride: 16,
                    size: (8, 8),
                    mesh_groups: 0,
                },
            )
            .unwrap_or_else(|e| panic!("{label}: draw failed: {e}"));
        for (i, texel) in texels.iter().enumerate() {
            let (x, y) = (i % 8, i / 8);
            let want = if x < y { [1.0; 4] } else { [0.0; 4] };
            assert_eq!(*texel, want, "{label}: pixel ({x}, {y})");
        }
        ran += 1;
    }
    assert!(ran > 0, "no SM 6.6 device");
}
