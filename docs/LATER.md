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

## 0b. Identities and Currencies are built, and switched off

**Status:** built, screenshotted, covered offline, and never run against a
chain — out of the rail deliberately, and until the two runs below have
happened.

`crates/pecu-ui/ui/app.slint` carries `hidden: true` on the `identities` and
`currencies` entries of `nav`, ⌘8 and ⌘9 are unbound, and `Core::search`
answers with addresses and currencies and nothing else. The execution is
clean: there are no dangling entry points. `Actions.refresh-identities()` is
called from one place — the Refresh button on the hidden screen — and
`ScreenId::Identities` is produced only by the rail, so `IdentityState.rows` is
empty by construction and the two things outside that screen which read it, the
nav chip and Receive's "or by name" panel, draw nothing.

That is 44 of the 150 reference images, over 22 screens and states, reachable
from nowhere.

**Why it is off, said properly rather than in five code comments.** It was
written in `app.slint`, in `Core::search`, in `wire_search`, in the palette
overlay and on `NavItem.hidden`, and the reason all five gave was product
scope: this build is the ordinary wallet, and identities and the currencies
they define are a second product on top of it. That is true and it is not the
blocker. The blocker is the one §0 already
names: **the identity write operations have never been run against a real
chain.** Shipping the rail entry without those runs ships untested transaction
paths that alter identities, revocation and recovery among them.

§0 is generous about the coverage that does exist, and this is the place to be
exact about it. "Locking, unlocking, revoking and recovering are covered by the
scripted chain" — as *states*, yes: the script seeds an identity that is
locked, one that is unlocking and one that is revoked, and the wallet's reading
of each is tested. As *operations*, they were covered by nothing at all until
`tests/identity_changes.rs`, which now builds and signs all five against the
scripted chain and measures that none of them reaches the network. That closes
the half that can be had for nothing. The chain still refuses every broadcast,
so what has never happened is still what §0 said had never happened.

**And a second gate §0 does not mention.** A currency has never been defined or
launched against a real chain either. `crates/pecu-core/tests/currency_launch.rs`
records why in its own header — two hundred coins on VRSCTEST, and it cannot be
undone. Its four tests build and sign against the scripted chain and stop one
step short of the network. Nothing in this file recorded that gate before now,
and Currencies rides on the same two bools as Profile: an exit criterion drawn
from §0 alone would authorise shipping the Currencies screen on identity
evidence.

**Exit criterion — two runs, one per gate,** recorded here the way §0 records
the payment to `dude.VRSCTEST@`: txid, block, and what the screen said
beforehand.

1. **Five txids from `tests/live_identity.rs`, and the blocks they confirmed
   in, written into this entry** — one each for authorities, lock, unlock,
   revoke and recover — together with the answer to the question the run is
   there to settle: whether consensus accepts a revocation of an identity that
   is still counting down. The file asks the chain and prints the answer either
   way; nowhere else in this tree says.

   **Not "the test went green".** It is `#[ignore]`d, and every precondition it
   cannot satisfy is a skip that returns early from a test which then reports as
   passing: no `PECU_LIVE_SEND`, no `PECU_LIVE_IDENTITY`, a key that is not
   among the subject's primary addresses, a subject that is its own recovery
   authority, an authority this key cannot sign for, a balance too short to pay
   five fees — or, on the branch that only recovers an identity an earlier run
   left revoked, too short to pay one. That is deliberate — running the whole suite with `--ignored`
   should not fail on a wallet nobody provisioned — and it means a green
   `live_identity` on an unset environment is evidence of nothing. The txids are
   the evidence.

   The order is forced and the file says why: authorities, lock, unlock, revoke,
   recover, in one test, because a revoked identity cannot be updated and a
   recovery needs something to act on. It needs a subject whose recovery
   authority is a **second identity the same key controls** — a revocation whose
   subject is its own recovery authority is refused before a signature exists,
   and a freshly registered identity is exactly that.
2. One currency defined and launched, by hand, from an identity claimed for the
   purpose — and the record has to show two things, not one: the currency
   answering `getcurrency` after its start block, and the three figures on the
   launch review checked against the transaction the chain kept. The second is
   the point of doing it by hand. §4b records that this screen omits the miner
   fee, which is exactly the kind of discrepancy only a hand-run catches.
   Written down here, not turned into a test — each run costs a name and two
   hundred coins and consumes the identity, since an identity defines exactly
   one currency. A harness that pretended otherwise would be a harness nobody
   could run twice.

**What the flip costs, so it is on the table before somebody starts.** It is
not two booleans.

