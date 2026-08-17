# The two faces this interface is drawn in

Both are bundled rather than assumed, and both are compiled into the binary by
`import "…ttf"` in `ui/theme/theme.slint`.

## Why they are here at all

Before this, the interface named **Menlo** for every amount, address, txid and
block height. Menlo ships with macOS and exists nowhere else, so on Linux and
Windows every one of those rendered in whatever monospace face the system
happened to offer. That is a fork in the design that is invisible from a Mac,
which is exactly the kind that survives to a release.

The UI face was the platform default — SF Pro here, Segoe there, something else
again on Linux — for the same reason and with the same consequence.

## What is here

| File | Family | Weight | Used for |
|---|---|---|---|
| `Inter-Regular.ttf` | Inter | 400 | body text |
| `Inter-Medium.ttf` | Inter | 500 | labels, active nav, emphasis |
| `Inter-SemiBold.ttf` | Inter | 600 | headings |
| `IBMPlexMono-Regular.ttf` | IBM Plex Mono | 400 | money, addresses, txids, heights |
| `IBMPlexMono-Medium.ttf` | IBM Plex Mono | 500 | the same, emphasised |

Static instances rather than the variable fonts: Slint's font matching picks a
family and a weight, and the static files carry the typographic family name that
makes `font-weight: 500` resolve to Medium. Five files at roughly 1.5 MB total,
which is the price of the interface looking the same everywhere.

Italics are deliberately absent. Nothing in this interface is italic, and a face
nobody asks for is a megabyte in every copy of the binary.

## Why a monospace family for money at all

Slint exposes no OpenType feature control, so `tnum` — tabular figures — cannot
be requested on a proportional face. Without it a balance visibly jitters as its
digits change width, and amounts do not line up in a column. A monospace family
solves both structurally rather than by asking for a feature that cannot be
asked for.

## Licensing

Both are **SIL Open Font License 1.1**, and both licences are in this directory:

- `Inter-LICENSE.txt` — Inter 4.1, © 2016 The Inter Project Authors,
  from <https://github.com/rsms/inter/releases/tag/v4.1>
- `IBMPlexMono-LICENSE.txt` — IBM Plex Mono 2.3, © 2017 IBM Corp,
  reserved font name "Plex", from `google/fonts`

The OFL permits bundling and redistribution, including inside a binary, and
requires the licence and copyright notice to travel with the font. Because these
are compiled *into* the executable, that obligation follows the executable — so
Settings → About names both faces and their licence. Removing that line does not
make the obligation go away.

Neither font is modified. If one ever is, note that the OFL's Reserved Font Name
clause means the result may not be called Inter or Plex.
