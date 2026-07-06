#!/usr/bin/env python3
"""Generate the temporary runtime surface_id denylist bridge.

The reviewed source of truth is config/surface-denylist/{en,zh}.txt. This
script maps those normalized surface keys to surface_id values using
data/preprocess/surface_qids.tsv, then binds the result to a runtime manifest.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys


REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_DENYLIST_DIR = REPO_ROOT / "config" / "surface-denylist"
DEFAULT_SURFACE_QIDS = REPO_ROOT / "data" / "preprocess" / "surface_qids.tsv"
DEFAULT_RUNTIME_DIR = REPO_ROOT / "data" / "runtime"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate a runtime-bound surface_id denylist JSON."
    )
    parser.add_argument(
        "--version",
        required=True,
        help="Runtime data version, e.g. zh-en-20260702.",
    )
    parser.add_argument(
        "--surface-qids",
        type=pathlib.Path,
        default=DEFAULT_SURFACE_QIDS,
        help="Path to data/preprocess/surface_qids.tsv.",
    )
    parser.add_argument(
        "--runtime",
        type=pathlib.Path,
        default=DEFAULT_RUNTIME_DIR,
        help="Path to data/runtime containing manifest.json.",
    )
    parser.add_argument(
        "--denylist-dir",
        type=pathlib.Path,
        default=DEFAULT_DENYLIST_DIR,
        help="Directory containing en.txt and zh.txt.",
    )
    parser.add_argument(
        "--out",
        type=pathlib.Path,
        help=(
            "Output JSON path. Defaults to "
            "config/surface-denylist/runtime-<version>.surface-ids.json."
        ),
    )
    return parser.parse_args()


def tsv_unescape(value: str) -> str:
    output: list[str] = []
    index = 0
    while index < len(value):
        ch = value[index]
        if ch != "\\":
            output.append(ch)
            index += 1
            continue
        index += 1
        if index >= len(value):
            output.append("\\")
            break
        escaped = value[index]
        index += 1
        if escaped == "\\":
            output.append("\\")
        elif escaped == "t":
            output.append("\t")
        elif escaped == "n":
            output.append("\n")
        elif escaped == "r":
            output.append("\r")
        else:
            output.append("\\")
            output.append(escaped)
    return "".join(output)


def read_reviewed_surfaces(denylist_dir: pathlib.Path) -> list[str]:
    surfaces: list[str] = []
    seen: set[str] = set()
    for name in ("en.txt", "zh.txt"):
        path = denylist_dir / name
        if not path.is_file():
            raise SystemExit(f"denylist file not found: {path}")
        for line_number, raw_line in enumerate(
            path.read_text(encoding="utf-8").splitlines(), 1
        ):
            line = raw_line.strip()
            if not line or line.startswith("#"):
                continue
            if "\t" in line:
                raise SystemExit(f"{path}:{line_number}: tabs are not allowed")
            if line not in seen:
                seen.add(line)
                surfaces.append(line)
    if not surfaces:
        raise SystemExit(f"no reviewed surfaces found in {denylist_dir}")
    return surfaces


def read_runtime_manifest(runtime_dir: pathlib.Path) -> dict:
    manifest_path = runtime_dir / "manifest.json"
    if not manifest_path.is_file():
        raise SystemExit(f"runtime manifest not found: {manifest_path}")
    try:
        return json.loads(manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise SystemExit(f"failed to parse {manifest_path}: {exc}") from exc


def display_path(path: pathlib.Path) -> str:
    try:
        return str(path.resolve().relative_to(REPO_ROOT))
    except ValueError:
        return str(path)


def map_surfaces_to_ids(
    surface_qids_path: pathlib.Path, reviewed_surfaces: list[str]
) -> dict[str, int]:
    if not surface_qids_path.is_file():
        raise SystemExit(f"surface_qids.tsv not found: {surface_qids_path}")

    wanted = set(reviewed_surfaces)
    found: dict[str, int] = {}
    with surface_qids_path.open("r", encoding="utf-8") as file:
        header = file.readline().rstrip("\n")
        if header != "surface_key\tqids\tqid_count":
            raise SystemExit(f"unexpected surface_qids.tsv header: {header}")
        for line_number, raw_line in enumerate(file, 1):
            surface_key, separator, _rest = raw_line.rstrip("\n").partition("\t")
            if not separator:
                raise SystemExit(
                    f"{surface_qids_path}:{line_number + 1}: row has no tab"
                )
            surface_key = tsv_unescape(surface_key)
            if surface_key in wanted:
                found[surface_key] = line_number - 1
                if len(found) == len(wanted):
                    break

    missing = [surface for surface in reviewed_surfaces if surface not in found]
    if missing:
        formatted = ", ".join(repr(surface) for surface in missing)
        raise SystemExit(f"reviewed surfaces missing from surface_qids.tsv: {formatted}")
    return found


def main() -> int:
    args = parse_args()
    out_path = args.out or (
        DEFAULT_DENYLIST_DIR / f"runtime-{args.version}.surface-ids.json"
    )
    reviewed_surfaces = read_reviewed_surfaces(args.denylist_dir)
    surface_ids_by_text = map_surfaces_to_ids(args.surface_qids, reviewed_surfaces)
    manifest = read_runtime_manifest(args.runtime)

    output = {
        "runtime_version": args.version,
        "surface_normalization": manifest.get("surface_normalization"),
        "surface_count": manifest.get("surface_count"),
        "qid_count": manifest.get("qid_count"),
        "source_files": {
            "denylist": [
                display_path(args.denylist_dir / "en.txt"),
                display_path(args.denylist_dir / "zh.txt"),
            ],
            "surface_qids": display_path(args.surface_qids),
            "runtime_manifest": display_path(args.runtime / "manifest.json"),
        },
        "surface_ids": sorted(surface_ids_by_text.values()),
        "surface_text_to_id": {
            surface: surface_ids_by_text[surface] for surface in reviewed_surfaces
        },
    }

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        json.dumps(output, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"wrote {out_path}")
    print(f"surfaces: {len(reviewed_surfaces)}")
    print(f"surface_ids: {len(output['surface_ids'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
