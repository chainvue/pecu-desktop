#!/usr/bin/env bash
#
# The checks, as CI runs them — because CI runs this file.
#
#   scripts/check.sh
#
# There is one definition of "the checks" and it is here. The workflow in
# .github/workflows/ calls this script rather than repeating the commands, so
# the two cannot drift apart and be right about different things.
#
# `-D warnings` is not decoration. The README says warnings are not acceptable
# output, and a warning that only fails on somebody else's machine is a warning
# nobody fixes.
#
# It needs no display. The interface tests render offscreen through Slint's
# software renderer, which is what lets this run on a headless runner at all —
# verified by running the suite with DISPLAY and WAYLAND_DISPLAY unset.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

echo "── clippy ──────────────────────────────────────────────────────────────"
cargo clippy --workspace --all-targets -- -D warnings

echo "── tests ───────────────────────────────────────────────────────────────"
cargo test --workspace