- **134 of the 150 reference images move.** Every snapshot renders the whole
  window at 1240×800 with the rail expanded, and `visual.rs` compares pixels
  with a per-channel tolerance of 1 and a noise budget. Two new rail entries
  change the rail in every image that shows the shell, and only eight of the
  seventy-five draw over it: the two onboarding states, the two locked states,
  and the four backup steps, which take the whole window. So 67 names have to be
  re-recorded and re-reviewed. The 44 identity and currency images change twice
  over — they also gain a *selected* rail entry, which none of them has today
  (see `docs/shots/identities-light.png`: the Profile screen, with no Profile
  in the rail).
- **The shortcut numbers stop counting down the rail.** `app.slint` states the
  invariant about itself: "In rail order, so the number is what somebody counts
  down to". `Network` is still `hidden`, so Profile is the *seventh* visible
  entry while `nav-index` calls it 7 and the comment reserves ⌘8 for it.
  Binding 8 and 9 would give the seventh item ⌘8 and leave ⌘7 dead. That is a
  design decision, not a mechanical bind.
- **Search is two restorations, not one.** `1142a29` deleted two loops, one per
  hidden screen — the identities these keys control, and the currencies they
  define — and only the first goes back cheaply.
  - *Restoring the identities* is **three edits, a string and an assertion**:
    one loop in `palette_hits`, one arm in `wire_search`, the icon branch in
    `overlay.slint` — which is binary today,
    `hit.kind == "address" ? "send" : "currency"` — plus the placeholder, which
    names the two kinds it searches, and
    `the_palette_puts_addresses_above_currencies` in `lib.rs`, which pins the
    kinds to exactly those two and goes red for anybody who did the other four.
  - *Restoring the currencies you define* is that again and a **third kind**.
    `"currency"` no longer means what it meant when the loop was deleted: it is
    now a market hit, and `wire_search` routes it to the markets detail through
    the i-address `OpenMarket` takes. A definition hit reusing that string would
    open the wrong screen carrying the right label, and the row would look
    correct in the list on the way there. So it needs a `kind` of its own, its
    own icon branch, its own arm in `wire_search` — and the assertion becomes
    three kinds rather than two.
- **The German catalogue is stale.** `crates/pecu-ui/translations/de/LC_MESSAGES/pecu-ui.po`
  has `Identities`; the rail label was renamed to `Profile`, which has no
  entry. A German window would show an English word the moment the rail shows
  it.
- **Two features come alive, and four reference images already show them.** The
  nav chip and Receive's "or by name" panel are both driven by
  `IdentityState.rows`. `receiving` in `crates/pecu-ui/src/fixtures.rs` sets one
  row on purpose and `receiving_without_a_phrase` builds on it, so
  `receive-light.png`, `receive-dark.png` and the two `receive-no-phrase` shots
  carry `robert.VRSCTEST@` twice over: in the OR BY NAME panel and on the chip
  at the foot of the rail. Neither feature is unphotographed and neither is
  unreviewed. What is unrepresentative is every *other* fixture: they leave
  `rows` empty, and `shell.slint` draws the chip only `if root.account != ""`,
  so it is **absent** rather than blank — a wallet that controls a name
  photographed as one that does not. Whether that is worth fixing is a
  judgement about how far a reference image has to be a session somebody could
  actually have had. It is not a blocker and it is not another 134 images; it
  overlaps the re-recording the rail already forces.

**What is already in place, and the one thing that is not.**
`ScreenEntered(Identities)` and `ScreenEntered(Currencies)` both refresh, so
both lists fill on arrival — the currency walk chains off `finish_identities`
rather than starting at `enter_screen`, because a currency is a flag on an
identity and the identity list has to exist before it can be walked.
`accessibility.rs` already walks both screens, and the rail items are labelled
by one loop in `shell.slint` that `hidden` only makes invisible.
`shortcuts.rs` needs nothing unless the shortcuts change.

The new-block poller is the exception, and only for Currencies. It re-reads the
identities on a new block **while Identities is the screen being polled** —
`Core::finish_tip` tests `self.polling.screen == ScreenId::Identities` and
nothing else — so somebody sitting on Currencies when a block lands sees
neither list move: the currency walk hangs off a `finish_identities` that the
poller never starts there. `Core::refresh` does not cover it either; it reads
balances and history and never touches identities.

The arm is one extra screen in that condition and it is deliberately **not**
being added by this change. This change touches no code path that puts a
request on the wire, the screen the arm would fix cannot be reached, and an arm
that nobody can exercise until the flip is a behaviour change with no way to
observe it — the same argument that keeps the two booleans where they are. It
belongs to the flip, so it is written down here beside the images and the
shortcut numbers rather than half-done now.

