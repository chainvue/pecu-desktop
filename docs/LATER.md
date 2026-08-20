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

- **Sending and registration have been done against a real chain; the identity
  operations have not.** A payment of 5 VRSCTEST to `dude.VRSCTEST@` was made
  from the interface on 2026-08-20 and confirmed at block 1197422, txid
  `68320bb5eb723ca3ab3f92d26133b4309d03c59e9ce3e93dba85d68379e98883` — the
  review screen showed exactly the two outputs the chain then recorded.
  `tests/live_send.rs` repeats that path on demand with a funded key.

  Locking, unlocking, revoking and recovering are covered by the scripted chain
  and by unit tests over their wording and their gating, and by nothing else. Each
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

## 1. Choosing a chain — what is left of it

**Status:** built. `Command::SetRequestedNetwork` is handled, `Paths` owns the
layout, the choice survives a restart, and Network is a tab in Settings with
the chooser. Switching relocks the wallet, reopens the vault, both databases,
the pending ledger and the reservation, resets the node list, clears every
cached figure and probes the new chain's active node.

The shipped node list **is** per chain as of `908eac4`: `BUILTIN_NODES` is gone
from `pecu-app` and `Network::builtin_nodes()` in `pecu-chain` answers with that
chain's endpoints and no others, so a wallet on VRSC no longer lists
`api.verustest.net` and marks it `WrongNetwork` once it answers. The table sits
with the rest of the chain knowledge rather than in the shell, and the demo
build's one scripted entry comes from `pecu_core::shipped_nodes` with the mock
flag, so it does not have to be special-cased at the call site.

Two things it does **not** do:

1. **Theme, reduce-motion and the auto-lock timer are per chain.** They are
   application preferences living in a per-chain database because that is the
   only database there is. A switch copies them across when the new chain has no
   answer of its own, which makes the common case behave — but two chains can
   still drift apart, and the honest fix is a settings store at the home level.
2. **Only the five chains that ship have a way in.** `Network::shipped()` now
   offers VRSC, VRSCTEST, vARRR, CHIPS and vDEX, each with its own public node,
   and `Network::Other` is carried everywhere with `dir_name()` sanitising one
   into a directory. What is missing is a way to name a *sixth* — the list is a
   constant in `pecu-chain`, and adding a node or a chain by hand was taken out
   of the interface deliberately. Putting it back is the same question as
   before: where a list of chains nobody shipped is supposed to come from, and
   how a wallet decides it is talking to the chain it was told about.

The read guard and the spend permit compared `requested` against `effective`
long before any of this, so the safety half was never the missing part.

---

## 2. Shielded — receiving, then sending

**Status:** the reading half is built and tested offline. The half that needs a
live server is blocked on somebody else's certificate.

### What is built

* `pecu_keystore::shielded` — the account a recovery phrase produces, derived
  under the data key with no passphrase prompt, so a scan can run on a timer.
  What leaves the keystore is `ShieldedView`: the diversifiable full viewing key
  and the `zs…` address, never the spending key. `coin_type` is **133 on both
  networks**, which is the Verus Mobile path; a wallet that used ZIP-32's
  testnet 1 would derive an account no other Verus wallet reaches from the same
  words. Eight tests, including one that walks BIP-39 → `m/32'/133'/0'` → bech32
  separately and compares, so it cannot pass by agreeing with itself.
* `pecu_chain::light` — `LightServer`, which cannot be constructed without
  having asked the server which chain it serves and having been told the right
  one. Same `Network::from_chain_name` the node health check uses. The guard
  matters more here than there: a transparent balance from the wrong chain is
  visibly wrong because the addresses do not match, and a shielded balance is
  one number with nothing on screen to contradict it.
