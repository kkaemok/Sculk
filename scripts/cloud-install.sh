#!/usr/bin/env bash
set -euo pipefail
export CARGO_HOME=/workspace/.cargo
export RUSTUP_HOME=/workspace/.rustup
export PATH="$CARGO_HOME/bin:$PATH"
if [ ! -x "$CARGO_HOME/bin/rustup" ]; then
  installer=$(mktemp /tmp/sculk-rustup.XXXXXX)
  trap 'rm -f "$installer"' EXIT
  curl --fail --show-error --location https://sh.rustup.rs -o "$installer"
  sh "$installer" -y --no-modify-path --profile minimal --default-toolchain stable
fi
rustup toolchain install stable --profile minimal --component rustfmt --component clippy
cd /workspace/Sculk
cargo +stable fetch --locked
cargo +stable build --workspace --release --all-features --locked
cargo +stable test --workspace --all-features --locked
cargo +stable run -p sculk-basic-client --locked
