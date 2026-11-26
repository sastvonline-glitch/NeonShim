//! Console wave64 compute programs on a host of 32-lane waves, in the paired form and in the
//! forms that run a console wave as two host waves of its own, checked against what the
//! console's 64 lanes compute: hand-written programs, lowered, validated by `dxil.dll` when
//! present, and run on the host GPU when it takes waves of 32.

use super::host::{Dispatch, Host, Root, View};
use super::{HostShader, lower, validator};
use crate::native::{ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource};
use crate::{io, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;

// Operands as the hardware numbers them: SGPR n is n, VCC 106, EXEC 126, an inline integer n
// 128 + n, 0.5 240, a literal (the next dword) 255, VGPR n 256 + n.
const VCC: u32 = 106;
const EXEC: u32 = 126;
const HALF: u32 = 240;
const LIT: u32 = 255;

const fn v(n: u32) -> u32 {
    256 + n
}

const fn k(n: u32) -> u32 {
    128 + n
}

const fn sop1(op: u32, sdst: u32, src0: u32) -> u32 {
    0xBE80_0000 | sdst << 16 | op << 8 | src0
}

const fn sop2(op: u32, sdst: u32, src0: u32, src1: u32) -> u32 {
    0x8000_0000 | op << 23 | sdst << 16 | src1 << 8 | src0
}

/// A branch by `imm` dwords from the next instruction.
const fn sopp(op: u32, imm: i16) -> u32 {
    0xBF80_0000 | op << 16 | imm as u16 as u32
}

const fn vop1(op: u32, vdst: u32, src0: u32) -> u32 {
    0x7E00_0000 | vdst << 17 | op << 9 | src0
}

const fn vop2(op: u32, vdst: u32, src0: u32, vsrc1: u32) -> u32 {
    op << 25 | vdst << 17 | vsrc1 << 9 | src0
}

const fn vopc(op: u32, src0: u32, vsrc1: u32) -> u32 {
    0x7C00_0000 | op << 17 | vsrc1 << 9 | src0
}

const fn vop3(op: u32, dst: u32, src0: u32, src1: u32) -> [u32; 2] {
    [0xD400_0000 | op << 16 | dst, src0 | src1 << 9]
}

/// `buffer_store_dword v<data>, v<addr>, s[8:11], 0 offen offset:<offset>`
const fn store(data: u32, addr: u32, offset: u32) -> [u32; 2] {
    [0xE070_1000 | offset, 0x8002_0000 | data << 8 | addr]
}

/// `buffer_atomic_add v<data>, v<addr>, s[8:11], 0 offen glc`: the old dword returns in
/// `v<data>`.
const fn atomic_add(data: u32, addr: u32) -> [u32; 2] {
    [0xE0C8_5000, 0x8002_0000 | data << 8 | addr]
}

/// `regs` stored as each thread's dwords of the UAV, the thread's index (v0) times 32 on.
fn stores(regs: [u32; 8]) -> Vec<u32> {
    let mut w = vec![vop2(0x1A, 15, k(5), 0)]; // v_lshlrev_b32 v15, 5, v0
    for (n, r) in regs.into_iter().enumerate() {
        w.extend(store(r, 15, 4 * n as u32));
    }
    w
}

/// A wave64 compute program of `threads` threads (the thread index in v0) with 4 root
/// constants in s0..s3 and a raw UAV's descriptor in s[8:11], then `body`.
fn native(body: &[u32], threads: u32) -> NativeProgram {
    let mut words = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
        125 << 25, // s_load_dwordx4 s[8:11], s[4:5], 0
    ];
    words.extend_from_slice(body);
    words.push(S_ENDPGM);
    NativeProgram {
        stage: HwStage::Cs,
        wave64: true,
        vgprs: 16,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1, // 5 user SGPRs: s0..s4
            threads: [threads, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// `native` in the paired form on a host of 32-lane waves, validated by `dxil.dll` when present.
fn paired(native: &NativeProgram) -> HostShader {
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let res_st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&res_st)).expect("resource::track");
    let io = io::raise(&p, native).expect("io::raise");
    let bytes = crate::dxbc::build(&[]);
    let c = super::Container::parse(&bytes).expect("container");
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let shader = super::paired(&c, native, &p, &io, &res);
    lower::TEST_HOST_WAVE.set(was);
    let (shader, wave) = shader.expect("the paired form");
    assert_eq!(wave, Some(32));
    assert_eq!(super::forms::form_of(&shader.dxil), super::forms::PAIRED);
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    shader
}

/// `native` as [`super::emit`] gives it to a host of 32-lane waves, with the form it took;
/// validated by `dxil.dll` when present.
fn chosen(native: &NativeProgram) -> (HostShader, &'static str) {
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resource::track");
    let io = io::raise(&p, native).expect("io::raise");
    let bytes = crate::dxbc::build(&[]);
    let c = super::Container::parse(&bytes).expect("container");
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let shader = super::emit(&c, native, &p, &st, &io, &res);
    lower::TEST_HOST_WAVE.set(was);
    let shader = shader.expect("emit");
    let form = super::forms::form_of(&shader.dxil);
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    (shader, form)
}

/// `shader` on the hardware device: one group, root constants `s0..s3`, a raw UAV of `dwords`
/// zeroed dwords, which come back; None (and a line) without a device that takes it.
fn run(shader: &HostShader, constants: [u32; 4], dwords: u32) -> Option<Vec<u32>> {
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 hardware device; not run");
        return None;
    };
    let (root, pso) = match host.compute_pipeline(shader) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("the hardware device does not take this program: {e}; not run");
            return None;
        }
    };
    let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    for u in &shader.user_data {
        if let (RootType::Constants, 0, d @ 0..=3) = (u.root, u.slot, u.dword) {
            ud[u.index as usize] = constants[d as usize];
        }
    }
    let mut out = host
        .dispatch(
            &root,
            &pso,
            &Dispatch {
                roots: vec![(0, Root::Constants(ud))],
                views: &[View::RawUav(dwords)],
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
    Some(out.swap_remove(0))
}

/// Panics at the first dword of `got` that is not the console's (`want`), naming the thread
/// and which of its `per` dwords.
fn same_as_console(got: &[u32], want: &[u32], per: usize) {
    assert_eq!(got.len(), want.len());
    if let Some(n) = (0..want.len()).find(|&n| got[n] != want[n]) {
        panic!("thread {}, dword {}: {:#x}, the console's {:#x}", n / per, n % per, got[n], want[n]);
    }
}

/// A branch to the word at index `to` of `w`, placed at index `at` (filled in later).
fn branch_to(w: &mut [u32], at: usize, op: u32, to: usize) {
    w[at] = sopp(op, (to as isize - at as isize - 1) as i16);
}

/// One console wave of 64 threads: values written under one guard (EXEC of a per-lane branch,
/// which the paired form takes wave-wide) and read under the same guard, a narrower one in the
/// same block (`v_cmpx`), a narrower one in a nested branch, a wider one after it (where the
/// lanes outside the narrower guard must read what they held), and by every lane past the
/// branch (where the lanes outside it keep their old values); floats and an accumulator
/// (`v_fmac_f32`) too. In the paired form a reader whose guard implies the writer's takes the
/// writer's value without its select (`guarded`); the others keep the select.
#[test]
fn paired_lanes_read_guarded_writes_as_the_console() {
    let mut w = Vec::new();
    w.extend(vop3(0x169, 1, v(0), k(3))); // v_mul_lo_u32 v1, v0, 3
    w.push(vop2(0x25, 1, k(1), 1)); // v_add_nc_u32 v1, 1, v1
    w.extend([vop1(0x01, 2, LIT), 0x55]); // v_mov_b32 v2, 0x55
    for r in [3, 4, 9, 12] {
        w.push(vop1(0x01, r, k(0))); // v_mov_b32 v<r>, 0
    }
    w.push(vop1(0x01, 5, k(7))); // v_mov_b32 v5, 7
    w.push(vop1(0x01, 7, k(9))); // v_mov_b32 v7, 9
    w.push(vopc(0xC4, k(40), 0)); // v_cmp_gt_u32 vcc, 40, v0: lanes below 40
    w.push(sop1(0x24, 12, VCC)); // s_and_saveexec_b64 s[12:13], vcc
    let skip1 = w.len();
    w.push(0); // s_cbranch_execz past the branch
    w.push(vop2(0x25, 2, k(50), 1)); // v_add_nc_u32 v2, 50, v1
    w.push(vop2(0x1A, 3, k(1), 2)); // v_lshlrev_b32 v3, 1, v2
    w.push(vop1(0x06, 10, v(2))); // v_cvt_f32_u32 v10, v2
    w.push(vop2(0x08, 11, HALF, 10)); // v_mul_f32 v11, 0.5, v10
    w.push(vop2(0x2B, 11, v(10), 10)); // v_fmac_f32 v11, v10, v10
    w.push(vop1(0x07, 12, v(11))); // v_cvt_u32_f32 v12, v11
    w.push(sop1(0x04, 16, EXEC)); // s_mov_b64 s[16:17], exec
    w.push(vopc(0xD4, k(30), 0)); // v_cmpx_gt_u32 exec, 30, v0: lanes below 30
    w.extend(vop3(0x169, 7, v(2), k(5))); // v_mul_lo_u32 v7, v2, 5
    w.push(sop1(0x04, EXEC, 16)); // s_mov_b64 exec, s[16:17]: lanes below 40 again
    w.push(vop2(0x25, 9, v(7), 3)); // v_add_nc_u32 v9, v7, v3
    w.push(vopc(0xC1, k(20), 0)); // v_cmp_lt_u32 vcc, 20, v0: lanes above 20
    w.push(sop1(0x24, 14, VCC)); // s_and_saveexec_b64 s[14:15], vcc
    let skip2 = w.len();
    w.push(0); // s_cbranch_execz past the nested branch
    w.push(vop2(0x1D, 4, v(2), 3)); // v_xor_b32 v4, v2, v3
    w.push(vop2(0x25, 2, v(4), 9)); // v_add_nc_u32 v2, v4, v9
    let end2 = w.len();
    w.push(sop2(0x11, EXEC, EXEC, 14)); // s_or_b64 exec, exec, s[14:15]
    w.push(vop2(0x25, 5, k(3), 2)); // v_add_nc_u32 v5, 3, v2
    let end1 = w.len();
    w.push(sop2(0x11, EXEC, EXEC, 12)); // s_or_b64 exec, exec, s[12:13]
    w.push(vop2(0x25, 6, v(2), 5)); // v_add_nc_u32 v6, v2, v5
    branch_to(&mut w, skip1, 0x08, end1);
    branch_to(&mut w, skip2, 0x08, end2);
    w.extend(stores([2, 3, 4, 5, 6, 7, 9, 12]));
    let shader = paired(&native(&w, 64));
    let want: Vec<u32> = (0..64u32)
        .flat_map(|n| {
            let v1 = 3 * n + 1;
            let (mut v2, mut v3, mut v4, mut v5, mut v7, mut v9, mut v12) = (0x55, 0, 0, 7, 9, 0, 0);
            if n < 40 {
                v2 = v1 + 50;
                v3 = v2 << 1;
                let f = v2 as f32;
                v12 = (f * 0.5 + f * f) as u32;
                if n < 30 {
                    v7 = v2 * 5;
                }
                v9 = v7 + v3;
                if n > 20 {
                    v4 = v2 ^ v3;
                    v2 = v4 + v9;
                }
                v5 = v2 + 3;
            }
            [v2, v3, v4, v5, v2 + v5, v7, v9, v12]
        })
        .collect();
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(got) = run(&shader, [0; 4], 64 * 8) else { return };
    same_as_console(&got, &want, 8);
    eprintln!("ran on the hardware device: guarded writes read as the console reads them");
}

/// One console wave of 64 threads, the threads with (37 t & 7) < s0 requesting a slot each, as
/// the title's light-probe allocation requests do: the first requesting lane counts the
/// requests (`s_bcnt1`), sums that count over the wave in a whole-wave section (only it holds
/// one), then reuses the register that held the count (`v_mbcnt_lo`, under its own EXEC), and
/// reserves the sum with an atomic add on dword 0; every requesting lane takes the base
/// (`v_readfirstlane_b32`) plus its rank (`v_mbcnt` of the requests) and stores its thread
/// index there, one dword past the counter.
///
/// The allocation idiom ([`super::idiom::wave_local`]), which lets each host wave reserve for
/// its own half; it was taken as mixing the count into the reused register in the lanes
/// outside that register's EXEC, which no lane reads (`split::read_inside_only`), and the
/// program got the exchange form (four group barriers a first-lane read) or the paired form.
/// Now each host wave on its own: the counter holds the number of requests and each request
/// is stored once (in an order of the halves' reservations), as on the console.
#[test]
fn an_allocation_reusing_its_count_register_takes_each_host_wave_on_its_own() {
    let sum: Vec<u32> =
        super::wave::WAVE_OR_V7_TO_S7.iter().map(|&w| if w >> 25 == 28 { w & 0x01FF_FFFF | 0x25 << 25 } else { w }).collect();
    let mut w = Vec::new();
    w.extend(vop3(0x169, 1, v(0), k(37))); // v_mul_lo_u32 v1, v0, 37
    w.push(vop2(0x1B, 1, k(7), 1)); // v_and_b32 v1, 7, v1
    w.push(vopc(0xC4, 0, 1)); // v_cmp_gt_u32 vcc, s0, v1: the requests
    w.push(sop1(0x24, 12, VCC)); // s_and_saveexec_b64 s[12:13], vcc
    let skip = w.len();
    w.push(0); // s_cbranch_execz to the end
    w.push(sop1(0x04, 14, EXEC)); // s_mov_b64 s[14:15], exec
    w.extend(vop3(0x365, 3, 14, k(0))); // v_mbcnt_lo_u32_b32 v3, s14, 0
    w.extend(vop3(0x366, 3, 15, v(3))); // v_mbcnt_hi_u32_b32 v3, s15, v3: the rank
    w.extend(vop3(0x365, 4, 193, k(0))); // v_mbcnt_lo_u32_b32 v4, -1, 0
    w.extend(vop3(0x366, 4, 193, v(4))); // v_mbcnt_hi_u32_b32 v4, -1, v4: the lane
    w.push(sop1(0x14, 16, EXEC)); // s_ff1_i32_b64 s16, exec: the first request
    w.push(vop1(0x01, 7, k(0))); // v_mov_b32 v7, 0
    w.push(sop1(0x04, 18, EXEC)); // s_mov_b64 s[18:19], exec
    w.push(vopc(0x92, 16, 4)); // v_cmpx_eq_i32 exec, s16, v4: its lane
    let skip2 = w.len();
    w.push(0); // s_cbranch_execz past the reservation
    w.push(sop1(0x10, 20, 14)); // s_bcnt1_i32_b64 s20, s[14:15]: the count
    w.push(vop1(0x01, 7, 20)); // v_mov_b32 v7, s20
    w.extend(&sum); // the wave's sum of v7 into s7
    w.extend(vop3(0x365, 7, 193, k(0))); // v_mbcnt_lo_u32_b32 v7, -1, 0: v7 held the count
    w.extend(vop3(0x366, 9, 193, v(7))); // v_mbcnt_hi_u32_b32 v9, -1, v7
    w.push(vop1(0x01, 7, 7)); // v_mov_b32 v7, s7
    w.push(vop1(0x01, 10, k(0))); // v_mov_b32 v10, 0
    w.extend(atomic_add(7, 10)); // buffer_atomic_add v7, v10, s[8:11], 0 offen glc: the base
    let join = w.len();
    w.push(sop1(0x04, EXEC, 18)); // s_mov_b64 exec, s[18:19]: the requests
    w.push(vop1(0x02, 21, v(7))); // v_readfirstlane_b32 s21, v7
    w.push(vop2(0x25, 6, 21, 3)); // v_add_nc_u32 v6, s21, v3: the slot
    w.push(vop2(0x1A, 6, k(2), 6)); // v_lshlrev_b32 v6, 2, v6
    w.push(vop2(0x25, 6, k(4), 6)); // v_add_nc_u32 v6, 4, v6: past the counter
    w.extend(store(0, 6, 0)); // buffer_store_dword v0, v6, s[8:11], 0 offen
    let end = w.len();
    w.push(sop1(0x04, EXEC, 12)); // s_mov_b64 exec, s[12:13]
    branch_to(&mut w, skip, 0x08, end);
    branch_to(&mut w, skip2, 0x08, join);
    let native = native(&w, 64);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    super::idiom::wave_local(&p, &st).expect("the allocation idiom");
    let (local, form) = chosen(&native);
    assert_eq!(form, super::forms::LOCAL, "each host wave on its own");
    let pairs = paired(&native);
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for s0 in [3u32, 8, 1, 0] {
        let requests: Vec<u32> = (0..64).filter(|&t| ((37 * t) & 7) < s0).collect();
        for (shader, name) in [(&local, "each host wave on its own"), (&pairs, "paired")] {
            let Some(got) = run(shader, [s0, 0, 0, 0], 65) else { return };
            assert_eq!(got[0] as usize, requests.len(), "{name}, s0 {s0}: the counter");
            let mut stored: Vec<u32> = got[1..=requests.len()].to_vec();
            if name == "paired" {
                // one console wave: its own order, the rank's
                assert_eq!(stored, requests, "{name}, s0 {s0}: in the console's order");
            }
            stored.sort_unstable();
            assert_eq!(stored, requests, "{name}, s0 {s0}: each request once");
            assert!(got[1 + requests.len()..].iter().all(|&x| x == 0), "{name}, s0 {s0}: nothing past the count");
        }
    }
    eprintln!("ran on the hardware device: the requests reserved by each host wave, each stored once");
}