* `pecu_core::shielded` — holds the viewing key and the `ScanResult`, folds in
  each tail with `absorb`, tells a lagging server apart from a reorg, and rolls
  back to the oldest *verifiable* checkpoint when the chain really did move.
  Six tests against the SDK's captured VRSCTEST blocks — a real note of 5
  VRSCTEST at position 3176, found, valued, and then worth nothing once its
  nullifier appears.

### What is blocked, and by what

**Verus's public testnet lightwalletd is serving an expired certificate.**
`lightwalletd.verustest.net:8125` presents a Let's Encrypt certificate for
`*.verustest.net` — so the hostname and port are right — that expired
**2026-08-11 17:01:37 UTC**, measured on 2026-08-20. Their auto-renewal has
stopped. `tests/live_light.rs` in `pecu-chain` fails on exactly this and says so
in its message.

There is no way around it that a wallet should take. Disabling certificate
verification to reach a shielded balance would hand every block this wallet
asks for to anyone on the path — which is the correlation a shielded address
exists to prevent — and `GrpcWebTransport` refuses plaintext to a non-loopback
host for the same reason. So this waits for the operator, and the offline tests
carry the weight until then.

### Nothing is written to disk, deliberately

A `ScanResult` is the shielded history: every note, with amounts and heights.
The wallet's databases are plain SQLite and only the vault is encrypted, so
persisting it would put a shielded balance and its history in a file any other
process can read — the exact property somebody chose a shielded address to
avoid. So the scan lives in memory and starts again next launch. `birthday` for
a new wallet is the tip, so the common case costs nothing; a restored wallet
pays a wait. Persisting it properly needs somewhere as protected as the keys
are, and that is its own decision rather than a line of code.

### One upstream defect found

`dfvk_from_bytes` **panics** on 128 bytes that are not a viewing key — the
panic is in `sapling-crypto` (`keys.rs:207`), below the SDK, so the `Result` it
returns can never carry that case. The SDK argues the opposite principle in
`derive_account`'s own documentation: it errors rather than panics on a bad
seed, "which is not acceptable at a library boundary that takes caller input".
Not a live hazard here — the only bytes that reach it were derived a moment
earlier from a phrase, with no path from a file, a socket or a text field — so
it is recorded by a `should_panic` test rather than papered over with
validation that would be dead code. The fix belongs upstream.

### The receive screen shows it

`Receive` has a shielded column beside the transparent address — the `zs…`
address, a copy button, and a plain statement that this wallet can be paid there
and cannot yet pay out of it. A key that arrived as a WIF gets the same column
saying why it has none, rather than the column being absent: somebody looking
for a z-address has to find out why there is not one, and a missing panel is not
an answer. Two reference images, both themes.

None of that touches the light server, so it works today.

**Two layout defects were found by looking at the rendered image, not by
reading the markup.** Three 320px columns beside the address card overflowed the
window and clipped the names panel off the right edge; and the shielded card
came out about fifty pixels short — exactly the two lines its closing paragraph
wraps onto — so the text rendered below the card's own bottom edge and over the
section beneath it. The second is the `Rectangle`-does-not-take-its-height-from-
a-`VerticalLayout` trap that `Notice` exists for, and the fix is the same:
`height: <inner>.preferred-height`, with the layout as the card's direct child.
Binding it while the layout sat behind an `if` did **not** work.

### What is left

* **Persistence**, on the terms above.
* **A balance on screen.** `pecu_core::shielded` computes one; nothing calls it
  from the actor yet, because the only server that could answer is the one with
  the expired certificate. The wiring is a scan on the same timer the portfolio
  uses, and it is deliberately not written blind.
* **Sending**, which is the `prover` feature: ~30 s of Groth16 on a background
  thread, and the ~50 MB parameter download below.

### The original research, still current

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

`crates/pecu-ui/tests/accessibility.rs` walks the live element tree on seven
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

---

## 4b. The miner fee on the launch review — needs one SDK field

**Status:** disclosed in words, not in figures. Blocked on the SDK, not on the
wallet.

