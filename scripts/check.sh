#!/usr/bin/env bash
#
# The checks, as CI runs them — because CI runs this file.
#
#   scripts/check.sh          # both, which is what you want at your desk
#   scripts/check.sh clippy   # one half
#   scripts/check.sh test     # the other
#
# There is one definition of "the checks" and it is here. The workflow in
# .github/workflows/ calls this script rather than repeating the commands, so
# the two cannot drift apart and be right about different things.
#
# # Why it takes a half
#
# Not for your benefit — run it with no argument. It is for the runner. Clippy
# and rustc key their metadata differently and share nothing, so running both
# leaves two complete sets of artefacts in `target`: forty gigabytes on the
# machine this was written on, and more than a four-core runner with sixteen
# gigabytes of memory would survive. CI gives each half its own machine.
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

what="${1:-all}"

case "$what" in
  clippy|test|all) ;;
  *) echo "usage: ${BASH_SOURCE[0]##*/} [clippy|test]" >&2; exit 2 ;;
esac

if [ "$what" = clippy ] || [ "$what" = all ]; then
  echo "── clippy ────────────────────────────────────────────────────────────"
  cargo clippy --workspace --all-targets -- -D warnings
fi

if [ "$what" = test ] || [ "$what" = all ]; then
  echo "── tests ─────────────────────────────────────────────────────────────"
  cargo test --workspace
fi
