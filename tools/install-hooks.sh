#!/usr/bin/env bash
set -euo pipefail

HOOK_DIR=".git/hooks"
HOOK_FILE="$HOOK_DIR/pre-push"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOK_SOURCE="$SCRIPT_DIR/pre-push"

chmod +x "$HOOK_SOURCE"
mkdir -p "$HOOK_DIR"

ln -sf "$HOOK_SOURCE" "$HOOK_FILE"
echo "✅ Pre-push hook symlinked to $HOOK_SOURCE"
