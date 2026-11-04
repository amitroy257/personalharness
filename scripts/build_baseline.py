#!/usr/bin/env python3
"""Build crates/harness-core/data/baseline.json from public LMArena snapshots.

The app ships this file so rankings work on first launch (and fully offline).
At runtime the app refreshes the same data from the network when allowed.

Source: https://github.com/oolong-tea-2026/arena-ai-leaderboards (MIT, data
sourced from arena.ai). Snapshots only cover the top N models per board, so we
merge several snapshots over the past months (newest value wins).

Usage: python3 scripts/build_baseline.py [--days 240] [--step 7]
"""
import argparse
import datetime as dt
import json
import pathlib
import sys
import urllib.request

BASE = "https://raw.githubusercontent.com/oolong-tea-2026/arena-ai-leaderboards/main/data"
BOARDS = ("text", "code")
OUT = pathlib.Path(__file__).resolve().parent.parent / "crates/harness-core/data/baseline.json"


def get(url):
    try:
        with urllib.request.urlopen(url, timeout=20) as r:
            return json.loads(r.read().decode())
    except Exception:
        return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--days", type=int, default=240)
    ap.add_argument("--step", type=int, default=5)
    args = ap.parse_args()

    today = dt.date.today()
    merged = {b: {} for b in BOARDS}
    first_seen = {b: {} for b in BOARDS}
    used = []
    # oldest first so newer snapshots overwrite older values
    for back in range(args.days, -1, -args.step):
        day = (today - dt.timedelta(days=back)).isoformat()
        idx = get(f"{BASE}/{day}/_index.json")
        if not idx or not idx.get("leaderboards"):
            continue
        got = False
        for b in BOARDS:
            if b not in idx["leaderboards"]:
                continue
            snap = get(f"{BASE}/{day}/{b}.json")
            if not snap:
                continue
            for m in snap.get("models", []):
                if m.get("score") is None or not m.get("model"):
                    continue
                first_seen[b].setdefault(m["model"], day)
                merged[b][m["model"]] = {
                    "score": float(m["score"]),
                    "votes": m.get("votes"),
                    "vendor": m.get("vendor"),
                    "license": m.get("license"),
                    "date": day,
                }
                got = True
        if got:
            used.append(day)
            print(f"merged {day}", file=sys.stderr)

    out = {
        "generated_at": today.isoformat(),
        "source": "arena.ai via github.com/oolong-tea-2026/arena-ai-leaderboards",
        "snapshots": used,
        # first_seen equal to the first snapshot is ambiguous (could be older), so it is omitted
        "boards": {
            b: [
                dict(model=k, **v, first_seen=(first_seen[b][k] if used and first_seen[b][k] != used[0] else None))
                for k, v in sorted(merged[b].items(), key=lambda kv: -kv[1]["score"])
            ]
            for b in BOARDS
        },
    }
    OUT.write_text(json.dumps(out, indent=1) + "\n")
    print(f"wrote {OUT} ({sum(len(v) for v in merged.values())} entries from {len(used)} snapshots)", file=sys.stderr)


if __name__ == "__main__":
    main()
