# What was found in this package before anything was built from it

`README.md`, `pecu-tokens.json`, `screens/`, `design-kit/`, `handoff/` and
`icons/` are the designer's delivery, copied in unchanged. This file is the
review of them: what was measured, what is wrong, and what the decisions were.
It exists so none of it is rediscovered by finding it in the rendered UI.

The package is a **design direction for five screens**, not a desktop
specification. It covers Dashboard, Send, Markets + currency detail, Messenger
and History. This wallet has around twenty-four screens in ninety-six reference
images. Not covered by any drawing: Settings, Network, the whole currency-launch
flow, Receive, Onboarding, the seed backup, identity detail, authorities, revoke,
and the transaction sheet. Those are drawn in the new language rather than
copied from it.

The handoff is written for a phone — Face ID, safe areas, slide-to-send — and
scaled onto a 1060×640 window. Where it contradicts the desktop, the desktop
wins, and the places that happens are below.

---

## The light palette does not meet the contrast floor this project keeps

Computed from `pecu-tokens.json` against each theme's own base, WCAG 2.1
relative luminance:

```
dark  textSub   #8B95A3 on #0B0D10   6.42:1   ok
dark  accent    #12D6B4 on #0B0D10  10.47:1   ok
dark  negative  #FF5C7A on #0B0D10   6.54:1   ok
dark  warning   #FDBE3B on #0B0D10  11.68:1   ok
dark  onAccent  #062A24 on #12D6B4   8.28:1   ok

light textSub   #5B6470 on #F4F6F8   5.54:1   ok
light accent    #0FA38A on #F4F6F8   2.93:1   FAILS
light onAccent  #FFFFFF on #0FA38A   3.17:1   FAILS  ← primary button label
light negative  #E5484D on #F4F6F8   3.61:1   FAILS
light warning   #B98900 on #F4F6F8   2.92:1   FAILS
```

The dark set is clean throughout and is taken as delivered. The light set has
four breaks, and the worst of them is the label on the primary button — the
control that sends money.

This is the same failure the previous theme had and the same fix: darken each
until it clears 4.5:1 against its own base, and write the measurement next to
the value so the next person can check it rather than trust it. The hues are
kept; only the lightness moves. `-soft` fills are backgrounds, not text, and
keep their values.

## The type scale is a phone scale

The tokens ask for body 12.5px, label 10px, section header 9px, list subtitle
9.5px, seed index 8.5px. At 1060px wide on a display without HiDPI — which is
most Windows and Linux machines — 9px is not read, it is guessed at.

Used instead, and the reasoning is that premium on a desktop comes from
whitespace and stillness rather than from small text:

```
default window   1240 × 800     min window   960 × 600
body             14px           caption / micro   12 / 11px
section header   11px upper     hero balance      40px mono
```

The proportions of the design survive this; the absolute sizes do not.

## The tokens contradict themselves on hit targets

`components.button.minHitTarget` is 44. `components.listRow.paddingY` is 9,
which builds a row around 30px, and the mockups' nav items sit at 8px padding
for about 32px. 44 is kept for anything that can be pressed; the list row is a
row.

## `icons/master.svg` renders the wrong glyph off this machine

The `@` is a `<text>` element with `font-family="JetBrains Mono, monospace"`,
not a path. As the scalable source for the Linux hicolor theme it renders in
whatever the system falls back to, on every machine that does not have that font
installed — which is the normal case for a freshly installed application.

The glyph has to be outlined before it is used as an icon anywhere.

## The macOS icon is the wrong shape

`icons/macos/icon-1024.png` is a full-bleed square with the ink background
filling it, and the README says macOS applies the squircle. It does not, for
classic `.icns` — the artwork is drawn as delivered, so this lands in the Dock
as a hard square beside every other icon that is not one. Apple's template is a
rounded rectangle inset within the canvas.

To be rebuilt, and in this repository the icon is rendered from `ui/icon.slint`
rather than being a bitmap that nothing keeps in step with the palette.

