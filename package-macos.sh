#!/bin/sh
set -eu
cd "$(dirname "$0")"
if [ "$(uname -s)" != Darwin ]; then
    echo "The app bundle is for macOS. Use ./run.sh on other platforms." >&2
    exit 1
fi
if [ -x .tools/cargo/bin/cargo ]; then
    export CARGO_HOME="$PWD/.tools/cargo"
    export RUSTUP_HOME="$PWD/.tools/rustup"
    export PATH="$CARGO_HOME/bin:$PATH"
fi
if [ -x .tools/search/bin/rg ]; then
    search_binary="$PWD/.tools/search/bin/rg"
else
    search_binary=$(command -v rg) || {
        echo "ripgrep is required: cargo install ripgrep --locked --root .tools/search" >&2
        exit 1
    }
fi
cargo build --release --locked
mkdir -p dist/LuxeHarness.app/Contents/MacOS dist/LuxeHarness.app/Contents/Resources
cp target/release/luxeharness dist/LuxeHarness.app/Contents/MacOS/LuxeHarness
cp "$search_binary" dist/LuxeHarness.app/Contents/MacOS/rg
cp assets/ripgrep-LICENSE-MIT dist/LuxeHarness.app/Contents/Resources/ripgrep-LICENSE-MIT
cp assets/Info.plist dist/LuxeHarness.app/Contents/Info.plist
cp assets/AppIcon.icns dist/LuxeHarness.app/Contents/Resources/AppIcon.icns
echo "Built dist/LuxeHarness.app"