**First move.** Provision the two testnet identities `live_identity.rs` asks
for and run it. Everything before that step is done: `identity::prepare` is a
free function the test drives instead of duplicating the wallet's own dispatch,
`tests/identity_changes.rs` covers the same five changes offline, and
`pecu-mock` can seed a subject whose recovery authority is somebody else —
which it could not, because every scripted identity was its own, so no
revocation could be prepared against any of them. And while writing the result
into this entry: there is still no one page saying how to run the live suite.
Ten files document themselves and `README.md` mentions none of them — the eight
`live_*.rs` in `crates/pecu-core/tests`, plus `crates/pecu-chain/tests/live_light.rs`
and `crates/pecu-chain/tests/live_probe.rs`, each of which carries its own
`--ignored --nocapture` line and is part of the page that does not exist.

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

## 1b. The three shipped chains whose identity is never cross-checked

**Status:** built for two chains of five. The other three need a value nobody
in this tree has.

`Node::record_success` no longer believes a node on its reported chain name
alone: where `Network::chain_id` pins an id, `ChainInfo::chain_id` has to bear
the name out, and a node whose two claims disagree is `Unidentified` — not read
from, not spent through. That covers VRSC and VRSCTEST.

It does not cover vARRR, CHIPS or vDEX, and those are the awkward three. They
carry real coins, and they are now the only shipped chains whose identity is
taken entirely on the node's word.

What is missing here is corroboration, not consent. The spending guard asks
`Network::may_be_real_money`, which is true for every chain but VRSCTEST, so all
three sit behind the same typed confirmation VRSC does and the word it asks for
is the chain's own name. The gap this entry is about is narrower and harder: a
wallet on one of these three has nothing to hold a node's *chain identity*
against.

The coins are a separate question, and since #44 they are not covered either.
`pecu_chain::corroborate` held the outputs a transparent payment or a shield
would spend against the shipped endpoint before anything was signed, but only
when the active node was one the user added — and that is no longer a thing a
wallet can be. Nothing runs it now; §14 is the entry that would. It would not
have closed this one regardless: it says nothing about which chain either
endpoint is on. Chain identity rests on the node's word.

**Why they cannot simply be derived.** A root chain's currency id *is* the id of
its own name — `hash160(sha256d(lowercase(name)))`, which
`verus_sdk::vdxf::root_namespace` does offline and which
`the_pinned_chain_ids_are_the_ones_the_derivation_produces` holds the two
existing pins to. A PBaaS chain is not a root chain: vARRR is registered under
VRSC, so its id is `identity_id("vARRR", VRSC)`. Deriving it from its own name
here would produce a value no node ever sends and would reject every honest
vARRR endpoint.

**First move.** Read each id from that chain's own `getinfo`, against the
endpoint `Network::builtin_nodes` already ships for it, and record where it came
from the way the oracle constants in `network.rs` do. Not a value derived
off-machine and typed in from memory: one wrong character in a pin is not a
weaker guard, it is an outage that refuses every honest node on that chain, and
it would look exactly like the chain being down.

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
  one. Same `Network::from_chain_name` the node health check reads a name with —
  though the node check no longer stops at the name, and holds it against the
  chain's own id (see 1b); `GetLightdInfo` carries no second statement to do
  that with. The guard matters more here than there: a transparent balance from
  the wrong chain is visibly wrong because the addresses do not match, and a
  shielded balance is one number with nothing on screen to contradict it.
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

### Nothing was written to disk — and then it was, sealed

A `ScanResult` is the shielded history: every note, with amounts and heights.
The wallet's databases are plain SQLite and only the vault is encrypted, so
persisting it would put a shielded balance and its history in a file any other
process can read — the exact property somebody chose a shielded address to
avoid. So the scan lived in memory and started again next launch.

That is honest and it is unusable. A VRSCTEST scan from Sapling activation is
about twelve hundred requests and three minutes; paying it on every start makes
the shielded balance a thing the wallet is permanently in the middle of finding
out, and the first thing anybody says about it is that it keeps scanning from
the beginning.

**Resolved 2026-08-21**, on the terms this paragraph asked for: somewhere as
protected as the keys are. `Vault::seal_blob` / `open_blob` seal arbitrary bytes
under the vault's data key — the same key the recovery phrase is under, held
only while the wallet is open — with the wallet id and a purpose in the AAD, so
a blob cannot be moved between wallets or presented as a different one. The
sealed string goes in the **cache** database, not the durable one, because every
byte of it can be recomputed by asking a server again; the birthday, which
cannot, is a setting in the durable one.

`tests/shielded_kept.rs` reads the database file back and greps the raw bytes
for the address, the value, the height and the JSON field names. That is the
assertion this section is worth anything for.

Two things were needed alongside it, and both are the sort that only show up
once the state is durable:

- **Locally-spent notes are saved immediately**, not at the next scan. The
  marker exists to cover the minute between broadcasting a shielded spend and
  the block arriving; leaving it in memory alone means a wallet closed inside
  that minute reopens willing to spend the note again.
