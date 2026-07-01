#!/usr/bin/env sh
set -eu

repo="Moskize91/wikispine"
version="latest"
bin_dir="${HOME}/.local/bin"

usage() {
  cat <<'USAGE'
Usage: scripts/install.sh [options]

Install the prebuilt wikispine CLI binary from GitHub Releases.
This installs only the CLI. Runtime data is installed later with `wikispine init`.

Options:
  --version <vX.Y.Z>  Release version to install (default: latest)
  --bin-dir <dir>     Binary install directory (default: ~/.local/bin)
  -h, --help          Show this help
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      version="$2"
      shift 2
      ;;
    --bin-dir)
      bin_dir="$2"
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

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "required command not found: $1" >&2
    exit 1
  fi
}

need curl
need tar

case "$(uname -s)" in
  Darwin)
    os="macos"
    ;;
  Linux)
    os="linux"
    ;;
  *)
    echo "unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
esac

case "$(uname -m)" in
  x86_64|amd64)
    arch="x86_64"
    ;;
  arm64|aarch64)
    arch="aarch64"
    ;;
  *)
    echo "unsupported architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

if [ "$os" = "linux" ] && [ "$arch" = "aarch64" ]; then
  echo "linux-aarch64 release assets are not published yet" >&2
  exit 1
fi

if [ "$version" = "latest" ]; then
  api_url="https://api.github.com/repos/${repo}/releases/latest"
  version="$(curl -fsSL "$api_url" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
  if [ -z "$version" ]; then
    echo "could not resolve latest release version" >&2
    exit 1
  fi
fi

case "$version" in
  v*)
    package_version="${version#v}"
    ;;
  *)
    echo "version must use a v-prefixed release tag, e.g. v0.1.0" >&2
    exit 1
    ;;
esac

artifact="wikispine-${package_version}-${os}-${arch}.tar.gz"
base_url="https://github.com/${repo}/releases/download/${version}"
tmp_dir="$(mktemp -d)"

cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

echo "downloading ${artifact}"
curl -fsSLo "${tmp_dir}/${artifact}" "${base_url}/${artifact}"
curl -fsSLo "${tmp_dir}/${artifact}.sha256" "${base_url}/${artifact}.sha256"

(
  cd "$tmp_dir"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "${artifact}.sha256"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "${artifact}.sha256"
  else
    echo "required command not found: sha256sum or shasum" >&2
    exit 1
  fi
)

tar -xzf "${tmp_dir}/${artifact}" -C "$tmp_dir"
mkdir -p "$bin_dir"
install -m 755 "${tmp_dir}/wikispine-${package_version}-${os}-${arch}/wikispine" "${bin_dir}/wikispine"

echo "installed ${bin_dir}/wikispine"
"${bin_dir}/wikispine" --version

case ":${PATH}:" in
  *":${bin_dir}:"*) ;;
  *)
    echo "note: ${bin_dir} is not on PATH"
    ;;
esac
