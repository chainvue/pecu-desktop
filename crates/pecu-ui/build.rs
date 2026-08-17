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
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_debug_info(debug),
    )?;
    println!("cargo:rerun-if-changed=ui");
    Ok(())
}