- **A reorg deeper than the scan can verify now discards the kept scan.** In
  memory that was a bad minute. On disk it is a wallet that restores poisoned
  state, fails identically, and never scans again.

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

### Sending is built too — all three directions

`prover` and `multicore` are on, so a shielded spend is compiled in and live;
`pecu_chain::light` carries what that costs in trust. The send form routes on
the pair (source pool, destination kind):

| Route | Where it lives | Proven live? |
|---|---|---|
| `t→z` | `pecu_core::shield` | **Yes** — a real Groth16 proof, 12.5 s in a debug build, 2171 bytes, valid txid |
| `z→z` | `shielded::{plan_spend, prove_spend}` | Planning tested; **the proof has never run** |
| `z→t` | the same, different recipient | as above |

**The Sapling parameters are found, not downloaded, wherever a node already has
them.** They are the stock Zcash ceremony files byte for byte, so
`~/Library/Application Support/ZcashParams`, `~/.zcash-params`,
`%APPDATA%\ZcashParams` and `/usr/share/zcash-params` are searched **before**
the wallet's own directory — a machine running a node must not accumulate a
second fifty-megabyte copy. Verified on this machine: both files present, both
hashes equal to the SDK's pinned constants, loaded in 7.4 s.

Two things worth knowing before touching this again:

* **`t→z` needs no light server.** A shield spends no notes, so there is nothing
  to witness and the anchor is the empty tree. That is why it is the one
  direction provable while the certificate below is expired.
* **The route decides what becomes public, and it is never inferred.** The
  source pool is a control, not a default — picking whichever balance covers the
  amount would decide somebody's privacy silently.

All four routes share one `send::Signed` enum and therefore one review, one
`SpendPermit` and one pending-ledger commit. `finish_broadcast` did not change.
Four parallel paths would have been four chances to forget the ledger.

### The endpoint was the wrong protocol, and that is now fixed

`Network::light_server()` named `https://lightwalletd.verustest.net:8125` for
testnet. **That endpoint cannot serve this wallet, and no certificate renewal
would change it.**

`verus-light` speaks **grpc-web over HTTP/1.1** on purpose — no HTTP/2 stack, no
async runtime, one transport for a desktop build and a wasm one. lightwalletd
speaks **native gRPC over HTTP/2**. Port 8125 is the second kind: it sends an
HTTP/2 SETTINGS frame the instant a socket opens, before any request, and
answers an HTTP/1.1 `GET` with the same. Ports 80, 443, 8080, 8081 and 9067 on
that host were checked too — nothing there serves grpc-web.

The expired certificate is what hid this: the connection failed before anything
could notice the protocol was wrong. Two faults, one symptom, and the second was
the load-bearing one.

`light_server()` now returns `None` for every chain, and a test says why. The
SDK's own example points at `http://127.0.0.1:8080` — a **local grpc-web
proxy** — which is the shape this was always meant to have.

> **Superseded 2026-08-21.** The wallet speaks native gRPC now, so port 8125 is
> reachable and `light_server()` names it again. The diagnosis above is still
> exactly right about what was wrong; only the remedy changed. See *A transport
> that speaks lightwalletd's own protocol*, below.

### The proxy, and what it unlocked — superseded, and worth keeping

`scripts/grpcweb-proxy.mjs` is sixty lines of Node standard library — `http` and
`http2`, no install, no module download, no container. It accepts grpc-web over
HTTP/1.1 on loopback and forwards to native gRPC over HTTP/2, translating the
trailers. The wallet's own transport is untouched and still refuses plaintext to
anything but loopback; the only reason it will talk to this is that 127.0.0.1
*is* loopback, which the SDK allows for exactly this deployment.

With it running, the read path is proven against the **live chain** rather than
against committed bytes:

```
version   v0.3.0-197-g1b13d05     chain VRSCTEST
branch id 76b809bb                tip   1198574
```

`crates/pecu-core/tests/live_shielded.rs` — four tests — finds the SDK's real
note at block 1 167 987 worth 5 VRSCTEST by asking the server, watches it become
worthless eight blocks later when its nullifier appears, continues a scan across
a call boundary (`1167995..=1167995`, no rollback), and confirms a stranger's
key sees none of it.

### `z→z` and `z→t`: everything but the coin

The last gap is not a protocol or a certificate any more. It is **funds**.

Proving a shielded spend needs a note *this wallet owns*, and the only way to
get one is to shield first. The SDK's captured fixtures deliberately publish
only a viewing key, so their notes cannot be spent by anyone; and there is no
VRSCTEST faucet at any of the obvious names.

`crates/pecu-core/tests/live_shielded_spend.rs` does the whole chain in one run
— `t→z`, then `z→z`, then `z→t`, each broadcast and each waited for — and needs
a funded WIF:

