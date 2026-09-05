// SyncBlaze desktop companion.
//
// This app has exactly one job beyond showing the existing web UI: run a
// tiny local relay server on the LAN so a phone's browser can reach this
// computer directly (no camera needed here, no internet needed at all) and
// exchange WebRTC signaling with whatever's running in the webview. The
// relay itself understands nothing about WebRTC — it just forwards opaque
// JSON messages between whoever's connected under the same pairing code,
// mirroring the same shape as the cloud "Quick Connect" relay
// (backend/src/sockets/quickPair.ts) so the shared frontend code can treat
// both the same way.
mod oauth;
mod relay;

use oauth::OAuthState;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
};

// Fixed rather than OS-assigned: predictable for the QR code, and a
// collision on this port is rare enough not to be worth the complexity of
// reading back an OS-chosen port through the setup hook.
pub const LAN_RELAY_PORT: u16 = 47811;

#[tauri::command]
fn get_lan_info() -> Result<serde_json::Value, String> {
    let ip = local_ip_address::local_ip().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "ip": ip.to_string(), "port": LAN_RELAY_PORT }))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let oauth_state = OAuthState::default();
            app.manage(oauth_state.clone());
            tauri::async_runtime::spawn(relay::serve(LAN_RELAY_PORT, oauth_state));

            // Closing the window shouldn't kill the relay — the whole point
            // is that it keeps running so the phone can reach it. Hide
            // instead, and give people a real way to actually quit via the
            // tray, otherwise this is just an undiscoverable zombie process.
            if let Some(window) = app.get_webview_window("main") {
                let window_for_close = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        window_for_close.hide().ok();
                        api.prevent_close();
                    }
                });
            }

            let quit = MenuItem::with_id(app, "quit", "Quit SyncBlaze", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .tooltip("SyncBlaze — local pairing running")
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "quit" {
                        app.exit(0);
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click { .. } = event {
                        if let Some(window) = tray.app_handle().get_webview_window("main") {
                            window.show().ok();
                            window.set_focus().ok();
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_lan_info, oauth::start_google_signin])
        .run(tauri::generate_context!())
        .expect("error while running SyncBlaze desktop");
}
