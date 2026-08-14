//! Tauri v2 mobile/desktop app entry point.
//!
//! TODO (Phase 4): wire up Tauri commands bridging to `rust_note_core` and,
//! eventually, to the server's API. This is currently a minimal skeleton
//! and is not expected to fully build without a `tauri.conf.json` and app
//! icons in place.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Edge-to-edge inset injection (status/navigation bar heights as CSS
        // variables) — auto-enables on webview load, no JS calls needed.
        .plugin(tauri_plugin_edge_to_edge::init())
        // SAF folder access for the notes-folder mirror; permissions granted
        // via capabilities/default.json ("android-fs:default").
        .plugin(tauri_plugin_android_fs::init())
        // OIDC login: open the OS browser (opener) and receive the device token
        // back via the `dev.rustnote.app://auth` deep link. The frontend reads
        // the launch/opened URL through the plugin's JS API (getCurrent /
        // onOpenUrl); no Rust-side handler needed. Permissions:
        // "deep-link:default" / "opener:default" in capabilities/default.json.
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