```sh
INSECURE=1 node scripts/grpcweb-proxy.mjs &   # while the upstream cert is expired
export PECU_LIGHT_URL=http://127.0.0.1:8080
export PECU_LIVE_SEND=1
export PECU_LIVE_WIF=<a funded VRSCTEST WIF>
cargo test -p pecu-core --test live_shielded_spend -- --ignored --nocapture
```

It generates **its own** recovery phrase for the shielded account, because a WIF
has none and never will, shields the transparent coin into that account, spends
inside the pool, and sends the remainder back to the WIF's address.

### Birthdays: recorded where they are a fact, and nowhere else

**Status:** built.

A first scan has to start somewhere, and the wallet now writes down where for
the one case it can defend: **a key generated here**. Fresh entropy has no
history, so the chain tip at the moment of generation is a correct floor, and it
is written per key — one height for a whole wallet would be one account's answer
applied to another's, in the direction that skips blocks.

The tip is usually not known at that moment, and this is the part the first
attempt got wrong. A key is generated during onboarding, seconds after launch,
and the node probe has not come back; writing nothing in that case looked safe
and was nearly useless — measured against a real testnet endpoint the tip was
unknown *every* time, so the birthday was recorded never and every new wallet
still walked the whole chain. So the moment is recorded instead and settled into
a height at the first tip, with the elapsed wall-clock converted to blocks at
**half** Verus' block target and a hundred-block floor under it. Every rounding
in that estimate goes the same way — earlier, meaning more scanning. Until it
settles the account is not scanned at all, because a scan started before the
birthday is known is the whole chain, which is the thing being avoided.

An **imported phrase** gets nothing, and this is the part worth not
"improving". The same words may have been in another wallet for years, so when
this wallet derived the account says nothing about when the account was first
paid. An earlier version recorded the tip at the moment a light server was
configured and hid a real payment of 10 VRSCTEST, sixty-three blocks the wrong
side of the line. An import walks the chain once and then never again, which is
the right way round: slow is recoverable, short is not.

If no node has reported a tip when a key is generated, nothing is recorded and
that key gets the same full first scan. Guessing there would trade a wait nobody
minds for a balance that is quietly wrong.

### A transport that speaks lightwalletd's own protocol

**Status:** built, and proven against two real servers.

Everything above solved the protocol mismatch by putting a translator in front
of lightwalletd. That works, and it has a cost nobody was paying attention to:
the translator has to be *somebody's*. What shipped was `lwd.chainvue.io` —
chainvue's own box, behind a Cloudflare tunnel — which meant every user's block
requests went through the machine of the people who wrote the wallet. For a
privacy feature that is the wrong default, and the fact that it was the only
option is not a defence.

`crates/pecu-chain/src/grpc.rs` is a native gRPC transport: HTTP/2 over rustls,
`Content-Type: application/grpc`, and the gRPC status read out of HTTP/2
trailers. The message framing is byte-identical to grpc-web, so the SDK's
request encoder and response decoder are untouched on both sides of it — the
new code moves bytes and reads one header block.

`LightServer::connect` now **probes** both dialects, native first, and keeps
whichever answered. It has to probe: ALPN cannot decide this, because
`lwd.chainvue.io` negotiates HTTP/2 at Cloudflare's edge while still speaking
grpc-web underneath. Measured, both ways:

```
https://lightwalletd.verustest.net:8125 -> native gRPC
https://lwd.chainvue.io                 -> grpc-web
```

So the shipped testnet address is Verus' own server again, reached directly, and
a proxy — or a lightwalletd on `http://127.0.0.1:9067`, which needs no
certificate — remains a valid thing to type.

**What it cost:** six crates — `h2`, `http`, `tokio-util`, `tokio-rustls`, and
the `mio`/`socket2` sockets that `tokio/net` pulls in. `rustls`, `webpki-roots`,
`tokio`, `bytes`, `slab`, `fnv`, `indexmap` and the `futures-*` family were
already compiled in this tree.

**One defect this found, which no fixture would have.** h2 charges every DATA
frame under 256 bytes as framing overhead and hangs up at a default budget of a
hundred frames in flight — a defence against a peer flooding empty frames.
lightwalletd streams one message per block and an empty testnet block compacts
to a few dozen bytes, so a thousand-block range is a thousand undersized frames
and every live scan died mid-range with `too_many_data_frames`. The budget is
raised, the flow-control window bounds what can actually be in flight, and
`a_thousand_small_frames_are_a_block_range_and_not_an_attack` reproduces the
failure against a real h2 server on loopback — it fails if that line is
reverted, which is the only reason it is worth having.

A full scan from Sapling activation to the tip measures at **about three
minutes**, against roughly four through the proxy.

### What is left