The launch review names three amounts: the launch fee, the half that becomes the
new currency's reserve deposit, and the half that is burned with no output. All
three come off the signed outcome. On top of them sits the miner fee, which the
review currently handles with a sentence — *"The miner fee is on top of this and
is not included."* — because the wallet cannot know the number.

**Why it cannot.** `verus_tx_transparent::SignedTransaction` carries `fee`, the
exact figure including any dust folded into it. `verus_flows::prepare_launch`
receives it as `signed.fee` and then builds a `Launched` that has no field for
it, so it is dropped one line before the wallet could see it. The send path has
no such gap: `Sent` carries `fee` and the send review prints it.

**Why it is not reconstructed here.** It could be recomputed —
`estimate_fee(inputs, outputs + 1, DEFAULT_FEE_PER_KB, true)` over the decoded
transaction — and it would be wrong in exactly the case that matters least and
misleads most: when change fell below the dust threshold it is folded into the
fee, and the recomputation cannot see that. A figure that is right except
sometimes is worse on this screen than a sentence that is always right.

**The move.** One field on `Launched` — `miner_fee: Amount`, set from
`signed.fee` at `verus-flows/src/launch.rs:293` — then a fourth line on the
review and a fourth figure in `currency::cost`.

The pin has moved on since this was written — it is `a08d652d`, which is the
`verus-rust-sdk` `main` tip, so the `verus-keys` base58check fix this entry
wanted is already in. What is left is genuinely a change to the SDK: the field
does not exist and has to be added there first.

Proportion, so this is picked up with the right expectations: the miner fee is
on the order of 0.0001 coins against a launch fee of 200. It does not change
anyone's decision. What it changes is whether a wallet that says "here is what
this costs" is telling the whole truth, and that is worth one field.

---

## 4c. Which currencies may actually be a reserve

**Status:** the picker lists every currency the chain reports, and the wallet
checks nothing about whether a given one is a legal reserve.

A basket's reserves are chosen from `listcurrencies`, which is every currency on
the system. The wallet refuses a repeated reserve, a reserve with no weight, and
a set whose weights do not add to one whole — those are rules it can state and
does. It says nothing about whether a *particular* currency may be a reserve of a
*particular* basket, because that rule has not been established against a node
and this project does not guess at consensus.

Two things are known and neither is checked:

* A currency whose `start_block` is ahead of the tip has not begun. The picker
  **says so** on the row — `Starts at block …` — and lets it be chosen, because
  a basket scheduled to start later than its reserve is legitimate and refusing
  it here would be the wallet inventing a rule.
* Nothing stops somebody choosing a currency from another system. `system_id` is
  on every entry and is not read.

**The move,** when it matters: establish the rule against a daemon — build a
basket over an unstarted reserve and over a foreign-system one on VRSCTEST, and
record what the node says. Then either filter in `currency::choices` or refuse in
`currency::problems`, with the recorded reply cited the way `getcurrency`'s `-8`
and `getidentity`'s `-5` are. Until that measurement exists, a refusal here would
be a rule this wallet made up, and the failure mode is worse than the one it
would prevent: a legal basket the wallet will not let anybody build.

---

## 4d. The native menu bar — built, measured, taken out again

**Status:** written, rendered, reverted. The blocker is in Slint, not here.

Slint 1.17 has `MenuBar`/`Menu`/`MenuItem`, and the winit backend answers
`supports_native_menu_bar()` with `true` wherever `muda` is compiled in — so on
macOS a declared menu bar really is the strip at the top of the screen and
nothing is drawn in the window. A menu with Wallet (Lock, Refresh, Settings), Go
(the six screens with their existing ⌘1–⌘6, plus Settings on ⌘,) and View
(switch theme) was written and compiled.

