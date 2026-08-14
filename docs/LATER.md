# Parked

Work that is understood, deliberately not started, and would otherwise be
rediscovered from scratch. Each entry says what is missing, what has already
been checked, and what the first move is — so picking it up does not begin with
another day of reading the SDK.

Ordered by what has to happen first, not by size.

---

## 0. What Phase 5 left behind

VerusIDs are built: finding, looking up, reading the content map, registering,
updating, locking, unlocking, revoking and recovering. One name — `maker` — was
registered on VRSCTEST for real.

Two things it did **not** finish, recorded here rather than in a summary that
scrolls away:

- **Only registration has been done against a real chain.** Locking,
  unlocking, revoking and recovering are covered by the scripted chain and by
  unit tests over their wording and their gating, and by nothing else. Each
  changes an identity in a way a test double cannot vouch for. Revocation in
  particular deserves a run on an identity registered *with a separate recovery
  authority*, since one registered without cannot be revoked at all — which is
  itself the thing worth confirming.
- **Recovery does not restore the primary addresses.** The SDK allows it, and a
  recovery legitimately may need it: the point of recovering is usually that the
  old keys are gone. Offering it without a screen built for that would be the
  most dangerous default in the application, so the current recovery only clears
  the revocation. A proper version needs its own review step.

Not started and out of scope for that phase: **PBaaS**. The SDK cannot build a
cross-chain transfer — all three `TransferDestination` constructors hard-code
`gateway: None` — so this waits on the SDK, not on the wallet.

---

## 1. Choosing a chain — VRSC and VRSCTEST

**Status:** the protocol has the command; nothing handles it.

`Command::SetRequestedNetwork` exists in `chainvue-protocol` and reaches the
actor's match arm as dead weight. `crates/chainvue-app/src/main.rs` hard-codes
`network: Network::Testnet`, and every path below it — vault, store, log
directory — is built from that one constant.

The eventual goal is every mainnet PBaaS chain. **Not now.** The immediate scope
is exactly two: VRSC and VRSCTEST.

What it actually costs, in order:

1. **Per-network directories.** `~/Library/Application Support/com.chainvue.wallet/{mainnet,testnet}/` already
   exists as a shape — `testnet/` is real, `mainnet/` has never been written.
   The vault, `wallet.sqlite` and `cache.sqlite` all move under it. This is the
   whole reason testnet figures can never render as mainnet, and it is why the
   directory is in the path rather than a column in the database.
2. **Reopening them.** Switching networks means closing a vault, closing two
   SQLite connections, and re-running `restore()` against different files. The
   actor owns all three by value, so this is a method on the actor rather than
   anything reentrant — but it must relock the wallet, because a DEK unlocked
   for one vault has no meaning for another.
3. **The node list follows the network.** `NodeManager` holds nodes for one
   chain. Switching has to swap the shipped defaults and reload the user's saved
   nodes for that network, or the wallet lands on a node it will then refuse to
   read from — see `reading_the_wrong_chain()`, which is already in place and
   would correctly block it.

The read guard and the spend permit both already compare `requested` against
`effective`, so the safety half of this is done. What is missing is only the
plumbing that makes `requested` a variable.

---

## 2. Shielded — receiving, then sending

**Status:** researched against the SDK, nothing written.

The user's stated goal. Testnet lightwalletd is `lightwalletd.verustest.net:8125`
and is currently the only one.

What was verified in the SDK:

- `verus-sapling` — scanning, ZIP-32 derivation, z-address encoding, note
  building, parameter loading.
- `verus-light` — a lightwalletd client speaking **grpc-web over HTTP/1.1**, so
  it needs no HTTP/2 stack and no async runtime. It fits the existing
  `spawn_blocking` model unchanged.
- Features: `light` (network + shielded + verus-light + verus-flows/shielded)
  and `prover`.
- `verus_flows::shielded::prepare_spend(light, reader, params, request)` returns
  `Unsent<ShieldedSpent>`, and `.broadcast(broadcaster)` finishes it. **The same
  shape as the transparent path** — which means the review step, the
  `SpendPermit`, the pending ledger and the `BroadcastUncertain` protocol all
  carry over rather than being rebuilt.

Verus shielded is unmodified Zcash Sapling: same circuit, byte-identical MPC
parameters. The only Verus-specific value anywhere on the path is the consensus
branch id in the sighash.

**Staging, and why in this order:**

- **(a) Balance first** — features `shielded` + `light`, *no* `prover`. A
  z-address to receive at, trial decryption to find notes, a note store, and
  scan progress in the UI. This is where most of the new machinery lives, and it
  needs no 50 MB download and no `bellman`.
- **(b) Sending second** — feature `prover`. Roughly thirty seconds of Groth16
  proving on a background thread, with real progress, and a proving-parameter
  distribution problem that is unresolved (see below).

**Two things to decide before starting (a):**

