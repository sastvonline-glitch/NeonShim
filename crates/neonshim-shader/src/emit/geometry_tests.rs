//! End-to-end tests of geometry programs (`crate::geometry`): a program of
//! the console's form, written for the tests, translated to a geometry
//! shader, validated by `dxil.dll` when present, and drawn on the host GPU
//! between a vertex and a pixel shader of its own.

use windows::Win32::Graphics::Direct3D12::{
    D3D12_SHADER_VISIBILITY_GEOMETRY, D3D12_SHADER_VISIBILITY_PIXEL, D3D12_SHADER_VISIBILITY_VERTEX,
};

use super::bitcode::{Attr, Term};
use super::container::SigElement;
use super::host::{Draw, Host};
use super::shader::Shader;
use super::{HostShader, emit, validator};
use crate::dxbc::{Container, FourCC};
use crate::geometry::{SWAP_XY, SWAP_XY_RING};
use crate::native::{HwStage, NativeProgram, StageIo};
use crate::{io, resource, structure};

fn element(name: &str, kind: u8, row: u32) -> SigElement {
    SigElement {
        name: name.into(),
        index: 0,
        kind,
        comp: 9,
        interp: 0,
        row: Some(row),
        col: 0,
        cols: 4,
        usage: 0,
        half: false,
    }
}

/// A position in register 0 and a float4 in register 1.
fn registers() -> Vec<SigElement> {
    vec![element("SV_Position", 3, 0), element("TEXCOORD", 0, 1)]
}

/// The geometry program `words` translated (its I/O as raised too): its
/// signatures both [`registers`], its ring [`SWAP_XY_RING`]'s.
fn translate(words: &[u32]) -> (HostShader, io::Io) {
    let native = NativeProgram {
        stage: HwStage::Gs,
        wave64: true,
        vgprs: 40,
        layout: vec![],
        used: vec![],
        io: StageIo::Gs(SWAP_XY_RING),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let isg1 = super::container::signature(&registers(), true);
    let osg1 = super::container::signature(&registers(), false);
    let bytes = crate::dxbc::build(&[(FourCC::ISG1, &isg1), (FourCC::OSG1, &osg1)]);
    let c = Container::parse(&bytes).unwrap();
    let p = crate::spill::build(&native).unwrap();
    let st = structure::structure(&p).unwrap();
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).unwrap();
    let io = io::raise(&p, &native).unwrap();
    (emit(&c, &native, &p, &st, &io, &res).unwrap(), io)
}

/// [`SWAP_XY`] translated.
fn translated() -> HostShader {
    translate(&SWAP_XY).0
}

fn validate(shader: &HostShader) {
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone()).unwrap();
    }
}

/// The program is a geometry shader of triangles in and three strip
/// vertices out, its registers on both sides; the validator, where there
/// is one, accepts it.
#[test]
fn a_geometry_program_is_a_geometry_shader() {
    let gs = translated();
    assert_eq!(gs.stage, HwStage::Gs);
    let c = Container::parse(&gs.dxil).unwrap();
    // Program version: geometry (2), shader model 6.6.
    assert_eq!(c.part(FourCC::DXIL).unwrap().data[..4], [0x66, 0, 2, 0]);
    let psv = c.part(FourCC::PSV0).unwrap().data;
    // Triangles in, triangle strips out, stream 0, a position.
    assert_eq!(psv[4..17], [3, 0, 0, 0, 5, 0, 0, 0, 1, 0, 0, 0, 1]);
    // Geometry stage, three vertices, two inputs, two outputs.
    assert_eq!(psv[28..34], [2, 0, 3, 0, 2, 2]);
    validate(&gs);
}

/// With the lane's index in v9, v2 is the primitive id the hardware gives:
/// written as vertex 0's last channel, it is the host's primitive id.
#[test]
fn a_geometry_program_reads_its_primitive_id() {
    let mut words = SWAP_XY;
    words[10] = 0xD765_0009; // v_mbcnt_lo_u32_b32 v9, -1, 0
    [words[12], words[13]] = [0xD766_0009, 0x0002_12C1]; // v_mbcnt_hi_u32_b32 v9, -1, v9
    words[27] = 0x0002_12FF; // v_mad_u32_u24 v8, lit(96), v9, s0
    words[47] = 0x0002_1008; // ds_write2_b32 v8, v16, v2 offset0:6 offset1:7
    let (gs, io) = translate(&words);
    assert_eq!(io.inputs.len(), 1);
    assert_eq!(io.inputs[0].1, io::Input::PrimitiveId);
    validate(&gs);
}

