#!/usr/bin/env bash
#
# The checks, as CI runs them — because CI runs this file.
#
#   scripts/check.sh          # all three, which is what you want at your desk
#   scripts/check.sh clippy   # the compiler's opinion
#   scripts/check.sh test     # the suite
#   scripts/check.sh deny     # the dependency graph
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
#
# # The third half
#
# `deny` is the odd one: it compiles nothing. `cargo deny` reads Cargo.lock and
# the RustSec advisory database and answers four questions the other two halves
# cannot — is anything in the graph known-vulnerable or abandoned, does every
# crate's licence match what this project is allowed to ship, does anything
# come from a remote nobody chose, and has a crate that touches key material
# quietly arrived at two versions. The policy and the reasoning behind every
# exception are in `deny.toml` at the repository root; read that file rather
# than this one when the check fails.
#
# It is here rather than in a workflow step of its own because the point of
# this script is that there is one list of what "green" means. A check that
# only exists in CI is a check you cannot run before pushing.
#
# # Why every invocation takes `--locked`
#
# Without it cargo is free to re-resolve and rewrite Cargo.lock in place, which
# is how a pull request comes back green against a dependency graph that exists
# on no machine and in no commit: widen a semver range, forget to commit the
# regenerated lockfile, and all three checks quietly build something else. That
# is not a general hygiene point here — it is this script's own subject.
# `deny` judges the graph cargo resolves for it, so a graph nobody committed is
# a supply-chain answer about nothing, and the workflow's cache key names a
# Cargo.lock that was not the one built. With `--locked` a stale lockfile fails
# immediately and names the manifest that moved, identically at a desk and on
# the runner.
#
# It wants `cargo-deny` on PATH: `cargo install --locked cargo-deny`, or the
# prebuilt binary the workflow downloads. `deny.toml` is written against the
# 0.20 schema, and an older cargo-deny rejects several of its keys outright
# rather than ignoring them — so a confusing "unknown field" failure means the
# tool is too old, not the policy wrong.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

what="${1:-all}"

case "$what" in
  clippy|test|deny|all) ;;
  *) echo "usage: ${BASH_SOURCE[0]##*/} [clippy|test|deny]" >&2; exit 2 ;;
esac

if [ "$what" = clippy ] || [ "$what" = all ]; then
  echo "── clippy ────────────────────────────────────────────────────────────"
  cargo clippy --locked --workspace --all-targets -- -D warnings
fi

if [ "$what" = test ] || [ "$what" = all ]; then
  echo "── tests ─────────────────────────────────────────────────────────────"
  cargo test --locked --workspace
fi

if [ "$what" = deny ] || [ "$what" = all ]; then
  echo "── supply chain ──────────────────────────────────────────────────────"
  # `--deny warnings` for the same reason clippy gets it: cargo-deny reports a
  # policy that no longer matches the tree — an ignored advisory that has been
  # withdrawn, an allowed licence nothing uses any more — as a warning, and a
  # policy nobody has to correct is a policy that stops describing anything.
  cargo deny --locked --manifest-path Cargo.toml check --deny warnings
fi
