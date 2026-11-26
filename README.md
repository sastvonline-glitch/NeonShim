# NeonShim

NeonShim runs Xbox Series X|S code natively on a Windows PC with an NVIDIA
card. There is no CPU emulation: the title's x86-64 code runs as is inside
our process. NeonShim provides the console platform underneath it (kernel,
runtime services, files, input, audio) and translates the console's GPU work
to Direct3D 12. Shaders are recompiled from the console's GPU code to DXIL
the first time they are used.

So far it has been tested with **one title only**. Other games will need
their own work. Don't expect anything else to boot.

**This repository contains no game files, keys, executables or assets, and
links to none.** You need your own copy. NeonShim is not affiliated with or
endorsed by any game developer, publisher or Microsoft.

## Status

Work in progress. On the test machine the title boots, runs its menus at
30 fps, loads into the world and is playable at low frame rates. Shadowed
areas are still too dark (the title's ray-traced lighting doesn't run yet).

Test machine: Windows 10 LTSC 21H2, AMD Ryzen 7 2700, NVIDIA RTX 3080 Ti
12 GB, 32 GB RAM, Series X profile.

| Area | State |
|------|-------|
| Boot, menus, saves | work |
| Intro video and sound | work |
| The world | renders and plays; slow |
| Shader translation | 25,948 of 25,953 programs translate and validate offline |
| Ray tracing | translated, not lit by yet; off by default |
| AMD cards | not tested |

Measured on the test machine (self-reported, `scripts/fps-report.py` over
the session log, `scripts/play.cmd` preset, warm shader cache):

| Metric | Value |
| --- | --- |
| Time in the world | 13 min 20 s, no crash or device removal |
| Steady fps (after 10 min) | 4.9 mean, 4.8 at the 10th percentile |
| Steady frame time, median | 203 ms |
| Draws per frame, median | 3,424 |

## What's different here