- **The parameters cannot go in git.** ~50 MB across two files, with SHA-256
  enforced by the SDK — for a stated reason: wrong parameters do not fail
  loudly, and maliciously constructed ones break the zero-knowledge property, so
  the proof leaks what it exists to hide. So: download on first shielded use,
  verify the hash, cache under the application directory, and show the download
  as a first-class step rather than a hang.
- **A WIF-imported key can never have shielded keys.** Shielded derivation is
  ZIP-32 from the seed. A key that arrived as a WIF has no seed and never will.
  The UI has to say so at the point somebody asks for a z-address, not fail
  later.

---

## 3. Screen-capture exclusion on the seed screen

**Status:** deliberately deferred, with the value re-examined and found lower
than the plan assumed. Kept, not dropped.

Step 5 of the seed screen's lifecycle in the plan: `NSWindow.sharingType = .none`
on macOS, `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` on Windows. Both
need `objc2` / `windows`, and both need `unsafe`, which this workspace sets to
`forbid` at the root — so this costs a narrowly scoped `allow(unsafe_code)` in a
platform module, not just an afternoon.

**What it actually protects against**, in order of how real each one is:

1. **Screen sharing.** Somebody on a call opens their backup and twenty-four
   words go to everyone watching.
2. **Their own screenshot.** People photograph a recovery phrase to "save" it,
   and it lands in `~/Desktop`, in Photos, in iCloud. This is the same failure
   the missing Copy button exists to prevent.
3. Screen-recording malware — but anything that can do that can also log the
   keystrokes of the passphrase, so it is not an argument.

**Why the value is smaller than it looks.** The screen is hold-to-reveal: the
words are on screen only while the pointer button is held, and are overwritten
and cleared on release, on leaving, after sixty seconds, and on losing the
window. So the screen-share exposure is seconds rather than indefinite, and
⌘⇧4 — which is itself a click-and-drag — is genuinely awkward to perform while
holding a button down. Neither of the two real cases is wide open to begin with.

**And it may not work.** `NSWindow.sharingType` is deprecated on current macOS,
and whether `.none` excludes the *built-in* screenshot tool — as opposed to
capture by other processes — needs measuring rather than assuming. Windows'
`WDA_EXCLUDEFROMCAPTURE` is unambiguous. A camera pointed at the screen defeats
both.

**So when this is picked up, measure first.** If `.none` does not stop ⌘⇧4 on the
macOS being shipped to, the API buys almost nothing here and the honest move is a
line on the screen saying a screenshot is possible and a bad idea — which
addresses case 2, the only one a person controls.

Everything else on that screen is done: per-word models, hold-to-reveal,
overwrite-then-clear, no copy button, the verify step returning one bool.

---

## 4. The VoiceOver listening pass

**Status:** the mechanical half is done and enforced; the half that needs ears
is not.

`crates/chainvue-ui/tests/accessibility.rs` walks the live element tree on seven
seeded screens and fails if any control a screen reader can reach has no name,
if any text input is left unnamed, or if the receive address is not exposed in
speakable groups. That is the failure mode that actually ships — a button
announced as "button" and nothing else — and it now cannot.

What it cannot check is whether the interface makes sense **heard**: whether the
reading order is followed, whether wording written to sit beside an icon still
works as sound, whether a change is announced at a moment that helps. No
automated check reaches any of that.

The pass itself is short. Turn VoiceOver on with ⌘F5, ignore the screen, and go
through the send flow with Tab and the VoiceOver arrow keys. Arriving at a sent
payment without looking is the whole test. What to listen for specifically:

- the recipient field announcing what it is *before* what is in it;
- the review step reading the outputs as outputs, not as a run of numbers;
- the address in groups, slowly enough to write down;
- "sent" being announced at all, from the live region, without stealing focus.

Note that `build.rs` emits Slint debug info in unoptimised builds only, which is
what makes the element tree inspectable. It is off in release deliberately.

## 5. Keychain opt-in

**Status:** planned, never started. `keyring` is not a dependency.

From the plan's table, and it has not changed:

| Item | Keychain? | Why |
|---|---|---|
| Vault passphrase | Opt-in, **off by default** | Storing it downgrades "knows a secret" to "has a login session". |
| DEK | **Never** | Bypasses Argon2 permanently and unlocks every key at once. |
| Keys / phrases | **Never** | They belong in the vault, which travels with a backup. |
| RPC basic-auth for a private node | **Yes** | Needed without user presence for background polling. |

The last row is the one with a real use today, and it is also the smallest.

---

## 6. Packaging

**Status:** never attempted.

There is no `.app` bundle, no icon, no `Info.plist`, no signing, no
notarisation, and `cargo build --release` has never been run — so the release
profile, the Skia renderer decision and the binary size are all unmeasured.

This is not hard, but "never once run" is worth writing down rather than
assuming.