## The documents need the network to be read

`support.js` — identical in all three folders — fetches React from
`https://unpkg.com` at load, and the pages hide their own markup until it
arrives. The `<helmet>` blocks additionally pull Space Grotesk and JetBrains
Mono from `fonts.googleapis.com`. Opened without a connection, all three files
are blank.

The markup inside `<x-dc>` is static and inline-styled, so nothing about the
content requires this. Noted rather than fixed: these are the designer's files
and are kept byte-identical to what was delivered.

---

## Things the drawings show that the chain does not have

Checked against `api.verustest.net` and against the SDK, so that none of it is
found out during implementation:

- **USD prices work, on testnet, exactly as drawn.** `DAI.vETH` exists on
  VRSCTEST (`iN9vbHXexEh6GTZ45fRoJGKTQThfbgUwMh`), `Bridge.vETH` holds it
  together with VRSCTEST, and `estimateconversion` answers
  `1 VRSCTEST = 0.53694578 DAI.vETH` through it. The design's
  "Route (price discovery)" line is a real mechanism and needs no third party.

  A price is still `Option`: a currency with no converter route has none, and
  that is what the package's own "— means unknown, not zero" is for.

  Two sources, two purposes. `getcurrencystate` (`lastconversionprice`,
  `viaconversionprice`) is the mid price and is what a *display* shows.
  `estimateconversion` is net of `conversionfees` and `fees` and belongs on the
  Convert screen, where those fees are the point.

- **Convert is supported.** `verus-flows/src/convert.rs` has `plan_conversion`
  → `prepare_conversion` → `broadcast`, the same `Unsent` shape as the send
  path, so the review step, the spend permit and the pending ledger carry over
  rather than being rebuilt.

- **24h volume is not available.** No node method reports it. It needs an
  indexer, and the dashboard tile that shows it has no source. (A price
  *history* is available — see `LATER.md` §8 — but volume is not the same
  question and no RPC answers it.)

- **The asset list is fictional.** It shows BTC and ETH as held balances. There
  is no native BTC on Verus, bridged or otherwise.

- **Chat is the largest single item and it is blocked.** It needs the whole
  shielded stack, which is researched and unwritten, plus a messaging protocol
  over 512-byte memos that does not exist. Every message is a transaction with
  a fee.

- **Login has no implementation.** VDXF login consent, plus a QR and deep-link
  handler. Self-contained and independent of the rest.

## Decisions taken from this review

- **Native window decoration on all three platforms.** Slint 1.17 has
  `no-frame`, `resize-border-width` and `safe-area-insets`, and the winit
  backend resizes frameless windows itself — but it does not expose moving one,
  which needs `WinitWindowAccessor` and a direct dependency on the backend
  crate, and then Aero Snap, double-click-to-maximise and the window menu all
  have to be rebuilt by hand. Decisive against it: the snapshot renderer uses
  `MinimalSoftwareWindow`, which has no winit window, so a hand-drawn title bar
  would appear in every reference image and in no shipped window. That is the
  trap recorded in `LATER.md` under the native menu bar, and it is the same
  trap.

  `safe-area-insets` exists for the day content should run under the macOS
  traffic lights, and that stays available.

- **Every string is `@tr` from the first line.** Not the `gettext` feature —
  it pulls `gettext-rs` and a C dependency, which is a Windows problem.
  `slint-build`'s `with_bundled_translations()` compiles `.po` files into the
  binary and `slint::select_bundled_translation()` switches at runtime: no
  external files, no C, identical on all three platforms. English is the
  original. Retrofitting this across the interface later is the work nobody
  does twice.

---

## Still open: the 16px icon

The app icon is drawn from source (`ui/icon.slint`) rather than taken from the
delivered PNGs, which fixes two of the three problems recorded above: the glyph
comes from the bundled `JetBrainsMonoNL-Bold` instead of an SVG `<text>` element
that renders differently on every machine, and the shape is Apple's 824-in-1024
rounded square instead of a full-bleed one.

