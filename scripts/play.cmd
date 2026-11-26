@echo off
rem NeonShim launcher, NVIDIA preset (tested on an RTX 3080 Ti, Windows 10).
rem
rem   scripts\play.cmd               run the title
rem   scripts\play.cmd warm          translate every shader and build its pipelines first (once per build)
rem   scripts\play.cmd old-sampler   run with the old indexed sampler range, to compare
rem
rem The game folder comes from neonshim.cfg next to this repo's root (not committed):
rem   GAME=D:\path\to\your\copy
rem   EXE=name_of_the_exe.exe        (optional; the first .exe in GAME otherwise)
rem   CONSOLE=series-s               (optional; series-x by default)
cd /d "%~dp0.."
if exist neonshim.cfg for /f "usebackq tokens=1,* delims==" %%a in ("neonshim.cfg") do (
  if /i "%%a"=="GAME" set "GAME=%%b"
  if /i "%%a"=="EXE" set "EXE_NAME=%%b"
  if /i "%%a"=="CONSOLE" set "CONSOLE=%%b"
)
rem series-x (default) or series-s
if "%CONSOLE%"=="" set "CONSOLE=series-x"
if "%GAME%"=="" ( echo Set GAME=^<your game folder^> in neonshim.cfg & exit /b 1 )
if not exist target\release\neonshim.exe ( echo Build first: cargo build --release -p neonshim ^&^& cargo xtask agility & exit /b 1 )

rem --- submission and waits ---
if not defined NEONSHIM_ASYNC_SUBMIT set NEONSHIM_ASYNC_SUBMIT=1
if not defined NEONSHIM_QUEUE2 set NEONSHIM_QUEUE2=1
if not defined NEONSHIM_INDIRECT_WINDOW set NEONSHIM_INDIRECT_WINDOW=1
if not defined NEONSHIM_LATE_WAIT set NEONSHIM_LATE_WAIT=3
if not defined NEONSHIM_WIDE_BACK set NEONSHIM_WIDE_BACK=1
if not defined NEONSHIM_MERGE_RUNS set NEONSHIM_MERGE_RUNS=4096
rem a texture refresh waits for the runtime's remapping (1 gave rainbow, pixelated skin)
if not defined NEONSHIM_TEXTURE_NO_WAIT set NEONSHIM_TEXTURE_NO_WAIT=0
rem --- memory on a 12 GB card ---
if not defined NEONSHIM_CARD_ROOM_FOR_TARGETS_MB set NEONSHIM_CARD_ROOM_FOR_TARGETS_MB=2048
if not defined NEONSHIM_TEXTURE_STAGE_MB set NEONSHIM_TEXTURE_STAGE_MB=128
if not defined NEONSHIM_LEVELS_ON_DEMAND set NEONSHIM_LEVELS_ON_DEMAND=1
if not defined NEONSHIM_DYNAMIC_SLOTS set NEONSHIM_DYNAMIC_SLOTS=524288
rem --- copies ---
if not defined NEONSHIM_WIDE_REFRESH set NEONSHIM_WIDE_REFRESH=1
if not defined NEONSHIM_WIDE_MERGE set NEONSHIM_WIDE_MERGE=1
if not defined NEONSHIM_JOIN_COPIES set NEONSHIM_JOIN_COPIES=1
if not defined NEONSHIM_COPY_PLAIN set NEONSHIM_COPY_PLAIN=1
if not defined NEONSHIM_JOIN_PARTS set NEONSHIM_JOIN_PARTS=1
if not defined NEONSHIM_BACK_OWN_VIEW set NEONSHIM_BACK_OWN_VIEW=1
if not defined NEONSHIM_INDIRECT_GATHER set NEONSHIM_INDIRECT_GATHER=1
if not defined NEONSHIM_WIDE_FILL set NEONSHIM_WIDE_FILL=1
if not defined NEONSHIM_WIDE_HEAP set NEONSHIM_WIDE_HEAP=1
if not defined NEONSHIM_PAGE_WRITES set NEONSHIM_PAGE_WRITES=slots
if not defined NEONSHIM_TABLE_BUFFERS set NEONSHIM_TABLE_BUFFERS=1
if not defined NEONSHIM_UNWATCHED_FRAMES set NEONSHIM_UNWATCHED_FRAMES=4
if not defined NEONSHIM_FILL_WAIT set NEONSHIM_FILL_WAIT=1
if not defined NEONSHIM_LOOKUP_CACHE set NEONSHIM_LOOKUP_CACHE=1
if not defined NEONSHIM_CARD_HASH_LEN set NEONSHIM_CARD_HASH_LEN=1
if not defined NEONSHIM_CARD_BUFFERS_MIN_KB set NEONSHIM_CARD_BUFFERS_MIN_KB=64
if not defined NEONSHIM_CARD_BUFFERS_MB set NEONSHIM_CARD_BUFFERS_MB=1280
if not defined NEONSHIM_HEAP_WORKER set NEONSHIM_HEAP_WORKER=1
rem --- pipelines ---
if not defined NEONSHIM_PIPELINE_WORKERS set NEONSHIM_PIPELINE_WORKERS=12
if not defined NEONSHIM_PIPELINE_NORMAL set NEONSHIM_PIPELINE_NORMAL=1
rem the title's GPU heap defrag shader left coloured blocks and stale textures
if not defined NEONSHIM_SKIP_PIPELINES set NEONSHIM_SKIP_PIPELINES=02384a60fda6f2d0
rem --- picture ---
rem the mode the title renders for; set it before running to override (e.g. 640x360)
if not defined NEONSHIM_DISPLAY_MODE set NEONSHIM_DISPLAY_MODE=1280x720
rem the largest the window may be; the picture is scaled to it
if not defined NEONSHIM_WINDOW_SIZE set NEONSHIM_WINDOW_SIZE=1280x720
rem constant indirect light where the ray-traced light doesn't run yet
if not defined NEONSHIM_AMBIENT set NEONSHIM_AMBIENT=0.6
rem water textures copied back, so boats float
if not defined NEONSHIM_TEXTURE_BACK set NEONSHIM_TEXTURE_BACK=1
rem rays that miss with no structure built get sky light, not black
if not defined NEONSHIM_BVH_MISS set NEONSHIM_BVH_MISS=1
if not defined NEONSHIM_NAN_SCRUB set NEONSHIM_NAN_SCRUB=1
rem --- log: frame times, copies, waits ---
if not defined NEONSHIM_FRAME_LOG set NEONSHIM_FRAME_LOG=1
if not defined NEONSHIM_WRITE_STATS set NEONSHIM_WRITE_STATS=1
if /i "%1"=="warm" (
  rem warm-up keys must match the device: 32-lane waves, NVIDIA view heap cap
  if not defined NEONSHIM_HOST_WAVE set NEONSHIM_HOST_WAVE=32
  if not defined NEONSHIM_VIEW_LIMIT set NEONSHIM_VIEW_LIMIT=999999
  target\release\neonshim.exe warm --jobs 16 "%GAME%\shaders.rpf"
  rem and the driver's pipelines, so the first session doesn't build ~700 of them while it plays
  target\release\neonshim.exe translate --pipelines --list --jobs 16 "%GAME%\shaders.rpf"
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
target\release\neonshim.exe run "%EXE%" --execute --console %CONSOLE% > "%LOG%" 2>&1
echo Game exited. Summary:
python scripts\fps-report.py "%LOG%"
