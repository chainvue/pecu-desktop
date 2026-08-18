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
  Glyphs, the product name, an example URL, a font licence, and `mainnet` —
  which the *core* compares the typed confirmation against, so a translated one
  would ask for a word the wallet then refuses.

## What is still not covered

`@tr` reaches `.slint` and nothing else, so a sentence built in Rust is one no
catalogue can touch. There were about a hundred and seven of them. There are
now **forty-one**, and all but a handful are in two files:

| file | left | what they feed |
|---|---|---|
| `pecu-core/src/currency.rs` | 17 | the currency draft validator's refusals and notes |
| `pecu-core/src/identity.rs` | 16 | the identity lookup and authority verdicts |
| `pecu-core/src/lib.rs` | 7 | `RegistrationVm.note`, the name check, the currency picker's problem |

Everything else goes through a **named reason** now. The core says what is
wrong and supplies the values; `components/note.slint` has the words, in three
chains: `NoteText` for a refusal beside a field, `NoticeTitle` for the headline
on a toast, `NoticeBody` for the line under it.

An unknown code renders as itself, which is ugly on purpose — a code with no
sentence is a bug, and it should be visible the first time it is drawn rather
than the first time somebody reads a screenshot carefully. That is not
theoretical: it caught a sentence filed in the wrong chain during this very
change, in the first render after the mistake.

## The German catalogue here is a fixture

`de/` has the eight navigation labels and nothing else. It exists so
`tests/translation.rs` has something real to switch to, and so the untranslated
fallback can be checked. It is not an offer of German — shipping a language
means a translator, not a developer with a dictionary.
