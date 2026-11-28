# Experimental VIctory-NVIDIA install alongside NeonShim

This is an experiment on a **copy** of NeonShim (`E:\NeonShim-Exp`). The original
repository and working build are untouched.

It installs the upstream [VIctory-NVIDIA](https://github.com/trustno1-coder/VIctory-NVIDIA)
launcher and runtime next to NeonShim so the same game dump can be run with
either layer. No NeonShim source was merged into VIctory-NVIDIA and no
VIctory-NVIDIA source was merged into NeonShim: this is a side-by-side install,
not a code merge.

## Layout

- `scripts/` — launcher scripts from VIctory-NVIDIA (GPL-3.0, see `LICENSE`).
- `target/release/victory.exe` — released runtime binary (downloaded from the
  upstream release, not committed: `.gitignore` excludes executables).
- `target/release/D3D12/` — Microsoft Agility SDK 1.614.1 (`D3D12Core.dll` plus
  its licence), needed on Windows 10.
- `victory.toml` — paths to the user's own game dump and to local cache/data.
- `run-experiment.ps1` — launches the game in Series X profile with DLSS quality.

`nvidia-sdk/` (NVIDIA Streamline/DLSS) and `vic-home/` (caches, saves, logs) are
local only and are excluded from git.

## Run

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\run-experiment.ps1
```

The release asset `victory-experimental.zip` contains `victory.exe` and the
`D3D12` folder with its licence.
