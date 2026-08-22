# Translations

English is the original. Every other language is a `.po` file here, compiled
into the binary at build time.

```
translations/<lang>/LC_MESSAGES/pecu-ui.po
```

`<lang>` is the locale directory name — `de`, `fr`, `pt-BR`. `pecu-ui` is the
crate name and `slint-build` derives it from `CARGO_PKG_NAME`; renaming the
crate renames these files.

## How it is wired

- `ui/*.slint` marks translatable text with `@tr("…")`.
- `build.rs` calls `with_bundled_translations("translations")`, which compiles
  every catalogue here **into the executable**.
- `slint::select_bundled_translation("de")` switches at runtime. It must be
  called *after* the first component is constructed — the bundle attaches to the
  context that one creates.
- `tests/translation.rs` proves the whole chain, in German, against the live
  element tree. If any link breaks it goes red.

There is no runtime file loading and no `gettext` C dependency. Adding a
language is a rebuild; for something that ships as a signed bundle it was going
to be one anyway.

## Two things that will bite

**Slint picks a language from the system locale.** When the first component is
built, it looks at the host locale and selects a matching bundle if one exists.
So merely *having* `de/` here changes what a German machine shows. Both the
application (`pecu-app/src/main.rs`) and the snapshot renderer
(`src/snapshot.rs`) therefore select `"en"` explicitly:

- the application, because a half-finished catalogue would give somebody a
  German rail and an English everything else, which is worse than one language;
- the renderer, because a reference image that depends on who rendered it is not
  a reference.

Both of those lines look redundant and are not. Removing either produces a
failure that only appears on machines whose locale is not English.

**The default translation context is off.** Slint otherwise uses the enclosing
component's name as the gettext `msgctxt`, which means moving a `Text` from one
component to another silently drops its translation. `build.rs` sets
`DefaultTranslationContext::None`, so extraction must match:

```sh
slint-tr-extractor --no-default-translation-context \
    -o translations/pecu-ui.pot $(find ui -name '*.slint')
```

`slint-tr-extractor` is not vendored — `cargo install slint-tr-extractor` when
you need it. Merge into an existing catalogue with `msgmerge`, which keeps the
translations that are still current and marks the rest fuzzy:

```sh
msgmerge --update translations/de/LC_MESSAGES/pecu-ui.po translations/pecu-ui.pot
```

## Nothing in `.slint` escapes any more, and a test says so

`tests/translatable.rs` reads every `.slint` source and fails on a literal in a
user-visible property that is not inside a `@tr(…)`. It exists because the
failure is silent in every other direction: a string that missed `@tr` renders
perfectly, reviews perfectly and photographs identically to one that did — the
only difference is that no catalogue can ever reach it. By the time anybody
counted there were around three hundred and forty of them, four screens with not
one `@tr` in the file.

Two kinds of literal are exempt and both are recognised rather than tolerated:

- **Anything being compared.** `kind == "login" ? @tr("Signed in") : …` has two
  strings and one message. Translating the other would not produce a bad
  sentence, it would take the wrong branch — quietly. The scan drops every
  literal that follows a `==` or `!=`.
- **The listed exceptions**, in `ALLOWED`, each with the reason beside it.
  Glyphs, the product name, an example URL, a font licence, and a BIP-39 example
  phrase — whose wordlist is English by specification, so a translated hint would
  show input the field then refuses.

  The word somebody types to arm spending is not on that list and no longer
  could be: it is the chain's own name, bound from `NetworkState.requested-name`,
  so there is no literal in the interface for the scan to see.

## The core builds no sentences any more

There were a hundred and seven when this started, then forty-one, and now
**five** — `Active`, `Locked`, `Unlocking`, `Revoked` and `Basket`. All five are
**vocabulary the code branches on**: `status == Status::Revoked.label()` is a
comparison in Rust, and translating the value would take the wrong branch
rather than produce a bad sentence. They are said in the interface with a
ternary, the same way a node's status is.

Everything else travels as a **named reason** — a `NoteVm` of a code and its
values — and the words live in `components/note.slint`, in three chains:

| component | what it spells |
|---|---|
| `NoteText` | a refusal beside a field, a figure's label, a relative time |
| `NoticeTitle` | the headline on a toast, and the same reason inline on a form |
| `NoticeBody` | the line under it, where there is one |

Two hundred and thirty-four sentences, and 815 `@tr` calls across the interface.

### What that bought, beyond the translation

- **Dates.** `portfolio` used to build "12 March" and "2 hours ago" in Rust,
  with twelve English month names and a hand-rolled `== 1` plural. It now sends
  a day, a month **index** and a count; `note.slint` names the month and lets
  `@tr`'s plural form choose. A language that writes "March 12", or has three
  plural cases, can now say so.
- **A dead-code check.** `tests/note_coverage.rs` asserts both directions: every
  reason the wallet names has words, and no words are written for a reason
  nothing names. The second half found two sentences whose emitters had been
  removed an hour earlier.

### Two tests are what keep it

`tests/translatable.rs` reads the `.slint` sources and fails on a literal in a
user-visible property that is not inside a `@tr(…)`. `tests/note_coverage.rs`
reads `pecu-core`'s sources and fails on a code with no sentence. Neither
depends on `pecu-core` as a crate — they read files, which is what a lint over a
repository does.

Both have caught real mistakes during the work that introduced them, which is
the only evidence a guard is worth having.
