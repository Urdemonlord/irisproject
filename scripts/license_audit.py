#!/usr/bin/env python3
"""Dependency license audit: fail on GPL/AGPL/LGPL/SSPL in Rust or web deps.

Rust: `cargo metadata` license field (SPDX expression). An `OR` expression passes if any
alternative is allowed; `AND` needs all. Web: every package.json under apps/web/node_modules.
Run from repo root: python3 scripts/license_audit.py
"""
import json, pathlib, re, subprocess, sys

DENY = re.compile(r"\b(A?GPL|LGPL|SSPL)", re.I)
root = pathlib.Path(__file__).resolve().parent.parent


def allowed(expr: str) -> bool:
    expr = expr.replace("/", " OR ").replace("(", " ").replace(")", " ")
    return any(all(not DENY.search(t) for t in alt.split(" AND ")) for alt in expr.split(" OR "))


meta = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--locked"], cwd=root))
rows = []
for p in meta["packages"]:
    if p["source"] is None:  # workspace crates
        continue
    rows.append(("cargo", p["name"], p["version"], p.get("license") or ("FILE:" + str(p.get("license_file")))))
for pj in (root / "apps/web/node_modules").rglob("package.json"):
    if pj.parent.parent.name not in ("node_modules",) and pj.parent.parent.parent.name != "node_modules":
        continue  # skip nested fixture/test package.json files
    try:
        d = json.loads(pj.read_text())
    except (ValueError, OSError):
        continue
    if "name" in d and "version" in d:
        lic = d.get("license") or d.get("licenses") or "UNKNOWN"
        rows.append(("npm", d["name"], d["version"], lic if isinstance(lic, str) else json.dumps(lic)))

bad = [r for r in rows if not allowed(r[3])]
unknown = [r for r in rows if r[3].startswith("FILE:") or r[3] == "UNKNOWN"]
counts = {}
for r in rows:
    counts[r[3]] = counts.get(r[3], 0) + 1
for lic, n in sorted(counts.items(), key=lambda x: -x[1]):
    print(f"{n:4}  {lic}")
print(f"\n{len(rows)} packages; {len(bad)} copyleft-denied; {len(unknown)} need manual review")
for r in bad + unknown:
    print("  ", *r)
sys.exit(1 if bad else 0)
