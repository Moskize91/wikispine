#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/build-service-image.sh [options]

Build the lightweight Wikispine HTTP service image. The image contains only
the wikispine runtime binary; runtime data must be mounted at container runtime.

Options:
  --tag <name>        Image tag (default: wikispine-service:local)
  --platform <value>  Docker target platform (default: linux/amd64)
  --push             Push the image instead of loading it locally
  --load             Load the image locally (default)
  -h, --help         Show this help

Example:
  scripts/build-service-image.sh --tag wikispine-service:0.1.0 --load
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tag="wikispine-service:local"
platform="linux/amd64"
output_flag="--load"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)
      tag="$2"
      shift 2
      ;;
    --platform)
      platform="$2"
      shift 2
      ;;
    --push)
      output_flag="--push"
      shift
      ;;
    --load)
      output_flag="--load"
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

if ! command -v docker >/dev/null 2>&1; then
  echo "required command not found: docker" >&2
  exit 1
fi

echo "building $tag for $platform"
docker buildx build \
  --platform "$platform" \
  -f "$repo_root/docker/Dockerfile" \
  -t "$tag" \
  "$output_flag" \
  "$repo_root"
