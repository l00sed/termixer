#!/usr/bin/env bash
set -euo pipefail

REPO="l00sed/termixer"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.cargo/bin}"

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os/$arch" in
    Linux/x86_64)  echo x86_64-unknown-linux-gnu ;;
    Linux/aarch64) echo aarch64-unknown-linux-gnu ;;
    Darwin/arm64)  echo aarch64-apple-darwin ;;
    Darwin/x86_64) echo x86_64-apple-darwin ;;
    *) echo "Unsupported platform: $os/$arch" >&2; exit 1 ;;
  esac
}

get_latest_version() {
  curl -sL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -o '"tag_name":"[^"]*"' | cut -d'"' -f4
}

main() {
  local target version url
  target="$(detect_target)"
  version="$(get_latest_version)"

  if [ -z "$version" ]; then
    echo "Failed to fetch latest release version" >&2
    exit 1
  fi

  url="https://github.com/$REPO/releases/download/$version/termixer-$version-$target.tar.gz"
  echo "Downloading termixer $version for $target..."

  mkdir -p "$INSTALL_DIR"
  curl -sL "$url" | tar xz -C "$INSTALL_DIR" termixer
  echo "Installed to $INSTALL_DIR/termixer"
}

main "$@"