The base is fxceless/victory-fork on VIctory (see [Credits](#credits)).
NeonShim is about NVIDIA under Windows 10.

### Our own fixes

Found by running the shader tests on an NVIDIA card under Windows 10, where
they used to skip:

- **Buffer sizes read from a descriptor.** A program that reads a
  structured buffer's element count straight from its descriptor (the
  title's water simulation does) got a raw buffer handle over a structured
  view. That is undefined in D3D12: AMD happens to answer bytes, NVIDIA
  answers 4-byte words, so the count came out wrong (2 instead of 9 in the
  test). The handle is now structured, so every card answers in elements.
- **An immediate sampler chosen by a branch.** The title's terrain programs
  pick a clamping or a wrapping sampler on a uniform condition. Translated as
  an indexed range of static samplers, the RTX 3080 Ti always sampled with
  the first one, whichever arm ran. Each arm now gets its own copy of the
  join with a constant sampler. `NEONSHIM_SAMPLER_RANGE=1` brings the old
  form back, for comparison (`scripts\play.cmd old-sampler`).
- **GPU tests that run on Windows 10.** The test executables now load the
  Agility SDK runtime like `neonshim.exe` does. Before, 39 shader tests and
  the GPU pipeline tests failed or skipped on Windows 10 for want of shader
  model 6.6, and so never checked anything there.
- **Merge leftovers.** Tests that no longer matched the code after merging
  several forks, a device id cut one byte short, duplicate blend fixes, and
  dead code, cleaned up so `cargo clippy -D warnings` and the tests pass.

### Merged from other forks

- **Windows 10 support.** `cargo xtask agility` puts Microsoft's Agility
  SDK runtime next to the exe, since the built-in D3D12 there stops at
  shader model 6.5.
- **Wave64 on 32-lane GPUs** (NitricAcidDev). Console compute programs
  written for 64 lanes run in a paired form or as two host waves where that
  is safe. Turn off with `NEONSHIM_WAVE_PAIRS=0` / `NEONSHIM_WAVE_SPLIT=0`.
- **Geometry shaders** (NitricAcidDev). The 195 voxelization stages run as
  host geometry shaders. Mesh programs over D3D12's 32 KiB output limit are
  linked with their pixel program (`NEONSHIM_MESH_LINK=0` to turn off).
- **Textures.** BCPack BC7 dictionary fix, 3D mip tail and depth pitch
  fixes, sRGB on compressed materials, streaming fixes for late blocks and
  remapped memory.
- **Large dispatches.** More than 65,535 groups in x are split and run
  whole instead of being cut off.

Most of the speed-related changes are behind environment switches. Each one
is described in a comment where it is read. `scripts\play.cmd` sets the ones
that worked best on the test machine.

## Getting started

Requirements:

- Windows 10 or 11, x64, CPU with AVX2
- NVIDIA card with shader model 6.6 and mesh shaders (RTX 20 series or newer)
- Rust (version pinned in `rust-toolchain.toml`)
- Visual Studio Build Tools with the C++ workload
- Several GiB of free Windows commit at startup. If `crashes.log` shows
  Windows error 1455, close other programs or grow the paging file.

```powershell
cargo xtask install-hooks                  # asset-policy check on every commit
copy neonshim.example.toml neonshim.toml   # then set your paths (outside the repo)
cargo xtask agility                        # Windows 10 only
cargo test --workspace                     # needs no game data
cargo run -p neonshim -- config            # check your paths
```

Build and run:

```powershell
cargo build --release -p neonshim
target\release\neonshim.exe warm --jobs 16 <game dir>\shaders.rpf   # optional, recommended
target\release\neonshim.exe run <game dir>\<the title's exe> --execute
```

The window caption shows fps, frame time and draw count. Saves, settings and
`crashes.log` live under `%LOCALAPPDATA%\neonshim\data`.

### With the NVIDIA preset

Put your game folder in `neonshim.cfg` at the repo root (it is ignored by
git):

```
GAME=D:\path\to\your\copy
```

Then:

```powershell
scripts\play.cmd warm          # once: translate every shader ahead of time
scripts\play.cmd               # play; the log goes to logs\
scripts\play.cmd old-sampler   # the same, with the old sampler form, to compare
```

When the game exits the script prints a summary of the session
(`scripts/fps-report.py`: time in the world, mean and steady fps, crashes).
Run it on any log by hand with `python scripts\fps-report.py <log>`.

### Input

XInput pads work. With no pad connected the keyboard acts as one while the
window has focus:

| Keys | Pad |
|------|-----|
| W A S D / I J K L | left / right stick |
| arrows | D-pad |
| Enter, Backspace, X, Y | A, B, X, Y |
| Q / E, Z / C | shoulders, triggers |
| F / H | stick clicks |
| Esc, Tab | Menu, View |

## Commands

```
neonshim inspect <exe|dll> [--imports] [--exports]
neonshim census [paths...] [--missing] [--strict]
neonshim lib-exports <import.lib> [--list]
neonshim shader-info [paths...] [--verbose] [--disasm]
neonshim translate [paths...] [--validate] [--pipelines] [--list] [--jobs N]
neonshim warm [--jobs N] <shader pack>
neonshim run <exe> [--execute] [--unimplemented trap|zero] [--strict]
             [--console series-x|series-s] [-- <title arguments>]
neonshim config
```

Logging: `NEONSHIM_LOG=neonshim_loader=trace,info`. Release builds log info
and above.

## Debugging

Release builds have no debugging aids. Build them in with:

```powershell
cargo build --profile diag -p neonshim --features diagnostics   # target\diag\neonshim.exe
```

| Variable | Effect |
|----------|--------|
| `NEONSHIM_LOG=<target>=debug,...` | debug and trace output |
| `NEONSHIM_STACKS=20,60` | dump every thread's stack at those seconds |
| `NEONSHIM_WATCH=<rva>,...` | log a title u32 when it changes |
| `NEONSHIM_KEYS=30:Enter,35:X` | press keys at those seconds |
| `NEONSHIM_CRASH_SAVE=<rva>:<len>,...` | save title memory on a crash |
| `NEONSHIM_CAPTURE=<dir>` | save presented frames |
| `NEONSHIM_DUMP_TARGETS=1` | with the above, save every render target of those frames |
| `NEONSHIM_D3D12_DEBUG=1` | D3D12 debug layer |
| `NEONSHIM_D3D12_GBV=1` | with the above, GPU-based validation (slow) |
| `NEONSHIM_D3D12_DRED=1` | on a GPU fault, the allocation hit and the commands in flight |
| `NEONSHIM_D3D12_WARP=1` | software adapter (slow, but can't reset the driver) |
| `NEONSHIM_PIPELINE_WAIT_MS=<ms>` | per-frame wait for pipelines still being built (default 500) |
| `NEONSHIM_SKIP_PIPELINES=<hash>,...` | skip draws whose pipeline has one of these shader hashes |
| `NEONSHIM_GPU_TIMING=1` | time each list on its own and log the slow ones |
| `NEONSHIM_SHADER_LOOP_LIMIT=<n>` | cap data-driven shader loops at `n` iterations (`pixel:<n>,compute:<n>,...` per stage) |
| `NEONSHIM_LOOP_NOTES=1` | count lanes that hit a loop limit and log them every 5 s |
| `NEONSHIM_AUDIO_CAPTURE=<file.wav>` | record what the title plays |
| `NEONSHIM_XMA_DUMP=<dir>` | save the first 32 XMA voices' context and input |

Every build appends crashes and GPU failures to `crashes.log`.

How it fits together: [docs/architecture.md](docs/architecture.md). Ray tracing:
[docs/ray-tracing.md](docs/ray-tracing.md).

## Credits

NeonShim stands on other people's work. Thank you all.

- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and
  contributors: the loader, runtime services, D3D12 back end and shader
  recompiler. Everything here starts from it.
- **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)**: the
  fork this one follows, with most of the world rendering fixes.
- **[uiliannn/VItoriaBr](https://github.com/uiliannn/VItoriaBr)**: shader
  translator and pipeline fixes.
- **[NitricAcidDev/VIctory-Omega-Fork](https://github.com/NitricAcidDev/VIctory-Omega-Fork)**:
  wave64 paired form, geometry shaders, loop limits per stage, large dispatches.
- **[Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD)**:
  pipeline library and descriptor work.
- **[myokex/victory-amd](https://github.com/myokex/victory-amd)**: guard over
  every view of a memory page, texture copy-back.
- **[Zettrox/ztrx](https://github.com/Zettrox/ztrx)**: resource fixes.
- **Pilote Production**, **Joshua Odom**, **Spexivus**, **joroyt**,
  **tightfall** and everyone else whose commits are merged here.
- **[XCL](https://github.com/ivall/xcl)** by ivall (GPL-2.0-only) was used as
  a reference for how the same title behaves under another layer. No code was
  copied from it.

Code taken from a specific commit is marked with its source in the file
header.

## License

GPL-3.0-or-later, the same as VIctory. See [LICENSE](LICENSE). Some files
carry their own compatible licenses (MIT, LGPL-2.1-or-later), noted at the
top of each.
