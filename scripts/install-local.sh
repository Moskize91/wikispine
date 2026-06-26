#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/install-local.sh [options]

Build and install the local wikispine CLI, then install runtime data into
the platform default data directory used by `wikispine`.

Options:
  --source <dir>    Runtime data source directory (default: data/runtime)
  --data-dir <dir>  Runtime data install directory
  --bin-dir <dir>   CLI binary install directory (default: ~/.local/bin)
  --replace         Replace an existing runtime data install directory
  -h, --help        Show this help
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_dir="$repo_root/data/runtime"
bin_dir="$HOME/.local/bin"
replace=0

case "$(uname -s)" in
  Darwin)
    data_dir="$HOME/Library/Application Support/wikispine/runtime"
    ;;
  Linux)
    data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
    data_dir="$data_home/wikispine/runtime"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    data_base="${LOCALAPPDATA:-$HOME/AppData/Local}"
    data_dir="$data_base/wikispine/runtime"
    ;;
  *)
    echo "unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
esac

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source)
      source_dir="$2"
      shift 2
      ;;
    --data-dir)
      data_dir="$2"
      shift 2
      ;;
    --bin-dir)
      bin_dir="$2"
      shift 2
      ;;
    --replace)
      replace=1
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

if [[ ! -f "$source_dir/manifest.json" && ! -f "$data_dir/manifest.json" ]]; then
  echo "runtime data not found at source or destination:" >&2
  echo "  source: $source_dir" >&2
  echo "  destination: $data_dir" >&2
  exit 1
fi

echo "building wikispine release binary"
cargo build --release -p wikispine-runtime --bin wikispine

echo "installing CLI to $bin_dir/wikispine"
mkdir -p "$bin_dir"
install -m 755 "$repo_root/target/release/wikispine" "$bin_dir/wikispine"

if [[ -f "$source_dir/manifest.json" ]]; then
  if [[ "$source_dir" != "$data_dir" ]]; then
    if [[ -e "$data_dir" ]]; then
      if [[ "$replace" -eq 1 ]]; then
        echo "removing existing runtime data at $data_dir"
        rm -rf "$data_dir"
      else
        echo "destination already exists: $data_dir" >&2
        echo "rerun with --replace to overwrite it" >&2
        exit 1
      fi
    fi
    echo "moving runtime data from $source_dir to $data_dir"
    mkdir -p "$(dirname "$data_dir")"
    mv "$source_dir" "$data_dir"
  fi
else
  echo "runtime data already installed at $data_dir"
fi

echo "verifying installed CLI and runtime data"
"$bin_dir/wikispine" status

echo "done"
