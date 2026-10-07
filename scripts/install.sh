#!/bin/sh
# Install the latest Lume release (or LUME_VERSION) for Linux or macOS.
#
#   curl -fsSL https://github.com/DeepBlueDynamics/lume/releases/latest/download/install.sh | sh
#
# Environment:
#   LUME_VERSION      tag to install, e.g. v0.12.1 (default: latest release)
#   LUME_INSTALL_DIR  destination directory (default: $HOME/.local/bin)
set -eu

repo="DeepBlueDynamics/lume"
install_dir="${LUME_INSTALL_DIR:-$HOME/.local/bin}"

fail() { echo "lume install: $*" >&2; exit 1; }
need() { command -v "$1" > /dev/null 2>&1 || fail "missing required command: $1"; }
need curl
need tar
need uname

case "$(uname -s)" in
  Linux) os="unknown-linux-gnu" ;;
  Darwin) os="apple-darwin" ;;
  *) fail "unsupported OS $(uname -s); on Windows use install.ps1" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch="x86_64" ;;
  aarch64 | arm64) arch="aarch64" ;;
  *) fail "unsupported CPU $(uname -m)" ;;
esac
target="$arch-$os"

tag="${LUME_VERSION:-}"
if [ -z "$tag" ]; then
  tag="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$tag" ] || fail "could not find the latest release of $repo"
fi

name="lume-$tag-$target"
base="https://github.com/$repo/releases/download/$tag"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "Installing lume $tag ($target) into $install_dir"
curl -fsSL "$base/$name.tar.gz" -o "$tmp/$name.tar.gz" || fail "no build for $target in $tag"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || fail "could not download SHA256SUMS"

expected="$(grep " $name.tar.gz\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)"
[ -n "$expected" ] || fail "$name.tar.gz is not listed in SHA256SUMS"
if command -v sha256sum > /dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1)"
fi
[ "$expected" = "$actual" ] || fail "checksum mismatch for $name.tar.gz"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$install_dir"
install -m 0755 "$tmp/$name/lume" "$install_dir/lume"

echo "Installed $("$install_dir/lume" --version)"
case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) echo "Add $install_dir to your PATH, e.g.: export PATH=\"$install_dir:\$PATH\"" ;;
esac
