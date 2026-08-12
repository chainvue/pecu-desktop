//! Compiles the Slint interface.
//!
//! A `.slint` syntax error failing this build *is* the UI test for the early
//! phases — there is no cheaper way to find one, and no reason to let it reach
//! runtime.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // One compile root. Everything else is imported from it, so there is a
    // single place where the component tree starts.
    slint_build::compile("ui/app.slint")?;
    println!("cargo:rerun-if-changed=ui");
    Ok(())
}
