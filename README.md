# Pecu

A self-custodial desktop wallet for Verus — Windows, macOS and Linux. Rust and
Slint, one binary, no runtime to install.

Forked from `chainvue-desktop-wallet`, and currently being redrawn against the
design package in `docs/design/`.

## Running it

```sh
cargo run -p pecu-app --features mock   # a scripted chain, no node, no coins
cargo run -p pecu-app                   # a real node, testnet by default
```

Start with `mock`. It runs against a scripted chain in its own directory, so
fixture figures can never end up in the files a real wallet reads back — and it
is structurally unable to reach a network, because `pecu-mock` does not have a
socket-capable crate in its dependency graph.

Only **one instance per wallet directory**. A second one says so and stops:
three of the files it would share are written whole, and the last writer wins —
including for a name registration that has already been paid for. To run two,
give the second its own home:

```sh
PECU_HOME=/tmp/pecu-second cargo run -p pecu-app --features mock
```

Wallet data lives in `%APPDATA%\Pecu` on Windows, `$XDG_DATA_HOME/pecu` on
Linux, and `~/Library/Application Support/com.pecu.wallet` on macOS. Logs are
under `logs/` inside it, rotated daily.

The interface is English, on every system. See `crates/pecu-ui/translations/`.

## Checks

```sh
scripts/check.sh          # all three, which is what you want before pushing
scripts/check.sh clippy   # warnings are not acceptable output
scripts/check.sh test
scripts/check.sh deny     # the dependency graph
```

`scripts/check.sh` is the one definition of "the checks". The workflow in
`.github/workflows/checks.yml` calls that same script rather than repeating the
commands, so what runs on a pull request and what runs at your desk cannot
drift into disagreeing. It needs no display — the interface tests render
offscreen — which is what lets a headless runner run them at all, and a failed
visual test uploads its diff images as a build artefact, because the log names
the screens that changed and only the images say what they now look like.

The third one is `cargo deny`, and it compiles nothing: it reads `Cargo.lock`
and the RustSec advisory database, and fails on a known-vulnerable or abandoned
crate, a licence this project may not ship, a source nobody chose, or a crate
that touches key material arriving at two versions. The policy is `deny.toml`
at the root — every tolerated advisory carries a line saying why it is tolerated
and what would end that, which is the part a bare ignore list never records. It
wants `cargo-deny` 0.20 or newer (`cargo install --locked cargo-deny`); the
other two halves run without it.

Some of the tests are unusual and are the point of the project rather than a
formality:

| Test | What it refuses to let happen |
|---|---|
| `pecu-ui/tests/visual.rs` | A layout change nobody looked at. Renders 75 screen states in both themes — 150 images — and compares them against checked-in references. |
| `pecu-ui/tests/accessibility.rs` | A control a screen reader announces as "button" and nothing else. |
| `pecu-ui/tests/dependency_boundary.rs` | The interface crate gaining the ability to name a `PrivateKey`. |
| `pecu-ui/tests/translation.rs` | "It is ready for translation" being false. |
| `pecu-core/tests/log_hygiene.rs` | A secret reaching a log file. |

After a deliberate visual change, look at the result before blessing it:

```sh
cargo run -p pecu-ui --example render_shots            # writes docs/shots/
UPDATE_SNAPSHOTS=1 cargo test -p pecu-ui --test visual # accepts them as references
```

Reference images are whole-tree artefacts: a commit that changes the palette
changes all of them, so source and images travel together or the tip is red.

There is **one set for every platform**. macOS and Linux do not draw this
identically — when it was measured, at 132 images, 116 of them differed — but
the entire disagreement was 872 pixels off by 1 of 255, in the navigation rail's
icon column, and nothing moved. So the comparison forgives a per-channel delta
of 1 and caps how many such pixels an image may carry, rather than keeping a set
per operating system: a change reviewed on one machine would otherwise land red
on every other, and the only answer to that is a blanket re-record, which
verifies nothing.

The tolerance does not blunt the test. The change in `3cfbd84` moved 2,096,525
pixels, 99.65% of them by more than 1, and not one of its 120 images would have
been absorbed. `crates/pecu-ui/tests/visual.rs` carries the numbers and the
argument, including what the budget is there to catch.

## Packaging

`render_icon` writes all three platforms' icons from `ui/icon.slint` — the macOS
iconset, the Linux hicolor theme and the Windows `.ico` — so run it after
changing the palette or the mark. The results are checked in.

