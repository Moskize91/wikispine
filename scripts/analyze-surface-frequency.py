#!/usr/bin/env python3
"""Collect high-frequency Wikispine surfaces from raw text samples.

The script sends input texts to a Wikispine /match endpoint, reconstructs the
matched raw spans from UTF-16 offsets, normalizes those spans into surface keys,
keeps only the top-N most frequent keys, then enriches the top keys with exact
QID candidates.
"""

from __future__ import annotations

import argparse
import collections
import json
import pathlib
import re
import sys
import time
import unicodedata
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any


DEFAULT_ENDPOINT = "https://wikispi-service-cxbfjlteab.cn-hangzhou.fcapp.run/match"


@dataclass
class SurfaceStats:
    frequency: int = 0
    line_bytes_total: int = 0
    word_boundary_count: int = 0
    non_word_boundary_count: int = 0
    surface_ids: collections.Counter[int] = field(default_factory=collections.Counter)
    qid_samples: dict[int, dict[str, Any]] = field(default_factory=dict)
    raw_examples: list[str] = field(default_factory=list)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Analyze high-frequency Wikispine surface matches."
    )
    parser.add_argument("--raw-dir", type=pathlib.Path, required=True)
    parser.add_argument("--language", required=True, help="Label for output metadata.")
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--endpoint", default=DEFAULT_ENDPOINT)
    parser.add_argument("--top", type=int, default=5000)
    parser.add_argument(
        "--scan-max-candidates",
        type=int,
        default=1,
        help="Candidate cap used during frequency scanning.",
    )
    parser.add_argument(
        "--enrich-qids",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="Fetch full exact-match QID[] for top surfaces.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=300.0,
        help="HTTP timeout per request in seconds.",
    )
    return parser.parse_args()


def normalize_surface_key(value: str) -> str | None:
    chars: list[str] = []
    for ch in value:
        if is_separator(ch):
            chars.append(" ")
        elif is_default_ignorable(ch):
            continue
        else:
            chars.append(ch)

    value = unicodedata.normalize("NFKC", "".join(chars)).casefold()
    value = unicodedata.normalize("NFD", value)

    chars = []
    for ch in value:
        if unicodedata.combining(ch):
            continue
        if is_separator(ch):
            chars.append(" ")
        elif is_default_ignorable(ch):
            continue
        else:
            chars.append(ch)

    normalized = re.sub(r" +", " ", "".join(chars)).strip()
    return normalized or None


def is_separator(ch: str) -> bool:
    if ch.isspace():
        return True
    category = unicodedata.category(ch)
    if category in {"Zs", "Zl", "Zp"}:
        return True
    if ch in {"+", "#", "&"}:
        return False
    if category.startswith("P"):
        return True
    return ch in {"·", "•", "|", "\\", "/"}


def is_default_ignorable(ch: str) -> bool:
    code = ord(ch)
    category = unicodedata.category(ch)
    if category == "Cf":
        return True
    if code == 0x00AD:
        return True
    if 0xFE00 <= code <= 0xFE0F:
        return True
    if 0xE0100 <= code <= 0xE01EF:
        return True
    return False


