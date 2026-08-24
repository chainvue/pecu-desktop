#!/usr/bin/env bash
#
# The checks, as CI runs them — because CI runs this file.
#
#   scripts/check.sh          # all four, which is what you want at your desk
#   scripts/check.sh clippy   # the compiler's opinion
#   scripts/check.sh test     # the suite
#   scripts/check.sh mock     # the suite the default feature set compiles away
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
# # The half nothing had ever run
#
# `mock` is not a default feature, so the `test` half compiles six of
# pecu-core's test files to empty binaries — send_prepare, send_corroboration,
# demo_chain, currency_launch, currency_from_a_new_name and identity_changes
# are all `#![cfg(feature = "mock")]`. They report "0 passed; 0 filtered out"
# and the run goes green. That is thirty-nine tests — 3, 11, 12, 4, 2 and 7 —
# on top of the 528 the `test` half counts at this commit: 526 across its test
# binaries and two doc-tests. They are the only offline coverage of
# `send::prepare`, of corroborating a payment against a second node, of the
# five ways an identity is changed, of defining a currency and of the scripted
# chain demo: everything that builds and signs a transaction without a node.
# Nothing here had ever run them.
#
# Both figures are re-measured rather than carried forward, because they are
# the kind that drifts silently: every one of them is a number about a run
# nobody is looking at.
#
# `--features mock` rather than `--all-features`, and today those are the same
# command. This workspace declares exactly one feature — `mock`, in pecu-app,
# pecu-chain and pecu-core — and its one optional dependency is taken as
# `dep:pecu-mock`, which adds no implicit feature of its own; with the tree
# built one way the other compiles nothing and exits in a second. What differs
# is the next feature somebody adds. `--all-features` would sweep it in
# silently, which is the argument for it and equally the argument against: the
# features this workspace is likely to grow are the heavy ones — the SDK's
# `prover` path, a second renderer — and a check whose contents are the union
# of every manifest gets slower, or red, for a reason nobody can attribute to
# their own commit. Named, it says what it runs, and
# `grep -rn 'cfg(.*feature' crates/*/src crates/*/tests` is how to find out
# whether it still says all of it. `src` as well as `tests`, because a
# `#[cfg(all(test, feature = "mock"))]` module inside a crate is compiled away
# by the `test` half exactly as quietly as a whole file is, and this half would
# run it. Today that returns nineteen sites: those six files, and thirteen in
# pecu-chain's and pecu-core's `src`, none of which is a test module.
#
# `--exclude pecu-ui`, because the feature cannot reach it: pecu-ui depends on
# pecu-protocol, pecu-chart, slint, qrcode and chrono, none of which has a
# `mock` feature, so its thirteen test files — fourteen binaries, counting the
# lib's own unit tests, which `--exclude` drops too — are the same bytes the
# `test` half already built and ran. They are also the largest single share of
# the suite's runtime: 55 tests, and a little over two fifths of the seconds
# the `test` half spends inside test binaries — three warm runs at d04a74d on
# the machine this was written on came out at 42%, 43% and 47%, with
# tests/visual.rs alone about a third of the half every time. The absolute
# figures are not worth writing down: those same three runs were 91, 174 and 81
# seconds of test time on one machine, so it is the share that travels to a
# runner and not the clock. What `--exclude` does not skip is compiling
# pecu-ui: pecu-app depends on the lib, so this half still pays for the crate,
# just not for its tests. Everything else stays selected, not just the six
# files — those thirteen `#[cfg(feature = "mock")]` sites in pecu-chain and
# pecu-core mean the rest of pecu-core's suite is running against a differently
# compiled crate, which is the other thing this half is for.
#
# Its own machine, rather than a second command inside `test`, and cost is not
# the argument — measured, cost says the opposite. With the default tree
# already built, adding this feature set recompiles three crates in 26 seconds
# and 720 MB of `target` — against the twenty minutes and six gigabytes the
# first build cost on the machine that was measured. Folding it in would be
# nearly free, and a second machine pays that first build again. What the
# second machine buys is a result of its own. A command appended to `test` runs
# only if the 528 tests before it passed, and lands inside their green tick
# when it does — and a suite whose result nobody could see is the entire
# disease being treated here. `fail-fast: false` in the workflow is the same
# judgement about the halves that already existed.
#
# Clippy's mock pass is folded into the clippy half instead, and that asymmetry
# is deliberate. It links nothing and runs nothing, so on a machine that
# already holds the clippy artefacts it is twelve seconds and two megabytes —
# and it has no result of its own to report, being one lint run seeing more
# code rather than a second suite. Putting it on the machine that compiles the
# mock *tests* would be two sets of artefacts on one runner, which is what the
# split at the top of this file exists to prevent. It is not decoration:
# `too_many_lines` had been failing in currency_from_a_new_name.rs at 102 lines
# against 100 for as long as that file existed, and nobody had seen it, because
# nothing had ever linted a target the default feature set compiles away.
#
# # The half that compiles nothing
#
# `deny` is the odd one: it compiles nothing. `cargo deny` reads Cargo.lock and
# the RustSec advisory database and answers four questions the other three
# halves cannot — is anything in the graph known-vulnerable or abandoned, does
# every crate's licence match what this project is allowed to ship, does
# anything come from a remote nobody chose, and has a crate that touches key
# material quietly arrived at two versions. The policy and the reasoning behind
# every exception are in `deny.toml` at the repository root; read that file
# rather than this one when the check fails.
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
# regenerated lockfile, and all four checks quietly build something else. That
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
  clippy|test|mock|deny|all) ;;
  *) echo "usage: ${BASH_SOURCE[0]##*/} [clippy|test|mock|deny]" >&2; exit 2 ;;
esac

if [ "$what" = clippy ] || [ "$what" = all ]; then
  echo "── clippy ────────────────────────────────────────────────────────────"
  cargo clippy --locked --workspace --all-targets -- -D warnings
  # And again with the one feature that is off by default, because
  # `--all-targets` does not reach a target the feature set compiles away. See
  # "The half nothing had ever run" above for why this rides here rather than
  # on the machine that runs those tests.
  cargo clippy --locked --workspace --all-targets --features mock -- -D warnings
fi

if [ "$what" = test ] || [ "$what" = all ]; then
  echo "── tests ─────────────────────────────────────────────────────────────"
  cargo test --locked --workspace
fi

if [ "$what" = mock ] || [ "$what" = all ]; then
  echo "── the gated suite ───────────────────────────────────────────────────"
  cargo test --locked --workspace --exclude pecu-ui --features mock
fi

if [ "$what" = deny ] || [ "$what" = all ]; then
  echo "── supply chain ──────────────────────────────────────────────────────"
  # `--deny warnings` for the same reason clippy gets it: cargo-deny reports a
  # policy that no longer matches the tree — an ignored advisory that has been
  # withdrawn, an allowed licence nothing uses any more — as a warning, and a
  # policy nobody has to correct is a policy that stops describing anything.
  cargo deny --locked --manifest-path Cargo.toml check --deny warnings
fi
