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
  indexer, and the dashboard tile that shows it has no source.

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