def utf16_to_py_index_map(text: str) -> list[int]:
    mapping: list[int] = []
    for index, ch in enumerate(text):
        for _ in range(len(ch.encode("utf-16-le")) // 2):
            mapping.append(index)
    mapping.append(len(text))
    return mapping


def slice_utf16(text: str, offset_map: list[int], start: int, end: int) -> str:
    if start < 0 or end < start or end >= len(offset_map):
        return ""
    return text[offset_map[start] : offset_map[end]]


def is_word_boundary_like(text: str, start_index: int, end_index: int) -> bool:
    left = start_index == 0 or not text[start_index - 1].isalnum()
    right = end_index >= len(text) or not text[end_index].isalnum()
    return left and right


def open_match_response(
    endpoint: str, text: str, options: dict[str, Any], timeout: float
) -> Any:
    body = json.dumps({"text": text, "options": options}, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(
        endpoint,
        data=body,
        headers={
            "content-type": "application/json",
            "accept": "application/x-ndjson",
        },
        method="POST",
    )
    try:
        return urllib.request.urlopen(request, timeout=timeout)
    except urllib.error.URLError as exc:
        raise RuntimeError(f"match request failed: {exc}") from exc


def scan_file(
    path: pathlib.Path,
    endpoint: str,
    scan_max_candidates: int,
    timeout: float,
    stats_by_surface: dict[str, SurfaceStats],
) -> dict[str, int]:
    text = path.read_text(encoding="utf-8")
    offset_map = utf16_to_py_index_map(text)
    response = open_match_response(
        endpoint,
        text,
        {"include_disambiguation": True, "max_candidates_per_surface": scan_max_candidates},
        timeout,
    )
    file_stats = {"matches": 0, "response_bytes": 0, "malformed": 0}
    with response:
        for line in response:
            file_stats["response_bytes"] += len(line)
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                file_stats["malformed"] += 1
                continue
            if event.get("type") != "match":
                continue
            match = event["match"]
            start = int(match["start"])
            end = int(match["end"])
            raw_span = slice_utf16(text, offset_map, start, end)
            surface_key = normalize_surface_key(raw_span)
            if not surface_key:
                continue
            item = stats_by_surface.setdefault(surface_key, SurfaceStats())
            item.frequency += 1
            item.line_bytes_total += len(line)
            item.surface_ids.update([int(match["surface_id"])])
            for candidate in match.get("qids", []):
                qid_number = int(candidate["qid_number"])
                item.qid_samples.setdefault(qid_number, candidate)
            start_index = offset_map[start]
            end_index = offset_map[end]
            if is_word_boundary_like(text, start_index, end_index):
                item.word_boundary_count += 1
            else:
                item.non_word_boundary_count += 1
            if len(item.raw_examples) < 5 and raw_span not in item.raw_examples:
                item.raw_examples.append(raw_span)
            file_stats["matches"] += 1
    return file_stats


def exact_match_qids(
    endpoint: str, surface_key: str, timeout: float, expected_surface_id: int | None
) -> list[dict[str, Any]]:
    target_end = len(surface_key.encode("utf-16-le")) // 2
    best: list[dict[str, Any]] = []
    response = open_match_response(endpoint, surface_key, {"include_disambiguation": True}, timeout)
    with response:
        for line in response:
            event = json.loads(line)
            if event.get("type") != "match":
                continue
            match = event["match"]
            if int(match["start"]) != 0 or int(match["end"]) != target_end:
                continue
            if expected_surface_id is not None and int(match["surface_id"]) != expected_surface_id:
                continue
            best = match.get("qids", [])
            break
    return best


def word_count(text: str, language: str) -> int:
    if language.startswith("zh"):
        return sum(1 for ch in text if "\u4e00" <= ch <= "\u9fff")
    return len(re.findall(r"[A-Za-z0-9]+(?:['-][A-Za-z0-9]+)?", text))


def main() -> int:
    args = parse_args()
    if args.top <= 0:
        raise SystemExit("--top must be positive")
    files = sorted(args.raw_dir.glob("*.txt"))
    if not files:
        raise SystemExit(f"no .txt files found in {args.raw_dir}")

    started = time.time()
    stats_by_surface: dict[str, SurfaceStats] = {}
    input_files = []
    total_words = 0
    total_chars = 0
    total_matches = 0
    total_response_bytes = 0

    for path in files:
        text = path.read_text(encoding="utf-8")
        words = word_count(text, args.language)
        total_words += words
        total_chars += len(text)
        print(f"scanning {path} chars={len(text)} words={words}", file=sys.stderr)
        file_stats = scan_file(
            path,
            args.endpoint,
            args.scan_max_candidates,
            args.timeout,
            stats_by_surface,
        )
        total_matches += file_stats["matches"]
        total_response_bytes += file_stats["response_bytes"]
        input_files.append({"path": str(path), "chars": len(text), "words": words, **file_stats})

    top_items = sorted(
        stats_by_surface.items(),
        key=lambda item: (-item[1].frequency, item[0]),
    )[: args.top]

    records = []
    for surface_key, stats in top_items:
        primary_surface_id = stats.surface_ids.most_common(1)[0][0]
        qids = []
        if args.enrich_qids:
            print(f"enriching {surface_key!r}", file=sys.stderr)
            qids = exact_match_qids(
                args.endpoint, surface_key, args.timeout, primary_surface_id
            )
        records.append(
            {
                "surface_key": surface_key,
                "frequency": stats.frequency,
                "frequency_pct": stats.frequency / total_matches if total_matches else 0,
                "remaining_matches_if_denied": total_matches - stats.frequency,
                "line_bytes_total": stats.line_bytes_total,
                "line_bytes_avg": stats.line_bytes_total / stats.frequency,
                "length_chars": len(surface_key),
                "word_boundary_count": stats.word_boundary_count,
                "non_word_boundary_count": stats.non_word_boundary_count,
                "surface_ids": [
                    {"surface_id": sid, "count": count}
                    for sid, count in stats.surface_ids.most_common()
                ],
                "qid_sample_count": len(stats.qid_samples),
                "qid_count": len(qids) if qids else None,
                "qids": qids,
                "raw_examples": stats.raw_examples,
            }
        )

    output = {
        "language": args.language,
        "endpoint": args.endpoint,
        "raw_dir": str(args.raw_dir),
        "created_at_unix": int(time.time()),
        "elapsed_seconds": time.time() - started,
        "top": args.top,
        "scan_max_candidates": args.scan_max_candidates,
        "input_files": input_files,
        "totals": {
            "chars": total_chars,
            "words": total_words,
            "matches": total_matches,
            "response_bytes": total_response_bytes,
            "matches_per_1000_words": (
                total_matches / total_words * 1000 if total_words else None
            ),
            "unique_surface_keys": len(stats_by_surface),
        },
        "surfaces": records,
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(output, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