**Why it came out.** `tests/visual.rs` and `docs/shots/` render through
`MinimalSoftwareWindow`, which has no native menu bar — so it draws the menu
*inside* the window, as a 38px band above the title bar, in **all 132**
reference images. The pictures this project reviews changes with would then show
a control the shipped application does not have, on every screen. That is the
same failure the fixtures already warn about in three places: an image that
describes a wallet the product cannot produce.

**Why it cannot simply be told otherwise.** `supports_native_menu_bar` is on
`WindowAdapterInternal`, which `i-slint-core` does not export. A wrapper around
`MinimalSoftwareWindow` cannot provide it, and there is no way in `.slint` to
declare a menu bar conditionally — `MenuBar` "must not be in a `for` or an `if`".

**What is not lost.** The backend installs a default native menu bar of its own
when an application declares none, so ⌘Q and the window menu already work. Every
shortcut the menu would have advertised — ⌘1–⌘6, ⌘L, ⌘R, ⌘K, ⌘, — is in the
focus scope in `app.slint` and keeps working. What is missing is discoverability, for
somebody who looks in the menu rather than pressing keys.

**The move,** in order of preference: take it when Slint exposes native-menu
support to a custom `WindowAdapter`, or when the snapshot renderer moves to a
windowed backend. Failing both, split `AppWindow` into a `Window` that carries
the `MenuBar` and an inner shell the snapshots render — which is the normal way
to structure this and was judged too large a change to the one compile root for
what it buys today.

**There is no Edit menu in any version of this.** Slint 1.17 gives an
application no clipboard and no way to ask which element has focus — `TextInput`
owns both, which is why the receive screen copies by selecting its own element.
Cut/Copy/Paste entries could only ever act on one hard-coded field.

---

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

**Status:** the bundle is built and runs. Signing and notarisation are not done,
and cannot be done from here.

`scripts/bundle.sh` produces `target/Pecu.app` — icon, `Info.plist`, the
release binary — out of macOS built-ins only (`sips`, `iconutil`, `plutil`,
`codesign`). The icon is rendered from `ui/icon.slint` by
`cargo run -p pecu-ui --example render_icon`, so it follows the palette
rather than being a bitmap nothing keeps in step.

**Measured**, on this machine (M-series, `--release`, `lto = "thin"`,
`strip = true`): 2m06s to build, **37 MB** binary, 38 MB bundle. `__text` is
24 MB of it — that is FemtoVG, winit, tokio, rustls and the SDK, not debug
information, which the profile already strips. It has been started, and started
twice to check the instance guard refuses the second.

**What is left, and it needs credentials this machine does not have:**

* `CODESIGN_IDENTITY="Developer ID Application: …" scripts/bundle.sh` signs with
  `--options runtime --timestamp`, which is what notarisation requires.
* Notarisation itself is an App Store Connect API key and a round trip to Apple.
  The four commands are printed by the script at the end of a run rather than
  executed, because nobody should hand credentials to a script they have not
  read.

Until it is notarised the bundle runs on the machine that built it and Gatekeeper
blocks it everywhere else. That is correct behaviour and not a bug in the bundle.

**Not attempted:** a universal binary. This is an arm64 build; an Intel slice
needs `cargo build --target x86_64-apple-darwin` and `lipo`, and nothing has been
built or run on Intel.

---

## 7. The 16px application icon

**Status:** measured, and the mark does not survive the size. What ships there
is legible and is not the mark.

The icon is `@` and a cursor, drawn from `ui/icon.slint` and rendered at every
size macOS asks for. At 32px and above it is the wordmark reduced to its two
halves and it reads. At **16px it does not**: the glyph has a counter inside a
counter, and rendered at that size — inspected pixel for pixel, nearest
neighbour, not a smoothed upscale — it is an amorphous blob. Enlarging it to
fill the square was tried first and produced a larger blob.

So below 32px the mark becomes a path: an `@` reduced to a ring broken on the
right with a tail. The honest report is that **at 16px the break and the tail do
not survive either**. What lands is a clean ring. That is a legible mark instead
of a smudge, which is the trade worth making, and it is not recognisably an `@`.

