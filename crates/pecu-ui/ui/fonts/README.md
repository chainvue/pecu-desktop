# The two faces this interface is drawn in

Both are bundled rather than assumed, and both are compiled into the binary by
`import "…ttf"` in `ui/theme/theme.slint`.

## Why they are here at all

A face named but not shipped is a face that exists on the machine the design was
drawn on and nowhere else. The interface then renders in whatever the system
happens to offer — a fork in the design that is invisible from the developer's
own screen, which is exactly the kind that survives to a release.

## What is here

| File | Family | Weight | Used for |
|---|---|---|---|
| `SpaceGrotesk-Regular.ttf` | Space Grotesk | 400 | body text |
| `SpaceGrotesk-Medium.ttf` | Space Grotesk | 500 | labels, active nav, emphasis |
| `SpaceGrotesk-Bold.ttf` | Space Grotesk | 700 | headings, buttons |
| `JetBrainsMonoNL-Regular.ttf` | JetBrains Mono NL | 400 | addresses, txids, heights |
| `JetBrainsMonoNL-Medium.ttf` | JetBrains Mono NL | 500 | the same, emphasised |
| `JetBrainsMonoNL-Bold.ttf` | JetBrains Mono NL | 700 | amounts, the wordmark |

Roughly 980 KB in total.

Static instances rather than the variable fonts. Slint's font matching picks a
family and a weight, and `fontdb` reads the **typographic** family name (name ID
16) in preference to the basic one (ID 1) — which matters here, because
`SpaceGrotesk-Medium.ttf` calls itself "Space Grotesk Medium" in ID 1 and "Space
Grotesk" / "Medium" in IDs 16 and 17. Without that preference `font-weight: 500`
would find nothing and fall back to Regular. It was checked in the fontdb source
rather than assumed.

There is **no 600**. The design asks for 400/500/700, Space Grotesk ships no
SemiBold cut, and a request for 600 is silently resolved to a neighbour by the
matcher. `Typo` therefore offers `w-regular`, `w-medium` and `w-bold` and no
name that would invite one.

Italics are deliberately absent. Nothing in this interface is italic, and a face
nobody asks for is a megabyte in every copy of the binary.

## Why the "NL" cut of JetBrains Mono

`NL` is the no-ligature cut, and taking it is a correctness decision rather than
a stylistic one.

The default cut carries the code ligatures the typeface is known for. They live
in a `calt` feature — contextual alternates — with a 25 KB `GSUB` table, and
`calt` is enabled by default in the shaper, so they are not something the
application opts into. `JetBrainsMono NL` has no `GSUB` table at all. Both were
read out of the files rather than taken from the documentation:

```
JetBrainsMono-Regular.ttf     GSUB 25108 bytes   features: calt, ccmp, ss01…
JetBrainsMonoNL-Regular.ttf   GSUB     0 bytes   features: none
```

This interface prints node URLs, receive addresses, transaction ids and amounts
in mono and asks people to check them character by character. `//` and `://` are
among the sequences the default cut merges into a single glyph. A wallet that
draws two characters as one, in the field somebody is verifying, has made the
single mistake that bundling a monospace face was meant to prevent.

Nothing else is lost. The ligatures only fire on runs of symbols, so digits,
base58 and hex are the same drawing in either cut.

## Why a monospace family for money at all

Slint exposes no OpenType feature control, so `tnum` — tabular figures — cannot
be requested on a proportional face. Without it a balance visibly jitters as its
digits change width, and amounts do not line up in a column. A monospace family
solves both structurally rather than by asking for a feature that cannot be
asked for.

## Licensing

Both are **SIL Open Font License 1.1**, and both licences are in this directory:

- `JetBrainsMono-LICENSE.txt` — JetBrains Mono 2.304, © 2020 The JetBrains Mono
  Project Authors, from
  <https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304>
- `SpaceGrotesk-LICENSE.txt` — Space Grotesk 2.0.0, © 2020 Florian Karsten,
  from <https://github.com/floriankarsten/space-grotesk/releases/tag/2.0.0>

The OFL permits bundling and redistribution, including inside a binary, and
requires the licence and copyright notice to travel with the font. Because these
are compiled *into* the executable, that obligation follows the executable — so
Settings → About names both faces and their licence. Removing that line does not
make the obligation go away.

Neither font is modified. If one ever is, note that the OFL's Reserved Font Name
clause means the result may not be called JetBrains Mono or Space Grotesk.
