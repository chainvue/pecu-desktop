//! Compiles the Slint interface.
//!
//! A `.slint` syntax error failing this build *is* the UI test for the early
//! phases — there is no cheaper way to find one, and no reason to let it reach
//! runtime.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Debug info, in unoptimised builds only.
    //
    // Slint drops the element tree's names, ids and source positions unless
    // asked to keep them, and `tests/accessibility.rs` walks that tree to read
    // back what each control exposes to a screen reader. Without this the walk
    // finds nothing — which the test does catch, but only because it refuses to
    // pass on an empty result.
    //
    // Off in release, where it is dead weight in the binary and nothing reads
    // it. `PROFILE` is set by cargo for exactly this kind of decision.
    let debug = std::env::var("PROFILE").as_deref() != Ok("release");

    // One compile root. Everything else is imported from it, so there is a
    // single place where the component tree starts.
    //
    // # Translations are bundled, not loaded
    //
    // `with_bundled_translations` compiles every `translations/<lang>/
    // LC_MESSAGES/pecu-ui.po` **into the binary**, and
    // `slint::select_bundled_translation` switches between them at runtime.
    //
    // The alternative is Slint's `gettext` feature, which resolves against the
    // system catalogue at runtime through `gettext-rs` — a C dependency, which
    // on Windows is a build problem rather than a line in a manifest. It would
    // also mean a wallet that reads its own interface text out of files beside
    // the executable, and there is no reason to give an installer that surface.
    //
    // The cost is that adding a language is a rebuild. For an application that
    // ships as a signed bundle, it was going to be one anyway.
    let config = slint_build::CompilerConfiguration::new()
        .with_debug_info(debug)
        .with_bundled_translations("translations")
        // Slint defaults the gettext context to the enclosing component's name,
        // which means every `msgctxt` in every `.po` has to track the name of
        // the component the string happens to sit in — so moving a `Text` from
        // one component to another silently drops its translation. Off, and the
        // extractor is invoked with `--no-default-translation-context` to
        // match. See `translations/README.md`.
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);

    slint_build::compile_with_config("ui/app.slint", config)?;
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-changed=translations");
    Ok(())
}
