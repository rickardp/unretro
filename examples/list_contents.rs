//! Example: List contents of any supported container format
//!
//! Usage: cargo run --example list_contents --features full -- <path>

use std::env;
use unretro::{EntryType, Loader, VisitAction, detect_format};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <container-file>", args[0]);
        eprintln!();
        eprintln!("Supported formats:");
        eprintln!("  - ZIP archives");
        eprintln!("  - LHA/LZH archives");
        eprintln!("  - GZIP compressed files");
        eprintln!("  - BinHex 4.0 encoded files (.hqx)");
        eprintln!("  - StuffIt archives (.sit)");
        eprintln!("  - MacBinary files (.bin)");
        eprintln!("  - AppleSingle/AppleDouble files");
        eprintln!("  - HFS disk images");
        eprintln!("  - SCUMM data files");
        std::process::exit(1);
    }

    let path = &args[1];

    // First detect the format
    let data = std::fs::read(path)?;
    if let Some(format) = detect_format(path, Some(&data)) {
        println!("Detected format: {}", format.name());
    }

    // Open and list contents using Loader
    println!("\nContents of {}:", path);
    println!("{:-<60}", "");

    let mut count = 0;
    Loader::from_path(path).visit(EntryType::Leaves, |entry| {
        let size = entry.data.len();
        println!("{:50} {:>8} bytes", entry.path, size);
        count += 1;
        Ok(VisitAction::Continue) // Continue to process all entries
    })?;

    println!("{:-<60}", "");
    println!("Total: {} entries", count);

    Ok(())
}