**The design package asks for exactly the thing that fixes this** and does not
ship it: its Windows spec says "16/24px: nur @ ohne Cursor (Pixel-Hinting)" —
hand-hinted pixel art, drawn *to* the grid rather than scaled onto it. That is a
designer's task, not a developer's, and it is why this is parked rather than
attempted again.

**Where it shows:** the Finder list, the window proxy icon, the ⌘-Tab strip, and
the Windows `.ico` at 16 and 24. Everywhere else the real mark is what renders.

**The move,** when somebody picks it up: get 16 and 24 as hand-drawn PNGs, and
have `render_icon.rs` use them for those two sizes instead of rasterising the
component. The loop that draws every size is already per-size, so this is a
branch in one function rather than a new pipeline.

### What the icon reaches today, per platform

`render_icon.rs` writes all three sets from `ui/icon.slint`, and
`crates/pecu-app/tests/packaging.rs` checks the files without needing the
platform they are for.

* **macOS — done, end to end.** `scripts/bundle.sh` runs `iconutil` over the
  checked-in iconset and `target/Pecu.app` carries `AppIcon.icns`. Built and the
  icon inspected. Note this only reaches the **bundle**: `cargo run` produces a
  bare binary, and macOS gives one of those a generic Dock icon whatever it
  contains.

* **Linux — installed by a script nobody has run on Linux.** `assets/hicolor/`
  holds eight sizes and `assets/pecu.desktop` names them, and since `ec83ce3`
  `scripts/install-linux.sh` puts both where a desktop already looks:
  `install -Dm644` into `share/icons/hicolor/*/apps/` and
  `share/applications/` under a prefix that defaults to `~/.local`, then
  `gtk-update-icon-cache` and `update-desktop-database` best-effort. It replaces
  every file it wrote on a re-install, and swaps the binary through a temporary
  name so re-installing over a running Pecu cannot hit `ETXTBSY`.

  **None of it has been run on Linux.** The workspace itself has been — clippy
  and 371 tests on Ubuntu, at `33bf5e5` — but that is `cargo test`, not this
  script, and the two prove different things. **`StartupWMClass=pecu` is the
  documented default and is unverified**; it is what attaches the icon to the
  *window* rather than only to the launcher, and it has to match the WM class
  winit sets. A `.deb`, an AppImage or a Flatpak is still nobody's job.

* **Windows — the file exists, the executable does not carry it.** `assets/
  pecu.ico` holds seven sizes as PNG-in-ICO, written by hand rather than by a
  crate, and the container is checked. For Explorer and the taskbar to show it,
  the icon has to be linked into the `.exe` as a resource — which needs a build
  script and a resource compiler (`embed-resource`, `winres`), and that is a
  dependency decision rather than a line of code. **Nothing here has been opened
  on Windows.** PNG-in-ICO is understood from Vista onward; a shell older than
  that wants BMP entries for the small sizes.

---

## 8. ~~The price chart~~ — done, and what it cost

**Status:** done. Kept because what it took is worth knowing before the next
thing needs a method the SDK does not have.

`getcurrencystate` always took an optional `"from, to, step"`. Nothing in the
pinned SDK could send it: `currency_state` passes one parameter, `call` and
`call_raw` are private, `RequestBody::new` is `pub(crate)` and `Method` is a
closed enum. There was no way round it from this side — the transport is a
public extension point and *composing a request* deliberately is not.

So the SDK gained `ChainReader::currency_state_range`, on the `price-history`
branch of `chainvue/verus-rust-sdk`, and the pin moved off `8f01520` — which
also brings the commits that had accumulated, including the `verus-keys`
base58check fix §4b wanted. **That branch has since landed** as `#192`, and the
pin is `a08d652d`: `main`'s tip, not a fork of it.

### What it turned up

