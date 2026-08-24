use std::{error::Error, fs, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let out_path = Path::new("src/resources/logo.ansi");
    let logo_bytes = include_bytes!("src/resources/logo.png");
    let ansi = logo_art::image_to_ansi(logo_bytes, 90);
    // Only rewrite when the content differs: `cargo package` verification
    // rejects packages whose build script modifies files in the source dir.
    if !matches!(fs::read(out_path), Ok(current) if current == ansi.as_bytes()) {
        fs::write(out_path, ansi)?;
    }
    println!("cargo::rerun-if-changed=src/resources/logo.png");
    Ok(())
}