/// A vertex shader that passes a position and a float4 through, and a
/// pixel shader that writes the float4.
fn pass_through() -> (HostShader, HostShader) {
    let position = SigElement {
        interp: 4,
        usage: 15,
        ..element("SV_Position", 3, 0)
    };
    let coord = SigElement {
        interp: 2,
        usage: 15,
        ..element("TEXCOORD", 0, 1)
    };
    let mut vs = Shader::new(false);
    vs.inputs = vec![
        SigElement {
            usage: 15,
            ..element("POSITION", 0, 0)
        },
        SigElement {
            interp: 0,
            ..coord.clone()
        },
    ];
    vs.outputs = vec![position.clone(), coord.clone()];
    let mut ps = Shader::new(true);
    ps.inputs = vec![
        SigElement {
            usage: 0,
            ..position
        },
        coord,
    ];
    ps.outputs = vec![SigElement {
        usage: 15,
        ..element("SV_Target", 16, 0)
    }];
    // Vertex: output id from input id. Pixel: target 0 from input 1.
    for (s, copies) in [(&mut vs, &[(0, 0), (1, 1)][..]), (&mut ps, &[(1, 0)][..])] {
        let b = s.entry;
        for &(from, to) in copies {
            for col in 0..4 {
                let args = [s.m.i32(from), s.m.i32(0), s.m.i8(col), s.m.undef(s.t.i32)];
                let v = s.op(b, "loadInput.f32", Attr::ReadNone, s.t.f32, 4, &args);
                let args = [s.m.i32(to), s.m.i32(0), s.m.i8(col), v];
                s.op(b, "storeOutput.f32", Attr::NoUnwind, s.t.void, 5, &args);
            }
        }
        s.m.terminate(b, Term::Ret);
    }
    let host = |stage, s: Shader| HostShader {
        stage,
        dxil: s.finish(),
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
    };
    (host(HwStage::Vs, vs), host(HwStage::Ps, ps))
}

/// A triangle over the whole target whose float4 is its position: drawn
/// through the translated program, every texel holds its position with x
/// and y swapped, as the program writes the float4; without it, as it is.
#[test]
fn a_geometry_program_draws_on_the_card() {
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 device; skipped");
        return;
    };
    let (vs, ps) = pass_through();
    let gs = translated();
    let root = host
        .root_signature_for(&[
            (&vs, D3D12_SHADER_VISIBILITY_VERTEX),
            (&gs, D3D12_SHADER_VISIBILITY_GEOMETRY),
            (&ps, D3D12_SHADER_VISIBILITY_PIXEL),
        ])
        .unwrap();
    let vertex = |(x, y): (f32, f32)| [x, y, 0.5, 1.0, x, y, 0.0, 1.0];
    let vertices = [(-1.0, 1.0), (3.0, 1.0), (-1.0, -3.0)].map(vertex).concat();
    let draw = |gs: Option<&[u8]>| {
        let pso = host
            .pipeline_with_gs(&root, &vs.dxil, gs, &ps.dxil)
            .unwrap();
        let d = Draw {
            roots: Vec::new(),
            textures: &[],
            samplers: &[],
            vertices: &vertices,
            stride: 32,
            size: (8, 8),
            mesh_groups: 0,
        };
        host.draw(&root, &pso, &d).unwrap()
    };
    for (with, texels) in [(true, draw(Some(&gs.dxil[..]))), (false, draw(None))] {
        for (i, texel) in texels.iter().enumerate() {
            let (x, y) = ((i % 8) as f32, (i / 8) as f32);
            let (u, v) = ((x + 0.5) / 4.0 - 1.0, 1.0 - (y + 0.5) / 4.0);
            let want = if with {
                [v, u, 0.0, 1.0]
            } else {
                [u, v, 0.0, 1.0]
            };
            assert!(
                texel.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-4),
                "with the program {with}: texel {i} is {texel:?}, not {want:?}"
            );
        }
    }
}
