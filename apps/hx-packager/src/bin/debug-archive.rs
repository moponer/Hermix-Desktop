//! hx-packager subcommand to debug what files are bundled into the archive.
use hermix_plugin::app_archive;
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let app_dir = PathBuf::from(args.next().unwrap_or_default());
    println!("debug-archive app_dir={}", app_dir.display());
    println!("--- should_include tests ---");
    for rel in &[
        "index.html",
        "manifest.json",
        "vendor/sql-wasm.js",
        "vendor/sql-wasm.wasm",
        "vendor/sql-wasm.js.map",
        "dist/playground.exe",
        "node_modules/lodash/index.js",
        ".gitignore",
        "main.css",
        "src/main.rs",
    ] {
        println!("  should_include({:?}) = {}", rel, app_archive::should_include(rel));
    }
    println!("--- archive::build ---");
    match app_archive::build(&app_dir) {
        Ok(a) => {
            println!("entries ({}):", a.entries.len());
            for e in &a.entries {
                println!("  {} ({} bytes @ {})", e.relpath, e.length, e.offset);
            }
            println!("blob total: {} bytes", a.blob.len());
        }
        Err(e) => println!("ERROR: {e}"),
    }
}