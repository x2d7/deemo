#!/usr/bin/env bash
# Release build for the native game.
#
#   scripts/build.sh            # -> native/target/release/deemo
#
# The v0.1.0 release binary was built with rustc 1.99.1 on Fedora with a
# project-local toolchain, that is, with these environment variables exported
# before calling cargo:
#
#   export CARGO_HOME=/path/to/deemo/.cargo
#   export RUSTUP_HOME=/path/to/deemo/.rustup
#   export PATH="$CARGO_HOME/bin:$PATH"
#
# The script does not set them for you (the paths are machine-specific) — it
# only uses whatever toolchain is active. Point CARGO_HOME/RUSTUP_HOME at a
# pinned toolchain if you want to reproduce the release exactly; otherwise any
# recent stable Rust with edition 2024 support works.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root/native"

echo "rustc: $(rustc --version)"
echo "cargo: $(cargo --version)"
echo "building into $root/native/target/release"

cargo build --release "$@"

bin="$root/native/target/release/deemo"
echo "binary: $bin ($(du -h "$bin" | cut -f1))"
