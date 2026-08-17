//! Write the interface to PNG files, without opening a window.
//!
//! ```sh
//! cargo run -p pecu-ui --example render_shots
//! ```
//!
//! For looking at. `tests/visual.rs` renders the same frames and compares them
//! against the checked-in references, so if you change a layout deliberately:
//! run this, look at the result, and commit the images alongside the code.

use pecu_ui::snapshot;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = snapshot::install()?;

    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/shots");
    std::fs::create_dir_all(&out_dir)?;

    for (screen, label, seed) in snapshot::CASES {
        for dark in [true, false] {
            let frame = snapshot::render(&window, screen, dark, *seed)?;
            let theme = if dark { "dark" } else { "light" };
            let path = out_dir.join(format!("{label}-{theme}.png"));

            let image = image::RgbImage::from_raw(frame.width, frame.height, frame.rgb)
                .ok_or("buffer size did not match the image dimensions")?;
            image.save(&path)?;
            println!("{}", path.display());
        }
    }

    Ok(())
}