- **VRSCTEST barely moves.** Bridge.vETH published one reserve state in thirty
  days. Its chart is a flat line, and that is the honest picture. `vrealv1` is
  the one pool with movement — six supply steps, 0.23% — and the scripted chain
  now carries it for exactly that reason.
- **There is no honest 24-hour column.** Sampling daily and then labelling the
  difference `24h` is a figure that looks precise and is not: the newest sample
  can be most of a day old. The column is `30d`, which is what the data
  supports. Hourly sampling would fix the label at seven hundred readings per
  pool per screen.
- **A two-hop price needs a two-hop history.** `series` first refused anything
  it could not price through one pool, while `quote_for` happily priced through
  two — so the one currency on the chain that moves showed a price, a change of
  `—` and an empty chart at once. The samples are joined on **block time**, to
  the newest hop reading at or before each point: two pools do not notarize in
  the same block, so an exact match finds nothing and matching by position plots
  one currency's price against another's clock.
- **The mock read the clock per call.** `clock_at` called `SystemTime::now()`
  every time, so two reads of the same block disagreed by however long the calls
  were apart — invisible until something joins two series on their timestamps.
  The scripted chain now freezes its tip's time when it is built.

### What is still open here

The read is **one range call per started pool** — about twenty on VRSCTEST,
sequential, when somebody opens the screen. That is a few seconds behind a
spinner. Batching is not available: `getcurrencystate` takes one currency.

---

## 9. Signing a conversion — done, and what it still cannot vouch for

**Status:** built. `plan_conversion` → `prepare_conversion` → `broadcast`, with
a review screen decoded from the signed bytes, the `SpendPermit` on the
broadcast and the pending ledger underneath it. `convert_build.rs` is the
integration test: six cases through the SDK's real builder against a scripted
chain, with `ScriptedReader::broadcasts()` asserting nothing was sent.

What is left is not code. It is evidence.

**Nothing has been converted on a real chain.** VRSCTEST has `disabledefi` in
force since block 1 187 000 — the wallet now reads that from the chain's own
oracle rather than finding out by being refused, see `pecu_core::upgrade` — so
every conversion is rejected — which is why this was built anyway, and also
why the one thing a conversion flow most needs cannot be had yet. The moment
DeFi is re-enabled, the first move is a real conversion of a small amount of
VRSCTEST into a fractional and back, and reading what the daemon says at each
step. Until then the encoding rests on the SDK's own live test, which did
confirm on chain.

**The transfer fee is a constant.** `convert::TRANSFER_FEE_SATS` is 20 010
satoshis because that is what every conversion in the SDK uses, including the
live test whose transaction VRSCTEST accepted. Nothing derives it, and
`estimateconversion`'s `fee` is a different number — what the pool charges, in
the source currency. If a chain ever wants a different transfer fee this is the
line that has to learn where to ask.

**The token side has never run.** Converting a token rather than the chain's own
currency gathers reserve outputs through `convert::token_inputs`, which filters
to outputs carrying exactly the source currency because the builder refuses
anything else. That path is exercised by nothing: the demo wallet holds
Bridge.vETH but the scripted chain has no reserve utxos to hand it, so every
test here converts natively. A `with_reserve_utxo` fixture would cover the
filter; only a chain covers the rest.

**Preconversions are unreachable and should stay that way for now.**
`Book::route` filters unstarted pools, so `ConversionKind::Preconvert` cannot
be built by accident — which matters, because the chain rejects a preconvert
and an ordinary conversion at opposite sides of the same block height. Offering
preconversion deliberately is a separate screen with a separate warning: a
launch that misses its `min_preconversion` refunds everything, and that is the
ordinary outcome rather than a rare one.

**Burn is not offered at all.** The SDK has `burn`/`prepare_burn` and this
wallet deliberately does not reach them. It destroys value with no output
paying anything back, and it belongs behind its own confirmation, on its own
screen, not as a currency you can pick in the same list as the rest.