* **A scan is only written down when it comes back.** The worker holds the
  state and the actor holds the store, so the save happens once, at the end —
  including at the end of a scan that *failed* part way, which is why a hiccup
  no longer costs the whole pass. Quitting the application mid-scan still does:
  the worker result never arrives. Three minutes, once, and only if somebody
  quits during the first scan. Fixing it means the worker reporting state per
  stride rather than progress per stride, which is a bigger change than the
  thing it buys.
* **A "rescan from block N" control.** A scan starts at Sapling activation and
  continues from where it stopped; there is no way to ask for an earlier height
  after an import, short of forgetting the wallet.
* **The parameter download has no progress.** `params::fetch` reports it and no
  screen shows it. Instead the send is refused *before* dispatch when the files
  are absent, so nothing hangs — but somebody without a node has to fetch them
  by hand today.
* **Memos.** `ShieldedRecipient::with_memo` is right there and no field offers
  one.
* **Change goes to the note's own address, not a fresh diversified one.** Better
  for privacy would be a new address per spend; doing that silently would move
  where somebody's change lives without telling them, so it wants its own
  screen.

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

---

## 10. Sending a token — signposted, not built

**Status:** the absence is now said out loud; the capability is not there.

`SendDraft` has four fields and none of them names a currency
(`pecu-protocol/src/models.rs`), `send::prepare` calls
`verus_flows::send::prepare_send`, which builds a native output, and `validate`
compares the typed amount against the **native** spendable balance. So the send
path is not "untested for tokens" — there is no slot to put a token into. What
a person could do until now was read `48.5000 0000` off the Bridge.vETH row,
type `48.5` into Send, and pay somebody 48.5 of the chain's own coin: a valid
transaction, an amount they believed was their token, and a review step that
shows the amount and the recipient and never the currency, because there is
only ever one.

The dashboard and the send form now say which currency can be sent, and a token
asset row is a shortcut into Convert pre-filled with that currency. That is a
signpost, not a fix.

**And the signpost has a blind spot of its own.** Both sentences are gated on
`holds_tokens`, which the bridge folds out of the asset rows — and a portfolio
read whose `token_balances` call failed produces no token rows at all
(`portfolio::read` logs the warning and drops the result without even setting
`reading.failure`). So the person whose tokens could not be counted sees an
asset list that under-reports what they hold and no sentence about it, rather
than being told the count is unknown. `PortfolioVm::tokens_unknown` exists for
exactly that distinction and is still read by nobody; whoever wires it up should
know the caption is its second consumer.

**What building it costs.** Less than it looks, because both halves already
exist. The SDK has `send::prepare_send_token(reader, key, currency, to, amount,
token_utxos)` at the pinned rev, and its doc is explicit that discovering the
outputs is the caller's job. This wallet already discovers them:
`convert::token_inputs` walks the spendable set, decodes each script and keeps
reserve outputs carrying exactly the wanted currency. The work is a currency on
`SendDraft` and `SendState`, a picker on the form (the Convert screen's overlay
is the component to reuse — it already writes an i-address rather than a name,
for the recorded reason that `Bridge.vETH` and `Bridge.CHIPS` share a name
component), routing in `prepare`, and a currency-aware `validate`. That last one
is the part that ships wrong quietly: `amount-above-spendable` currently quotes
the native figure, which becomes a false sentence the moment a currency exists.

**Gated on evidence, not on effort.** The selector it would be built on is the
one §9 records as never having run — no reserve utxos on the scripted chain, no
conversions on VRSCTEST while `disabledefi` holds. So the first move is the
`with_reserve_utxo` fixture §9 already asks for, through `convert_build.rs`, and
then a real token conversion on a chain that allows one. Only then is there
reason to trust the same input selection with a payment. Steps one and two are
§9's outstanding item as well; the two entries share their first move.

**A currency picker has to decide what "send everything" means.** `SendDraft`
now has five fields, and the fifth is `send_all` — documented as native-coin
only, because that is the only thing it can mean while there is one currency.
The moment a currency sits beside it the flag becomes ambiguous, and not
harmlessly: "all of the token" and "all of the coin" are different transactions
that **cannot both be satisfied**, because a token transfer is paid for in
native coin. A key swept of its coin cannot then move its token, and a key swept
of its token still shows a coin balance. Whoever adds the picker owns that
sentence on the form as much as the routing in `prepare` —
`send::resolve_send_all` itself would also need rewriting, since a token
transfer uses CryptoCondition outputs and its fee ladder is the 200-byte one.

---

## 11. Sending everything on any route but `R → R`

**Status:** refused, in so many words, rather than ignored — and in two different
sets of words, because there are two different reasons.

