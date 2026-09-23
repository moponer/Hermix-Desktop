//! Smoke probe for the CUA driver layer.
//!
//! Answers three questions that reading the source cannot:
//! does screen enumeration work, does a screenshot actually produce
//! pixels (rather than silently falling back to a blank BitBlt frame),
//! and does UIA return a non-empty control tree. Only metadata is
//! printed — the captured frame is never written to disk.

use hermix_cua::capture::{screen_info, screenshot, CaptureConfig};
use hermix_cua::uia::UiaAutomation;

fn main() {
    match screen_info::enumerate() {
        Ok(monitors) => {
            println!("[MONITORS] count={}", monitors.len());
            for m in &monitors {
                println!(
                    "[MONITORS]   idx={} {} {}x{} @({},{}) primary={}",
                    m.index, m.name, m.width, m.height, m.x, m.y, m.is_primary
                );
            }
        }
        Err(e) => println!("[MONITORS] FAILED: {e}"),
    }

    match screenshot(CaptureConfig::default()) {
        Ok(s) => println!(
            "[SCREENSHOT] ok {}x{} monitor={} name={} png_bytes={} b64_len={}",
            s.width,
            s.height,
            s.monitor,
            s.monitor_name,
            s.bytes,
            s.png_base64.len()
        ),
        Err(e) => println!("[SCREENSHOT] FAILED: {e}"),
    }

    unsafe {
        match UiaAutomation::new() {
            Ok(auto) => match auto.get_control_view_tree(3) {
                Ok(tree) => {
                    println!("[UIA] ok elements={}", tree.len());
                    for e in tree.iter().take(8) {
                        println!(
                            "[UIA]   depth={} {:?} name={:?} class={:?}",
                            e.depth, e.control_type, e.name, e.class_name
                        );
                    }
                }
                Err(e) => println!("[UIA] tree FAILED: {e}"),
            },
            Err(e) => println!("[UIA] init FAILED: {e}"),
        }
    }

    match hermix_cua::element::enumerate_windows() {
        Ok(w) => println!("[WINDOWS] ok count={}", w.len()),
        Err(e) => println!("[WINDOWS] FAILED: {e}"),
    }
}
