#!/usr/bin/env python3
"""Turn test output into a readable GitHub Actions job summary.

  ci_summary.py cargo   <cargo-test-log> [title]
  ci_summary.py vitest  <vitest-json>    [title]
  ci_summary.py selfcheck <selfcheck-json> [title] [--require id,id,...]

Appends Markdown to $GITHUB_STEP_SUMMARY (or prints it). `selfcheck` exits 1
if any check failed or a required check did not pass.
"""
import json
import os
import re
import sys


def emit(md: str) -> None:
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(md + "\n")
    print(md)


def cargo(log: str, title: str) -> int:
    suites, current = {}, "tests"
    for line in open(log, encoding="utf-8", errors="replace"):
        m = re.search(r"Running (?:unittests )?(\S+)", line)
        if m:
            src = m.group(1)
            current = "unit tests" if "src/lib.rs" in src or "src/main.rs" in src else os.path.basename(src).split("-")[0].replace(".rs", "")
            continue
        m = re.match(r"test (\S+) \.\.\. (ok|FAILED|ignored)", line.strip())
        if m:
            suites.setdefault(current, []).append((m.group(1), m.group(2)))
    total = sum(len(v) for v in suites.values())
    failed = sum(1 for v in suites.values() for _, r in v if r == "FAILED")
    md = [f"### {title}", "", f"**{total - failed}/{total} passed**" + (f" — ❌ {failed} failed" if failed else " ✅"), ""]
    md += ["| Suite | Test | Result |", "|---|---|---|"]
    for suite, tests in suites.items():
        for name, res in tests:
            md.append(f"| {suite} | `{name}` | {'✅' if res == 'ok' else '❌' if res == 'FAILED' else '⏭️'} |")
    emit("\n".join(md))
    return 1 if failed else 0


def vitest(path: str, title: str) -> int:
    data = json.load(open(path, encoding="utf-8"))
    rows, failed = [], 0
    for f in data.get("testResults", []):
        for t in f.get("assertionResults", []):
            ok = t.get("status") == "passed"
            failed += 0 if ok else 1
            rows.append(f"| `{os.path.basename(f.get('name', ''))}` | {t.get('fullName') or t.get('title')} | {'✅' if ok else '❌'} |")
    md = [f"### {title}", "", f"**{len(rows) - failed}/{len(rows)} passed**" + (" ✅" if not failed else f" — ❌ {failed} failed"), ""]
    md += ["| File | Test | Result |", "|---|---|---|"] + rows
    emit("\n".join(md))
    return 1 if failed else 0


def selfcheck(path: str, title: str, required: list) -> int:
    raw = open(path, encoding="utf-8-sig").read()
    data = json.loads(raw)
    icon = {"pass": "✅ pass", "fail": "❌ FAIL", "info": "ℹ️ info"}
    md = [f"### {title}", "", f"Platform: `{data.get('platform')}` · desktop app: `{data.get('desktop')}` · "
          f"**{data.get('passed')} passed, {data.get('failed')} failed**", ""]
    md += ["| Check | Result | Evidence |", "|---|---|---|"]
    status = {}
    for c in data.get("checks", []):
        status[c["id"]] = c["status"]
        detail = str(c.get("detail", "")).replace("|", "\\|")
        md.append(f"| {c['label']} | {icon.get(c['status'], c['status'])} | {detail} |")
    missing = [r for r in required if status.get(r) != "pass"]
    if missing:
        md += ["", f"❌ Required checks not passing: {', '.join(missing)}"]
    emit("\n".join(md))
    return 1 if data.get("failed") or missing else 0


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    kind, path = sys.argv[1], sys.argv[2]
    rest = sys.argv[3:]
    required = []
    if "--require" in rest:
        i = rest.index("--require")
        required = [x for x in rest[i + 1].split(",") if x]
        rest = rest[:i] + rest[i + 2:]
    title = rest[0] if rest else kind
    return {"cargo": lambda: cargo(path, title), "vitest": lambda: vitest(path, title),
            "selfcheck": lambda: selfcheck(path, title, required)}[kind]()


if __name__ == "__main__":
    sys.exit(main())
