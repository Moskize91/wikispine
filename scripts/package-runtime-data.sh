#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/package-runtime-data.sh --version <version> [options]

Package a built Wikispine runtime data directory as a zip archive and update
config/runtime-data.json with artifact metadata. The generated artifact name is:

  wikigraph-runtime-data-<version>.zip

Options:
  --version <version>  Runtime data version, e.g. 2026-07-02 or zh-en-20260702
  --source <dir>      Runtime data directory (default: data/runtime)
  --out <dir>         Output directory (default: dist/runtime-data)
  --publish           Upload the generated ZIP to the configured Hugging Face dataset repo
  --delete-zip        Delete the local ZIP after a successful --publish upload
  -h, --help          Show this help
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_dir="$repo_root/data/runtime"
out_dir="$repo_root/dist/runtime-data"
provider="huggingface"
repo_id="moskize/wikispine-runtime"
revision="main"
version=""
publish=false
delete_zip=false

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
    --publish)
      publish=true
      shift
      ;;
    --delete-zip)
      delete_zip=true
      shift
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

if [[ "$delete_zip" == true && "$publish" != true ]]; then
  echo "--delete-zip requires --publish" >&2
  exit 1
fi

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
source_dir="$(cd "$source_dir" && pwd)"

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

if [[ "$publish" == true ]] && ! command -v hf >/dev/null 2>&1; then
  echo "required command not found: hf" >&2
  echo "install with: pipx install 'huggingface_hub[hf_xet]'" >&2
  exit 1
fi

created_at_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
artifact="wikigraph-runtime-data-${version}.zip"
mkdir -p "$out_dir"
archive_path="$out_dir/$artifact"
tmp_dir="$(mktemp -d)"

cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

ln -s "$source_dir" "$tmp_dir/runtime"

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
package = {
    "version": "$version",
    "archive_md5": "$archive_md5",
    "archive_bytes": int("$archive_bytes"),
    "created_at_utc": "$created_at_utc",
}
if path.exists():
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise SystemExit(f"failed to parse {path}: {exc}") from exc
else:
    data = {}

packages = data.get("packages")
if packages is None:
    # Migrate the pre-history single-package config shape.
    if data.get("version") or data.get("artifact"):
        packages = [{
            "version": data.get("version", ""),
            "archive_md5": data.get("archive_md5", ""),
            "archive_bytes": data.get("archive_bytes", 0),
            "created_at_utc": data.get("created_at_utc", ""),
        }]
    else:
        packages = []
elif not isinstance(packages, list):
    raise SystemExit(f"{path}: packages must be an array")

replaced = False
for index, existing in enumerate(packages):
    if existing.get("version") == package["version"]:
        packages[index] = package
        replaced = True
        break
if not replaced:
    packages.append(package)

data = {
    "default": package["version"],
    "packages": packages,
}
path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
PY

echo "wrote $config_path"
echo "artifact: $artifact"
echo "bytes: $archive_bytes"
echo "md5: $archive_md5"
echo "url: https://huggingface.co/datasets/$repo_id/resolve/$revision/$artifact"

if [[ "$publish" == true ]]; then
  echo "publishing $artifact to Hugging Face dataset $repo_id"
  hf repos create "$repo_id" --repo-type dataset --public --exist-ok
  HF_XET_HIGH_PERFORMANCE="${HF_XET_HIGH_PERFORMANCE:-1}" hf upload \
    "$repo_id" \
    "$archive_path" \
    "$artifact" \
    --repo-type dataset \
    --revision "$revision" \
    --commit-message "Publish runtime data $version"
  echo "published: https://huggingface.co/datasets/$repo_id/resolve/$revision/$artifact"
  if [[ "$delete_zip" == true ]]; then
    rm -f "$archive_path"
    echo "deleted local archive: $archive_path"
  fi
fi
