@echo off
rem NeonShim launcher, NVIDIA preset (tested on an RTX 3080 Ti, Windows 10).
rem
rem   scripts\play.cmd               run the title
rem   scripts\play.cmd warm          translate every shader into the cache first (do this once)
rem   scripts\play.cmd old-sampler   run with the old indexed sampler range, to compare
rem
rem The game folder comes from neonshim.cfg next to this repo's root (not committed):
rem   GAME=D:\path\to\your\copy
rem   EXE=name_of_the_exe.exe        (optional; the first .exe in GAME otherwise)
cd /d "%~dp0.."
if exist neonshim.cfg for /f "usebackq tokens=1,* delims==" %%a in ("neonshim.cfg") do (
  if /i "%%a"=="GAME" set "GAME=%%b"
  if /i "%%a"=="EXE" set "EXE_NAME=%%b"
)
if "%GAME%"=="" ( echo Set GAME=^<your game folder^> in neonshim.cfg & exit /b 1 )
if not exist target\release\neonshim.exe ( echo Build first: cargo build --release -p neonshim ^&^& cargo xtask agility & exit /b 1 )

rem --- submission and waits ---
set NEONSHIM_ASYNC_SUBMIT=1
set NEONSHIM_QUEUE2=1
set NEONSHIM_INDIRECT_WINDOW=1
set NEONSHIM_LATE_WAIT=3
set NEONSHIM_WIDE_BACK=1
set NEONSHIM_MERGE_RUNS=4096
rem a texture refresh waits for the runtime's remapping (1 gave rainbow, pixelated skin)
set NEONSHIM_TEXTURE_NO_WAIT=0
rem --- memory on a 12 GB card ---
set NEONSHIM_CARD_ROOM_FOR_TARGETS_MB=2048
set NEONSHIM_TEXTURE_STAGE_MB=128
set NEONSHIM_LEVELS_ON_DEMAND=1
set NEONSHIM_DYNAMIC_SLOTS=524288
rem --- copies ---
set NEONSHIM_WIDE_REFRESH=1
set NEONSHIM_WIDE_MERGE=1
set NEONSHIM_JOIN_COPIES=1
set NEONSHIM_COPY_PLAIN=1
set NEONSHIM_JOIN_PARTS=1
set NEONSHIM_BACK_OWN_VIEW=1
set NEONSHIM_INDIRECT_GATHER=1
set NEONSHIM_WIDE_FILL=1
set NEONSHIM_WIDE_HEAP=1
set NEONSHIM_PAGE_WRITES=slots
set NEONSHIM_TABLE_BUFFERS=1
set NEONSHIM_UNWATCHED_FRAMES=4
set NEONSHIM_FILL_WAIT=1
set NEONSHIM_LOOKUP_CACHE=1
set NEONSHIM_CARD_HASH_LEN=1
set NEONSHIM_CARD_BUFFERS_MIN_KB=64
set NEONSHIM_CARD_BUFFERS_MB=1280
set NEONSHIM_HEAP_WORKER=1
rem --- pipelines ---
set NEONSHIM_PIPELINE_WORKERS=12
set NEONSHIM_PIPELINE_NORMAL=1
rem the title's GPU heap defrag shader left coloured blocks and stale textures
set NEONSHIM_SKIP_PIPELINES=02384a60fda6f2d0
rem --- picture ---
rem the mode the title renders for; set it before running to override (e.g. 640x360)
if not defined NEONSHIM_DISPLAY_MODE set NEONSHIM_DISPLAY_MODE=1280x720
rem constant indirect light where the ray-traced light doesn't run yet
set NEONSHIM_AMBIENT=0.6
rem water textures copied back, so boats float
set NEONSHIM_TEXTURE_BACK=1
rem rays that miss with no structure built get sky light, not black
set NEONSHIM_BVH_MISS=1
set NEONSHIM_NAN_SCRUB=1
rem --- log: frame times, copies, waits ---
set NEONSHIM_FRAME_LOG=1
set NEONSHIM_WRITE_STATS=1
if /i "%1"=="warm" (
  rem warm-up keys must match the device: 32-lane waves, NVIDIA view heap cap
  set NEONSHIM_HOST_WAVE=32
  set NEONSHIM_VIEW_LIMIT=999999
  target\release\neonshim.exe warm --jobs 16 "%GAME%\shaders.rpf"
  goto :eof
)
if /i "%1"=="old-sampler" (
  echo Comparison run: indexed static sampler range ^(the behaviour before the NVIDIA fix^)
  set NEONSHIM_SAMPLER_RANGE=1
)
if not "%EXE_NAME%"=="" (set "EXE=%GAME%\%EXE_NAME%") else for %%f in ("%GAME%\*.exe") do if not defined EXE set "EXE=%%f"
for /f %%t in ('powershell -NoProfile -Command "Get-Date -Format yyyyMMdd-HHmmss"') do set TS=%%t
set "TAG=play"
if /i "%1"=="old-sampler" set "TAG=old-sampler"
if not exist logs mkdir logs
set "LOG=logs\%TAG%-%TS%.log"
echo Running. Log: %LOG%
target\release\neonshim.exe run "%EXE%" --execute > "%LOG%" 2>&1
echo Game exited. Summary:
python scripts\fps-report.py "%LOG%"