```sh
cargo run -p pecu-ui --example render_icon
```

### macOS

```sh
scripts/bundle.sh                            # target/Pecu.app
```

Unsigned, so it runs on the machine that built it and Gatekeeper blocks it
everywhere else — correct behaviour, not a bug. Signing needs a Developer ID;
notarisation needs a round trip to Apple, and `bundle.sh` prints those commands
rather than running them.

The Dock icon comes from the **bundle**. `cargo run` produces a bare binary and
macOS gives one of those a generic icon whatever it contains.

### Linux

A different shape, on purpose: macOS wants one directory that is the
application, and Linux wants files where the desktop already looks.

```sh
sudo apt install build-essential pkg-config libfontconfig-dev
scripts/install-linux.sh                     # into ~/.local
scripts/install-linux.sh --prefix /usr/local # system-wide
scripts/install-linux.sh --uninstall
```

`build-essential` is for SQLite, which `rusqlite` compiles from source rather
than linking — so there is no `libsqlite3-dev` in that list and no version of
SQLite to disagree with. `libfontconfig-dev` is the only other thing the build
asks pkg-config for.

The X11, Wayland, xkbcommon and GL libraries are **dlopened** by winit and
glutin rather than linked, so a desktop already has them and this does not name
them. A container does not, and there the runtime list is:

```
libx11-6 libxcursor1 libxrandr2 libxi6 libxkbcommon0 libwayland-client0 libgl1 libegl1
```

Running it again replaces everything it wrote — the binary, all eight icons,
the `.desktop` entry — so a re-install is how you move to a newer build. The
binary is renamed into place rather than written over, because writing over one
that is **currently running** fails with `ETXTBSY`; a rename leaves the running
process on its old inode until it is closed.

**It does not touch the wallet.** The vault, the databases and the logs live in
`$XDG_DATA_HOME/pecu` — `~/.local/share/pecu` — which is a sibling of the
`icons/` and `applications/` directories this writes, and neither installing nor
`--uninstall` opens it.

Not a package. There is no `.deb`, no AppImage and no Flatpak, so there is
nothing to hand somebody else — this installs onto the machine that ran it.

### Windows

The icon exists — `crates/pecu-app/assets/pecu.ico`, seven sizes — and nothing
links it into the executable. That needs a build script and a resource
compiler, which is a dependency decision rather than a line of code.

### What has actually been run

macOS only. `scripts/install-linux.sh` was written against the freedesktop
specifications and the files this repository generates, and **has never been run
on Linux**; nothing has been built on Windows at all. `crates/pecu-app/tests/
packaging.rs` checks what can be checked from anywhere — that the files exist,
that the `.ico` container parses back to the sizes that went in, and that
`Icon=pecu` matches the filenames in the icon theme.

## Layout

| Crate | |
|---|---|
| `pecu-app` | The binary. Wires callbacks to commands and owns the window. |
| `pecu-ui` | The Slint interface. Holds no keys, and **cannot name** the types that carry them. |
| `pecu-protocol` | The contract between the two. No SDK, no crypto, no I/O. |
| `pecu-core` | The wallet actor: owns all state, does all I/O, free of any UI toolkit. |
| `pecu-keystore` | Private keys on disk, encrypted. The only crate that may hold a `PrivateKey`. |
| `pecu-chain` | The node client, node health, and the permit that gates spending. |
| `pecu-store` | What survives a restart: settings, and a cache it can always throw away. |
| `pecu-chart` | Chart geometry. Pure arithmetic, no dependencies. |
| `pecu-mock` | A scripted chain, unable to reach a network. |

The boundary in the third row is enforced, not encouraged: `pecu-ui` does not
depend on the SDK, the keystore or the core, so Rust will not resolve those
types inside it. `dependency_boundary.rs` fails the build if that list grows.

## Reading

- `docs/design/REVIEW.md` — what was measured in the design package before
  anything was built from it, including four contrast failures and two icons
  that cannot ship as delivered.
- `docs/LATER.md` — work that is understood, deliberately not started, and would
  otherwise be rediscovered from scratch.
- `crates/pecu-ui/ui/fonts/README.md` — why the monospace is the no-ligature cut.
- `crates/pecu-ui/translations/README.md` — how to add a language, and what
  `@tr` cannot reach.

The SDK is pinned by revision (`8f01520`) rather than by version: it is not on
crates.io, and "latest main" is not a reproducible dependency for the crate that
owns every key, address and transaction here.
