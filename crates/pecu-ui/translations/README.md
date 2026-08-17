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

## What is not covered

`@tr` reaches `.slint` and nothing else. Around 140 user-visible sentences are
built in Rust — refusals and explanations from `pecu-core`, formatted values
from `pecu-protocol` — and they arrive at the interface as strings that are
already written. Slint cannot translate those, and neither can this directory.

That is a design question rather than a missing feature: the core should hand
the interface a *reason* it can render, instead of a sentence it can only
display. It is not answered yet, and no amount of `.po` files will answer it.

## The German catalogue here is a fixture

`de/` has the eight navigation labels and nothing else. It exists so
`tests/translation.rs` has something real to switch to, and so the untranslated
fallback can be checked. It is not an offer of German — shipping a language
means a translator, not a developer with a dictionary.
