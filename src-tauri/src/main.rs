// Entry point stays thin on purpose — real setup lives in lib.rs, shared
// with any future mobile target per Tauri v2 convention.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    syncblaze_desktop_lib::run();
}
