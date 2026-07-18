#!/usr/bin/env bash
set -euo pipefail

REPO="jyasuu/chromium-mcp"
BINARY="chromium-mcp"
INSTALL_DIR="${OKF_INSTALL_DIR:-$HOME/.local/bin}"

detect_platform() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux)
      case "$arch" in
        x86_64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64) echo "aarch64-unknown-linux-gnu" ;;
        *)       echo "unsupported"; return 1 ;;
      esac
      ;;
    Darwin)
      case "$arch" in
        arm64)   echo "aarch64-apple-darwin" ;;
        x86_64)  echo "x86_64-apple-darwin" ;;
        *)       echo "unsupported"; return 1 ;;
      esac
      ;;
    MINGW*|MSYS*|CYGWIN*)
      case "$arch" in
        x86_64)  echo "x86_64-pc-windows-msvc" ;;
        *)       echo "unsupported"; return 1 ;;
      esac
      ;;
    *)
      echo "unsupported"; return 1 ;;
  esac
}

get_version() {
  local version="$1"
  if [ -z "$version" ] || [ "$version" = "latest" ]; then
    version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
      | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
  fi
  echo "$version"
}

main() {
  local version="${1:-latest}"
  local platform

  platform="$(detect_platform)" || { echo "Error: unsupported platform"; exit 1; }
  version="$(get_version "$version")"

  local ext="tar.gz"
  local binary_name="$BINARY"
  if [[ "$platform" == *"windows"* ]]; then
    ext="zip"
    binary_name="${BINARY}.exe"
  fi

  local archive="${BINARY}-${version}-${platform}.${ext}"
  local url="https://github.com/$REPO/releases/download/${version}/${archive}"

  echo "Installing $BINARY $version for $platform..."
  echo "  Download: $url"

  local tmpdir
  tmpdir="$(mktemp -d)"
  trap "rm -rf '$tmpdir'" EXIT

  curl -fsSL "$url" -o "$tmpdir/$archive"

  if [ "$ext" = "tar.gz" ]; then
    tar xzf "$tmpdir/$archive" -C "$tmpdir"
  else
    unzip -q "$tmpdir/$archive" -d "$tmpdir"
  fi

  mkdir -p "$INSTALL_DIR"
  mv "$tmpdir/$binary_name" "$INSTALL_DIR/$BINARY"
  chmod +x "$INSTALL_DIR/$BINARY"

  echo "Installed $BINARY to $INSTALL_DIR/$BINARY"
  echo ""
  echo "Add to PATH if not already:"
  echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
}

main "$@"
