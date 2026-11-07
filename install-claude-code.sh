#!/bin/sh
set -eu
cd "$(dirname "$0")"
command -v npm >/dev/null 2>&1 || { echo "Install Node.js/npm, then run this script again." >&2; exit 1; }
npm install --prefix "$PWD/.tools/native" --no-audit --no-fund --save-exact @anthropic-ai/claude-code@2.1.312
"$PWD/.tools/native/node_modules/.bin/claude" --version