`send_all` rides on `SendDraft`, and `from_pool` is on the same struct, so the
flag reaches the shielded routes whatever the form offers. `Core::prepare_send`
turns it away there with `send-all-transparent-only`, and `send::validate`
refuses it on the same code so the button is disabled rather than pressed into a
refusal. The interface does not draw the control while the shielded balance is
paying — the same "omit, do not grey" convention the pool selector follows.

**And `R → z`, which is the reachable one.** The toggle *is* drawn whenever the
transparent balance is paying, so pasting a `zs1…` into a form already set to
send everything takes one keystroke. Both sides refuse it — `send_all_refusal`
in `pecu-core/src/lib.rs` and `send::validate` — under a second code,
`send-all-shielded-recipient`, because the two refusals are about different
halves of the payment: here the coins genuinely are the transparent ones and it
is the destination that cannot be served. Shielding runs through
`crate::shield` with a fee of its own, so `resolve_send_all` — which prices a
plain transparent payment — does not describe it. Implementing it means a
second resolver against the shield builder's fee, not a relaxed guard.

The form branched on `draft.from_pool` and the core on `route` for one commit,
and `R → z` is exactly where those disagree: the form said ready and the core
then refused, with a sentence telling somebody to pay from a balance that was
not the one paying. `the_two_send_all_refusals_do_not_share_a_sentence` and
`shielding_everything_and_sweeping_a_shielded_balance_are_told_apart` hold the
two halves shut.

**The arithmetic would be the easy half.** `min_relay_fee` counts outputs, never
bytes (`verus-flows/src/shielded/spending.rs`), so for two shielded outputs and
at most one transparent one it is a flat 10 000 satoshis whatever the input
notes number. The fixpoint `send::resolve_send_all` exists for does not arise;
the answer really is "the notes, minus the fee".

**The hard half is a product question.** `MAX_SPEND_NOTES = 10`, and
`Shielded::plan_spend` enforces it. A shielded balance spread over more than ten
notes cannot all move in one transaction at any fee, and each note is a Groth16
spend proof — tens of seconds of CPU. So "send everything" from the shielded
pool either means "what ten notes reach and no more", or it means a refusal that
explains the ceiling, and that decision does not belong inside a fee
calculation. `ShieldedError::NotEnough` already carries `reachable` — the sum of
the ten largest notes — which is the figure either answer would quote.

Testing it needs the Sapling proving parameters, which is why every existing
shielded build test is `#[ignore]`. The transparent case tests offline in
milliseconds; bundling them would have put a fee fixpoint and a fifty-megabyte
download in one commit.

---

## 12. "Left afterwards" is computed against the transparent balance, always

`Core::finish_prepare` passes `self.active_key_funds().spendable` — the
**transparent** figure — as `send::review`'s `spendable`, for every route. So
`balance_after_display` on a shielded payment subtracts a shielded amount from a
transparent balance and prints the result as if it meant something. Pre-existing,
and easy to miss while the review's other figures are all decoded from the
signed bytes.

The *key* half of this is fixed: that call used to pass `self.spendable`, the
sum over every address in the wallet, so "Left afterwards" on a two-key wallet
reported the other key's money as what this payment left behind. Send-everything
made that conspicuous — "Left afterwards" is the one number somebody emptying a
key reads closely — and on the transparent route it now reads what the swept key
actually still holds, which for a key with no sub-marginal coins is
`0.0000 0000`.

The *pool* half is not. The fix is to branch on `prepared.route` at the call
site and hand `review` the balance of the pool that is actually paying. Small,
and deliberately still separate — it changes a figure on three screens that the
send-all work does not otherwise touch.

---

## 13. A sweep says that coins can stay behind, never how many

**Status:** warned in general, unquantified in particular.

`send::resolve_send_all` excludes coins that cost more to move than they are
worth — past the fee floor an extra input costs about 1 800 satoshis, so a
500-satoshi coin makes the recipient worse off. That is the right economic
answer, and it means a payment the form called "everything" can leave a key
holding something.

The form now says so: *"A coin worth less than it costs to move stays where it
is."* That sentence had to exist, because without it "everything this key can
spend goes, less the network fee" is simply false whenever a key holds dust.

**What is still missing is the figure.** The review's "Left afterwards" line is
the key's balance less what the transaction takes, so on a sweep it *is* the
dust — but it is a bare number with no sentence attached, and somebody
decommissioning the machine that holds the key reads `0.0000 1500` as a rounding
artefact rather than as coins they still own.

Naming it properly needs a fact the review does not carry: whether this payment
was a sweep at all. The figure does not exist until coins have been selected,
which is inside the builder — the same reason there is no amount field in this
mode — so the sentence belongs on the review rather than beside the toggle, and
`SendReviewVm` is decoded from the signed bytes and deliberately knows nothing
about the draft. Either it gains a flag, or `Prepared` carries one for it. That
is a small change to a struct three screens read, which is why it is written
down here rather than done in passing.

