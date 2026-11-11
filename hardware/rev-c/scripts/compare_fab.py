#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Compare the Rev C Gerber/drill zip with Rev B's released plots (stdlib only).

usage: compare_fab.py [--report FILE]

Each of the 13 files in fab-jlcpcb/main-revc-jlcpcb-gerber.zip is compared
with its Rev B counterpart in ../rev-b/main/{gerber,stencil,drill} and in
Rev B's own order zip. Only header lines that carry the KiCad version, the
plot time or the project name/id may differ; any other difference fails.
"""
import argparse
import hashlib
import json
import re
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REV_B = ROOT.parent / "rev-b"
ZIP = ROOT / "fab-jlcpcb/main-revc-jlcpcb-gerber.zip"
METADATA = re.compile(r"^(%TF\.(GenerationSoftware|CreationDate|ProjectId),|G04 Created by KiCad |; DRILL file KiCad |"
                      r"; #@! TF\.(CreationDate|GenerationSoftware),)")


def compare():
    errors, members = [], {}
    with zipfile.ZipFile(ZIP) as rev_c, zipfile.ZipFile(REV_B / "fab-jlcpcb/main-revb-jlcpcb-gerber.zip") as rev_b_zip:
        names = sorted(rev_c.namelist())
        expected = sorted(n.replace("main-revb-", "main-revc-") for n in rev_b_zip.namelist())
        if names != expected:
            errors.append(f"zip members differ from Rev B's: {sorted(set(names) ^ set(expected))}")
        for name in names:
            old_name = name.replace("main-revc-", "main-revb-")
            folder = "drill" if name.endswith(".drl") else "stencil" if "Paste" in name else "gerber"
            new = rev_c.read(name)
            old = (REV_B / "main" / folder / old_name).read_bytes()
            if old != rev_b_zip.read(old_name):
                errors.append(f"{old_name}: Rev B's zip and plot directory disagree")
            a, b = old.decode().splitlines(), new.decode().splitlines()
            changed = [(x, y) for x, y in zip(a, b) if x != y]
            other = [pair for pair in changed if not (METADATA.match(pair[0]) and METADATA.match(pair[1]))]
            if len(a) != len(b) or other:
                errors.append(f"{name}: differs from Rev B beyond metadata ({len(other)} lines, {len(a)} vs {len(b)} total)")
            members[name] = {"sha256": hashlib.sha256(new).hexdigest(), "lines": len(b),
                             "metadata_lines_differing": len(changed), "other_lines_differing": len(other)}
    return {"passed": not errors, "zip_sha256": hashlib.sha256(ZIP.read_bytes()).hexdigest(),
            "compared_with": "../rev-b/main/{gerber,stencil,drill} and ../rev-b/fab-jlcpcb/main-revb-jlcpcb-gerber.zip",
            "metadata_lines_allowed": ["TF.GenerationSoftware", "TF.CreationDate", "TF.ProjectId",
                                       "G04 Created by KiCad", "; DRILL file KiCad"],
            "files": members, "errors": errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    report = compare()
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    if report["errors"]:
        raise SystemExit("\n".join(report["errors"]))
    total = sum(f["metadata_lines_differing"] for f in report["files"].values())
    print(f"Fabrication plots: {len(report['files'])} files equal Rev B's apart from {total} metadata lines.")


if __name__ == "__main__":
    main()
