mod gui;

use eframe::egui;
use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    // Handle flags before any window exists, so scripts can query the binary.
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") if args.next().is_none() => {
            println!("MacCleanUp {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help" | "-h") if args.next().is_none() => {
            println!(
                "MacCleanUp {}\n\nReviewed cleanup for macOS. Open MacCleanUp.app to use it.\n\nUsage: MacCleanUp\n       MacCleanUp --version",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
        None => {}
        // macOS passes -psn_… to apps opened from Finder on older releases.
        Some(argument) if argument.starts_with("-psn_") => {}
        Some(argument) => return Err(format!("unknown argument: {argument}. Use --help.").into()),
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set; cannot locate your Library safely")?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MacCleanUp")
            .with_inner_size([1240.0, 800.0])
            .with_min_inner_size([980.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "MacCleanUp",
        options,
        Box::new(move |_creation| Ok(Box::new(gui::MacCleanUp::new(home)))),
    )?;
    Ok(())
}
