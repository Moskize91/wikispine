#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/package-runtime-data.sh --version <version> [options]

Package a built Wikispine runtime data directory as a zip archive and update
config/runtime-data.json with artifact metadata. The generated artifact name is:

  wikigraph-runtime-data-<version>-<YYYYMMDD>.zip

Options:
  --version <version>  Runtime data version, e.g. 2026-07-02 or zh-en-20260702
  --source <dir>      Runtime data directory (default: data/runtime)
  --out <dir>         Output directory (default: dist/runtime-data)
  --url <url>         Public URL to write into config/runtime-data.json
  -h, --help          Show this help
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_dir="$repo_root/data/runtime"
out_dir="$repo_root/dist/runtime-data"
version=""
url=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      version="$2"
      shift 2
      ;;
    --source)
      source_dir="$2"
      shift 2
      ;;
    --out)
      out_dir="$2"
      shift 2
      ;;
    --url)
      url="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "$version" ]]; then
  echo "--version is required" >&2
  usage >&2
  exit 1
fi

case "$version" in
  *[!A-Za-z0-9._-]*|"")
    echo "--version may only contain letters, numbers, dot, underscore, and dash" >&2
    exit 1
    ;;
esac

if [[ ! -d "$source_dir" ]]; then
  echo "runtime data directory not found: $source_dir" >&2
  exit 1
fi

if [[ ! -f "$source_dir/manifest.json" ]]; then
  echo "runtime data manifest not found: $source_dir/manifest.json" >&2
  exit 1
fi

if ! find "$source_dir" -type f -print -quit | grep -q .; then
  echo "runtime data directory has no files: $source_dir" >&2
  exit 1
fi

if ! command -v zip >/dev/null 2>&1; then
  echo "required command not found: zip" >&2
  exit 1
fi

if command -v md5sum >/dev/null 2>&1; then
  md5_cmd=(md5sum)
elif command -v md5 >/dev/null 2>&1; then
  md5_cmd=(md5 -q)
else
  echo "required command not found: md5sum or md5" >&2
  exit 1
fi

created_date="$(date -u +%Y%m%d)"
created_at_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
artifact="wikigraph-runtime-data-${version}-${created_date}.zip"
mkdir -p "$out_dir"
archive_path="$out_dir/$artifact"
tmp_dir="$(mktemp -d)"

cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

staging="$tmp_dir/runtime"
mkdir -p "$staging"

echo "copying runtime data from $source_dir"
if command -v rsync >/dev/null 2>&1; then
  rsync -a --delete "$source_dir"/ "$staging"/
else
  (cd "$source_dir" && tar -cf - .) | (cd "$staging" && tar -xf -)
fi

echo "creating $archive_path"
rm -f "$archive_path"
(
  cd "$tmp_dir"
  zip -qr -X "$archive_path" runtime
)

archive_md5="$("${md5_cmd[@]}" "$archive_path" | awk '{print $1}')"
archive_bytes="$(wc -c < "$archive_path" | tr -d ' ')"
config_path="$repo_root/config/runtime-data.json"

python3 - "$config_path" <<PY
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
data = {
    "version": "$version",
    "artifact": "$artifact",
    "url": "$url",
    "archive_md5": "$archive_md5",
    "archive_bytes": int("$archive_bytes"),
    "created_at_utc": "$created_at_utc",
}
path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
PY

echo "wrote $config_path"
echo "artifact: $artifact"
echo "bytes: $archive_bytes"
echo "md5: $archive_md5"
if [[ -n "$url" ]]; then
  echo "url: $url"
fi
