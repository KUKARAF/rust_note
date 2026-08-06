//! Tauri v2 mobile/desktop app entry point.
//!
//! TODO (Phase 4): wire up Tauri commands bridging to `rust_note_core` and,
//! eventually, to the server's API. This is currently a minimal skeleton
//! and is not expected to fully build without a `tauri.conf.json` and app
//! icons in place.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