The third is not fixed. **At 16px the `@` cannot be drawn.** Rendered and
inspected pixel by pixel, the glyph is an amorphous blob — it has a counter
inside a counter and there are not enough pixels for either. Enlarging it to
fill the square makes a larger blob. What ships at that size is the mark reduced
to a ring, broken on the right with a tail, and at 16px even the break and the
tail disappear: it is a clean ring, legible, and not recognisably an `@`.

The package asks for exactly the thing that would solve this — "16/24px: nur @
ohne Cursor (Pixel-Hinting)" — and does not deliver it. Hand-hinted pixel art
for 16 and 24, drawn to the grid rather than scaled onto it, is a designer's
task. It matters for the Finder list, the window proxy icon and the ⌘-Tab strip.

---

## Where the built screens stand against the drawings

Added after a pass comparing each rendered screen against the drawing it came
from, so the next person asking "does this look like the design yet" reads an
answer rather than repeating the comparison.

The package covers five screens. Four of them exist in this build — Messenger
does not and cannot; see the Chat entry above.

**D1 · Dashboard — matches, and gained the one thing it was missing.** Total
balance with the day's change, the three actions, and the Assets / Markets /
Recent-activity columns are the drawing's layout. The balance chart is an
addition, standing where the drawing puts two tiles that no node can answer:
`24h volume` has no RPC source at all, and `Pooled (total)` is the same
question one currency at a time, which the markets screen already answers per
row as `Exit @2%`.

The gap that was real: **the search field**. Every drawing carries one across
the top and this build had ⌘K with nothing on screen to say so. Now in the
title bar, on every screen, naming what it actually searches.

Not copied, and why: the drawing's asset rows carry a fiat value and a change
percentage per asset. There is no fiat source in this wallet — the prices it
has are in whatever currency the markets book is quoted in — and the drawing's
own asset list is fictional (it shows native BTC and ETH balances, which do not
exist on Verus).

**D3 · Markets + detail — matches.** Name / Price / change / `Exit @2%`, the
detail beside it with its stats, its venues and its route, and the drawing's own
sentence — "— means unknown, not zero" — as the footnote. The change column is
`30d` rather than `24h` because sampling daily and labelling it 24h is a number
that looks precise and is not; see `LATER.md` §8.

**D5 · History + audit log — matches.** The four period tiles, the filter chips
(All · Payments · Converts · Logins · Identity), day headings, per-row icons and
the running confirmation count. The drawing's footnote promises a CSV export
that does not exist; that is a feature request rather than a difference in
appearance, and it is not in this build.

**D2 · Send — matches in structure, and three of its parts cannot be built
yet.** Contacts down the left (as "Paid before", from this wallet's own payment
history rather than an address book somebody has to curate), the form in the
middle, the summary on the right.

- **No `MAX` button**, and this is a deliberate refusal rather than an
  oversight. `MAX` means "spendable minus the fee", and the fee is not known
  until the transaction is built — so a button that filled in the spendable
  balance would produce a draft the builder rejects every time. Doing it
  honestly means estimating a fee for a transaction that does not exist yet.
  Worth doing; not worth shipping a button whose one job is to fail.
- **No `≈ $410.40`.** Same missing fiat source as the dashboard.
- **No encrypted note.** That is a shielded memo — `LATER.md` §2 — and every
  part of it is unwritten.

The drawing's "Recipient verified · has existed since 2025 · 34 shared
transactions" exists in two pieces: the review's first-time-recipient warning,
which is the half that matters, and the payment counts in the Paid-before list.

**Everything else in this wallet is drawn in the package's language rather than
from a drawing.** Settings, Network, Receive, Onboarding, the seed backup, the
transaction sheet, the whole conversion review, and — while they are out of the
rail — identities and the currency-launch flow. There is no drawing to hold
those against, which is worth saying plainly: "make every screen look like the
design" is not an executable instruction for nineteen of the twenty-four.