`sending_everything_leaves_behind_a_coin_that_costs_more_to_spend_than_it_is_worth`
in `pecu-core/tests/send_build.rs` is the case that produces it: 1 500 satoshis
left in the key, deliberately.

---

## 14. A second shipped endpoint per chain — and the code that has to ship with it

**Status:** the check is built, and **nothing runs it at all.**

This used to read "on a default install nothing runs it". #44 closed the gap the
other way. `pecu_chain::corroborate` fired only when the *active* node was one
the user added, and user-added endpoints are no longer a feature: `Command::AddNode`,
the `add-node` callback, `NodeManager::add`, `NodeManager::second_source` and
every reader of the store's `node` table are gone. `pecu_core::build_on_worker`
hands `send::prepare` `None` for its second source on every route, on every
chain, always.

So the honest one-line summary of this wallet today is that **every transparent
payment and every shield it signs is built on one node's word**, exactly as a
default install always was — the asymmetry #44 describes is resolved by making
everybody the uncorroborated case rather than by making anybody the checked one.

`pecu_chain::corroborate` holds the outputs a transparent payment or a `t→z`
shield would spend against a second node before anything is signed. It is real
and it is tested — by the scripted readers at the foot of that module and by
`pecu-core/tests/send_corroboration.rs`, which drives `send::prepare` with a
second source that no caller in the application can now supply. Those tests are
the only thing keeping it from rotting, and they are why it was kept rather than
deleted: this entry is the plan to re-attach it, and re-deriving the comparison
rule later would be worse than maintaining it unused.

`Network::builtin_nodes` returns exactly one endpoint for each of the five
shipped chains — `every_shipped_chain_is_complete_and_distinct` asserts it — so
there is no independent endpoint to hold anything to. Requiring corroboration
unconditionally today would refuse every spend on every chain.

**What this entry is really about is that it takes two changes, not one.**
Finding a second, independently operated endpoint per real-money chain and
paying for it is an operator decision. Turning it into a check is not, and it is
easy to record this as purely an operator problem and then ship a second URL
that changes nothing:

```rust
// NodeManager::second_source
if active.builtin {
    return SecondSource::Unheld;
}
```

That early return was the default install's whole exemption, and since #44 there
is no `second_source` left for it to live in — which does not make the trap go
away, it moves it. Whoever writes the function again has to write it around
*provenance* rather than around the active node's `builtin` flag, or a second
shipped endpoint will land and change nothing: the active node will be a
built-in, nothing will be held against anything, and the second URL will sit in
the list being a failover target. The two still have to land together.

Three other things have to come back with it, and they are easy to miss now that
none of them exist:

- **A decision before the build.** `corroborating_source` decided, from the node
  list, which endpoint a send had to be held against, and which routes had to
  ask — transparent payments and `t→z` shields, on what a transaction *spends*
  and not on the route's name.
- **The same decision before the broadcast.** `corroboration_missing` re-read it
  at the confirm gate, so bytes prepared under one node list could not be sent
  under another. The two have to be the same decision or they drift the first
  time a node's status changes.
- **Two refusals and their sentences.** `SpendRefused::NoSecondSource` and
  `SpendRefused::PreparedBeforeNodeChange` were removed with the code that
  raised them, along with their `node-*` and `spend-*` lines in
  `ui/components/note.slint`. The four that remain — `Uncorroborated`,
  `SecondSourceBehind`, `SecondSourceAhead`, `SecondSourceSilent` — are still
  produced by `send::prepare` and still have words.

And **#45** is the defect to fix *before* re-attaching any of it, not after: rule
3 in `corroborate::against` excuses any missing coin whenever the secondary is
ahead, with no bound, so the mirror of #29 renders as "nothing is wrong". It
reaches no user today because nothing reaches the rule. It will reach the first
user of whatever runs it next.

**What it costs, so the trade is on the table before somebody starts.** Every
transparent send and every shield gains a round trip to a server the user did
not choose, and that server is told which address is paying, once per payment —
the sentence on the network screen already says this for the added-node case and
would then be true for everyone. Corroboration fails closed, so a build that
required it everywhere would refuse to spend for as long as the second endpoint
was unreachable; the trap that already exists for users on their own node — see
the limits list on `pecu_chain::network::Network` — would become the trap for
everybody. Two shipped endpoints agreeing is also not proof: they defeat *one*
endpoint being wrong, and share an operator's mistakes if they share an
operator.

**First move.** Two endpoints for VRSC and VRSCTEST, run by different people,
recorded in `builtin_nodes` with a note on where each came from. Then rewrite
`second_source` around provenance rather than around the active node's
`builtin` flag, and decide what a build does when the second one is down —
because "refuse" and "spend uncorroborated" are the only two answers, and the
first one is the reason this is not just a configuration change.
