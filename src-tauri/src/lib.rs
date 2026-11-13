pub mod commands;
pub mod core;

#[cfg(test)]
mod wire;

use std::path::PathBuf;

use tauri::menu::{Menu, MenuItem, MenuItemKind, WINDOW_SUBMENU_ID};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, Runtime, WindowEvent};

use crate::core::ai::session as chat;
use crate::core::secrets::{self, KeychainStore, VaultStore};
use crate::core::state::AppState;

const QUIT_ID: &str = "sandb-quit";
const CLOSE_TAB_ID: &str = "sandb-close-tab";
const CLOSE_WINDOW_ID: &str = "sandb-close-window";

fn has_pending<R: Runtime, M: Manager<R>>(m: &M) -> bool {
    m.try_state::<AppState>()
        .is_some_and(|s| s.has_pending_work())
}

/// The default menu's Quit is the native `terminate:` action, which exits without a close request
/// (tao has no `applicationShouldTerminate:` hook), so it would skip the pending-transaction prompt.
/// Swap it for our own item with the same shortcut. Dock Quit still calls `terminate:` directly.
fn app_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let menu = Menu::default(app)?;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Some(MenuItemKind::Submenu(first)) = menu.items()?.into_iter().next() {
        // macOS: the app menu; Windows: File. Quit is the last item of both.
        let items = first.items()?;
        if let Some(MenuItemKind::Predefined(_)) = items.last() {
            first.remove_at(items.len() - 1)?;
            let quit = MenuItem::with_id(app, QUIT_ID, "Quit sandb", true, Some("CmdOrCtrl+Q"))?;
            first.append(&quit)?;
        }
    }
    // ⌘W / Ctrl+W closes the active query tab, as in a browser. The default menu binds it to Close
    // Window (File menu, and the Window menu on macOS), which would close the whole app window.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        // File follows the app menu on macOS and comes first on Windows. Its first item is Close Window.
        let file_at = if cfg!(target_os = "macos") { 1 } else { 0 };
        if let Some(MenuItemKind::Submenu(file)) = menu.items()?.into_iter().nth(file_at)
            && let Some(MenuItemKind::Predefined(_)) = file.items()?.first()
        {
            file.remove_at(0)?;
            let close_tab =
                MenuItem::with_id(app, CLOSE_TAB_ID, "Close Tab", true, Some("CmdOrCtrl+W"))?;
            file.insert(&close_tab, 0)?;
        }
    }
    // The Window menu's Close Window moves to ⇧⌘W. It goes through the same close request, so the
    // pending-transaction prompt still applies.
    #[cfg(target_os = "macos")]
    if let Some(MenuItemKind::Submenu(window)) = menu.get(WINDOW_SUBMENU_ID) {
        let items = window.items()?;
        if let Some(MenuItemKind::Predefined(_)) = items.last() {
            window.remove_at(items.len() - 1)?;
            let close = MenuItem::with_id(
                app,
                CLOSE_WINDOW_ID,
                "Close Window",
                true,
                Some("CmdOrCtrl+Shift+W"),
            )?;
            window.append(&close)?;
        }
    }
    Ok(menu)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let custom_dir = std::env::var_os("SANDB_DATA_DIR").map(PathBuf::from);
            // The credential store is shared by the whole OS account. A custom data dir gets its
            // own service name, so its secrets never mix with those of the real app data.
            let (dir, service) = match custom_dir {
                Some(d) => {
                    let service = format!("{} ({})", app.config().identifier, d.display());
                    (d, service)
                }
                None => (app.path().app_data_dir()?, app.config().identifier.clone()),
            };
            let vault = VaultStore::new(&dir, Box::new(KeychainStore::new(service.clone())));
            // Not fatal: the file stays and the next start tries again. Until then, the passwords
            // that were not moved are missing and those connections fail to log in.
            if let Err(e) = secrets::migrate_legacy_file(&dir, &vault) {
                eprintln!(
                    "sandb: could not move saved passwords to {}: {e}",
                    secrets::VAULT_FILE
                );
            }
            let state = AppState::new(dir, Box::new(vault))?;
            let accounts = state.secret_accounts();
            // Not fatal either: the old entries stay and the next start tries again.
            if let Err(e) = secrets::migrate_keychain_entries(
                &KeychainStore::new(service),
                state.secrets(),
                &accounts,
            ) {
                eprintln!(
                    "sandb: could not move saved passwords to {}: {e}",
                    secrets::VAULT_FILE
                );
            }
            app.manage(state);
            app.set_menu(app_menu(app.handle())?)?;
            Ok(())
        })
        // Quit asks the frontend first when a tab session could hold uncommitted work: the
        // window close (or menu Quit) is held back, the UI shows the same Commit / Rollback /
        // Cancel prompt as a tab close, and `quit_app` exits once the user decides.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && has_pending(window)
            {
                api.prevent_close();
                let _ = window.emit("sandb://quit-requested", ());
            }
        })
        // A reload of the main page drops the chat streams it listened to; stop their runs.
        .on_page_load(|webview, payload| {
            if payload.event() == PageLoadEvent::Started
                && webview.label() == "main"
                && let Some(state) = webview.try_state::<AppState>()
            {
                chat::cancel_all(&state);
            }
        })
        .on_menu_event(|app, event| {
            if event.id() == QUIT_ID {
                if has_pending(app) {
                    let _ = app.emit("sandb://quit-requested", ());
                } else {
                    app.exit(0);
                }
            } else if event.id() == CLOSE_TAB_ID {
                let _ = app.emit("sandb://close-tab", ());
            } else if event.id() == CLOSE_WINDOW_ID
                && let Some(w) = app.get_webview_window("main")
            {
                let _ = w.close();
            }
        })
        .invoke_handler(commands::handler())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
