"""Session summary from a NeonShim log, as a Markdown table.

    python scripts/fps-report.py <log file> [--world-draws N] [--steady-after MIN]

Reads the frame lines the host writes every few seconds
("12.3 fps, 81.2 ms/frame (...), 4567 draws/frame, ...") and reports the
session length, fps over the session and in steady play, and any crash or
device-removed lines. Samples with fewer draws than --world-draws (default
500) are menus and loading screens, and are counted apart.
"""

import argparse
import re
import statistics
import sys
from datetime import datetime

FRAME = re.compile(
    r"^(?P<ts>\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?)Z\s+INFO\s+\S+::host:\s+"
    r"(?P<fps>[\d.]+) fps, (?P<ms>[\d.]+) ms/frame .*?(?P<draws>\d+) draws/frame"
)
TROUBLE = re.compile(r"device removed|DEVICE_REMOVED|0x887A0005|crash|panicked", re.IGNORECASE)


def stamp(text):
    return datetime.fromisoformat(text[:26])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("log")
    ap.add_argument("--world-draws", type=int, default=500)
    ap.add_argument("--steady-after", type=float, default=10.0, help="minutes into the world")
    a = ap.parse_args()

    samples, trouble, first = [], [], None
    with open(a.log, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = FRAME.match(line)
            if m:
                t = stamp(m["ts"])
                first = first or t
                samples.append((t, float(m["fps"]), float(m["ms"]), int(m["draws"])))
            elif TROUBLE.search(line):
                trouble.append(line.strip()[:160])
    if not samples:
        sys.exit("no frame lines in this log")

    world = [s for s in samples if s[3] >= a.world_draws]
    last = samples[-1][0]
    rows = [
        ("Log", a.log.replace("\\", "/").rsplit("/", 1)[-1]),
        ("Session", minutes(last - first)),
        ("Samples", f"{len(samples)} ({len(world)} in the world)"),
    ]
    if world:
        entered = world[0][0]
        fps = [s[1] for s in world]
        rows += [
            ("In the world", minutes(last - entered)),
            ("World fps, mean / median", f"{statistics.mean(fps):.1f} / {statistics.median(fps):.1f}"),
            ("World fps, min / max", f"{min(fps):.1f} / {max(fps):.1f}"),
            ("Draws per frame, median", f"{statistics.median(s[3] for s in world):.0f}"),
        ]
        steady = [s for s in world if (s[0] - entered).total_seconds() >= a.steady_after * 60]
        if steady:
            sf = [s[1] for s in steady]
            rows += [
                (f"Steady (after {a.steady_after:g} min), mean fps", f"{statistics.mean(sf):.1f}"),
                ("Steady, 10th percentile fps", f"{percentile(sf, 10):.1f}"),
                ("Steady, median frame time", f"{statistics.median(s[2] for s in steady):.0f} ms"),
            ]
    rows.append(("Crash / device-removed lines", str(len(trouble)) if trouble else "none"))

    print("| Metric | Value |\n| --- | --- |")
    for k, v in rows:
        print(f"| {k} | {v} |")
    for t in trouble[:5]:
        print(f"\n    {t}")


def minutes(d):
    s = int(d.total_seconds())
    return f"{s // 60} min {s % 60:02d} s"


def percentile(xs, p):
    xs = sorted(xs)
    k = (len(xs) - 1) * p / 100
    lo = int(k)
    hi = min(lo + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


if __name__ == "__main__":
    main()
