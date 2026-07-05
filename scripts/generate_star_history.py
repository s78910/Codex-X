#!/usr/bin/env python3
from __future__ import annotations

import json
import math
import subprocess
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from xml.sax.saxutils import escape

REPO = "yynxxxxx/Codex-X"
OUT = Path("docs/assets/star-history.svg")


def load_stars() -> list[datetime]:
    raw = subprocess.check_output(
        [
            "gh",
            "api",
            "--paginate",
            "-H",
            "Accept: application/vnd.github.star+json",
            f"/repos/{REPO}/stargazers?per_page=100",
        ],
        text=True,
    )
    items = []
    for line in raw.splitlines():
        line = line.strip()
        if not line:
            continue
        data = json.loads(line)
        if isinstance(data, list):
            items.extend(data)
        else:
            items.append(data)
    dates = []
    for item in items:
        starred_at = item.get("starred_at")
        if starred_at:
            dates.append(datetime.fromisoformat(starred_at.replace("Z", "+00:00")))
    return sorted(dates)


def fmt_time(dt: datetime) -> str:
    return dt.strftime("%m-%d %H:%M")


def nice_step(max_value: int) -> int:
    if max_value <= 5:
        return 1
    raw = max_value / 4
    exp = 10 ** math.floor(math.log10(raw))
    for m in (1, 2, 5, 10):
        step = m * exp
        if raw <= step:
            return int(step)
    return int(10 * exp)


def build_svg(dates: list[datetime]) -> str:
    width, height = 920, 520
    ml, mr, mt, mb = 82, 34, 58, 72
    cw, ch = width - ml - mr, height - mt - mb
    now = datetime.now(timezone.utc)
    total = len(dates)
    if not dates:
        start = now
        end = now
        points = []
    else:
        start = dates[0]
        end = max(dates[-1], now)
        if end <= start:
            end = start.replace(second=start.second + 1)
        span = (end - start).total_seconds() or 1
        points = []
        for i, dt in enumerate(dates, 1):
            x = ml + ((dt - start).total_seconds() / span) * cw
            y = mt + (1 - (i / max(total, 1))) * ch
            points.append((x, y, i, dt))

    max_y = max(total, 1)
    step = nice_step(max_y)
    y_ticks = list(range(0, max_y + step, step))
    if y_ticks[-1] < max_y:
        y_ticks.append(max_y)
    x_ticks = []
    if dates:
        for r in (0, 0.25, 0.5, 0.75, 1):
            ts = start.timestamp() + (end.timestamp() - start.timestamp()) * r
            x_ticks.append(datetime.fromtimestamp(ts, timezone.utc))

    poly = " ".join(f"{x:.1f},{y:.1f}" for x, y, _, _ in points)
    area = ""
    if points:
        area = (
            f"M {points[0][0]:.1f},{mt+ch:.1f} "
            + " ".join(f"L {x:.1f},{y:.1f}" for x, y, _, _ in points)
            + f" L {points[-1][0]:.1f},{mt+ch:.1f} Z"
        )
    updated = now.strftime("%Y-%m-%d %H:%M UTC")
    title = escape(f"{REPO} Star History")

    grid = []
    labels = []
    for v in y_ticks:
        if v > max_y:
            continue
        y = mt + (1 - (v / max_y)) * ch
        grid.append(f'<line x1="{ml}" y1="{y:.1f}" x2="{ml+cw}" y2="{y:.1f}" class="grid"/>')
        labels.append(f'<text x="{ml-14}" y="{y+5:.1f}" text-anchor="end" class="tick">{v}</text>')
    for dt in x_ticks:
        x = ml + ((dt - start).total_seconds() / ((end - start).total_seconds() or 1)) * cw
        labels.append(f'<text x="{x:.1f}" y="{mt+ch+34}" text-anchor="middle" class="tick">{escape(fmt_time(dt))}</text>')

    markers = []
    if points:
        for idx in [0, len(points)//4, len(points)//2, len(points)*3//4, len(points)-1]:
            x, y, c, dt = points[idx]
            markers.append(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="4.5" class="dot"><title>{c} stars · {escape(fmt_time(dt))}</title></circle>')

    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="{title}">
  <style>
    .bg {{ fill: #0b1020; }}
    .panel {{ fill: #11182c; stroke: #263552; stroke-width: 1; }}
    .title {{ fill: #f8fafc; font: 700 26px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
    .sub {{ fill: #94a3b8; font: 500 13px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
    .big {{ fill: #fff; font: 800 46px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
    .tick {{ fill: #9ca3af; font: 12px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
    .axis {{ stroke: #64748b; stroke-width: 1.4; }}
    .grid {{ stroke: #263552; stroke-width: 1; }}
    .area {{ fill: url(#areaGradient); opacity: .42; }}
    .line {{ fill: none; stroke: #ff5a3c; stroke-width: 4; stroke-linecap: round; stroke-linejoin: round; }}
    .dot {{ fill: #ff5a3c; stroke: #fff; stroke-width: 2; }}
    .badge {{ fill: #19233d; stroke: #30415f; }}
  </style>
  <defs>
    <linearGradient id="areaGradient" x1="0" x2="0" y1="0" y2="1">
      <stop offset="0%" stop-color="#ff5a3c" stop-opacity="0.72"/>
      <stop offset="100%" stop-color="#ff5a3c" stop-opacity="0.05"/>
    </linearGradient>
  </defs>
  <rect class="bg" width="100%" height="100%" rx="18"/>
  <rect class="panel" x="24" y="24" width="872" height="472" rx="18"/>
  <text x="48" y="66" class="title">Codex-X Star History</text>
  <text x="48" y="92" class="sub">{escape(REPO)} · updated {escape(updated)}</text>
  <text x="806" y="72" text-anchor="end" class="big">{total}</text>
  <text x="806" y="94" text-anchor="end" class="sub">GitHub stars</text>
  {''.join(grid)}
  {''.join(labels)}
  <line x1="{ml}" y1="{mt}" x2="{ml}" y2="{mt+ch}" class="axis"/>
  <line x1="{ml}" y1="{mt+ch}" x2="{ml+cw}" y2="{mt+ch}" class="axis"/>
  <path d="{area}" class="area"/>
  <polyline points="{poly}" class="line"/>
  {''.join(markers)}
  <text x="{ml+cw/2:.1f}" y="{height-22}" text-anchor="middle" class="sub">Date</text>
  <text x="24" y="{mt+ch/2:.1f}" text-anchor="middle" class="sub" transform="rotate(-90 24 {mt+ch/2:.1f})">GitHub Stars</text>
</svg>
'''


def main() -> None:
    dates = load_stars()
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(build_svg(dates), encoding="utf-8")
    print(f"wrote {OUT} with {len(dates)} stars")


if __name__ == "__main__":
    main()
