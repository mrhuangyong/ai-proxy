pub mod converter;
pub mod http;
mod db;
mod error;
mod provider;
mod key;
mod routing;
mod interceptor;
mod usage;
mod logging;
mod server;
mod apps;
mod mcp;
mod skill;
mod update;
mod update_timer;

use crate::logging::layer::BroadcastLayer;

static LOG_LAYER: std::sync::OnceLock<BroadcastLayer> = std::sync::OnceLock::new();

pub fn get_log_layer() -> &'static BroadcastLayer {
    LOG_LAYER.get_or_init(BroadcastLayer::new)
}

use tauri::Manager;
use tauri::Emitter;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use std::sync::Mutex;
use once_cell::sync::Lazy;

const DEFAULT_PROXY_PORT: u16 = 7860;
const DEFAULT_PROXY_HOST: &str = "0.0.0.0";

static APP_RUNTIME: Lazy<Mutex<Option<tokio::runtime::Runtime>>> = Lazy::new(|| Mutex::new(None));

struct ProxyControl {
    running: bool,
    port: u16,
    host: String,
    shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
}

static PROXY_CONTROL: Lazy<Mutex<ProxyControl>> = Lazy::new(|| {
    Mutex::new(ProxyControl {
        running: false,
        port: DEFAULT_PROXY_PORT,
        host: DEFAULT_PROXY_HOST.to_string(),
        shutdown_tx: None,
    })
});

async fn get_proxy_config() -> (String, u16) {
    let pool = db::get_pool().await;
    let port_str: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'http_port'")
        .fetch_one(pool)
        .await
        .unwrap_or_else(|_| DEFAULT_PROXY_PORT.to_string());
    (DEFAULT_PROXY_HOST.to_string(), port_str.parse().unwrap_or(DEFAULT_PROXY_PORT))
}

fn to_connect_host(host: &str) -> String {
    if host == "0.0.0.0" {
        "127.0.0.1".to_string()
    } else {
        host.to_string()
    }
}

#[tauri::command]
async fn get_api_config() -> String {
    let (host, port) = get_proxy_config().await;
    format!("http://{}:{}", to_connect_host(&host), port)
}

#[tauri::command]
async fn apply_proxy_config() -> String {
    stop_proxy();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    start_proxy();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    get_api_config().await
}

#[tauri::command]
async fn reset_all_data(app: tauri::AppHandle) -> Result<(), String> {
    let base_data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let app_data_dir = if cfg!(debug_assertions) {
        base_data_dir.with_file_name("com.aiproxy.app-dev")
    } else {
        base_data_dir
    };

    // Stop proxy first
    stop_proxy();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Remove all data files
    if app_data_dir.exists() {
        std::fs::remove_dir_all(&app_data_dir).map_err(|e| format!("Failed to remove data: {}", e))?;
    }
    std::fs::create_dir_all(&app_data_dir).map_err(|e| format!("Failed to recreate data dir: {}", e))?;

    // Restart the app
    app.restart();
    #[allow(unreachable_code)]
    Ok(())
}

fn start_proxy() -> (String, u16) {
    {
        let ctrl = PROXY_CONTROL.lock().unwrap();
        if ctrl.running {
            let host = ctrl.host.clone();
            let port = ctrl.port;
            return (host, port);
        }
    }

    let handle = {
        let guard = APP_RUNTIME.lock().unwrap();
        guard.as_ref().expect("runtime not initialized").handle().clone()
    };

    let (tx, rx) = tokio::sync::watch::channel(false);

    handle.spawn(async move {
        let (host, port) = get_proxy_config().await;

        {
            let mut ctrl = PROXY_CONTROL.lock().unwrap();
            ctrl.running = true;
            ctrl.host = host.clone();
            ctrl.port = port;
            ctrl.shutdown_tx = Some(tx);
        }

        server::start_server(&host, port, rx).await;

        let mut ctrl = PROXY_CONTROL.lock().unwrap();
        ctrl.running = false;
    });

    (DEFAULT_PROXY_HOST.to_string(), DEFAULT_PROXY_PORT)
}

fn stop_proxy() {
    let mut ctrl = PROXY_CONTROL.lock().unwrap();
    if !ctrl.running {
        return;
    }
    if let Some(tx) = ctrl.shutdown_tx.take() {
        let _ = tx.send(true);
    }
    ctrl.running = false;
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(target_os = "macos")]
fn set_dock_visibility(visible: bool) {
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use std::ffi::CStr;

    unsafe {
        let ns_app_class = AnyClass::get(CStr::from_bytes_with_nul(b"NSApplication\0").unwrap())
            .expect("NSApplication not found");
        let ns_app: *mut AnyObject = msg_send![ns_app_class, sharedApplication];
        let policy: i64 = if visible { 0 } else { 1 }; // 0 = Regular, 1 = Accessory
        let _: () = msg_send![ns_app, setActivationPolicy: policy];

        if visible {
            // Restore the application icon to the bundle default
            let _: () = msg_send![ns_app, setApplicationIconImage: std::ptr::null_mut::<AnyObject>()];
            // Ensure the app is properly activated
            let _: () = msg_send![ns_app, activateIgnoringOtherApps: 1i8];
        }
    }
}

fn should_show_main_window_for_run_event(event: &tauri::RunEvent) -> bool {
    #[cfg(target_os = "macos")]
    {
        matches!(event, tauri::RunEvent::Reopen { .. })
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = event;
        false
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tracing_subscriber::prelude::*;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::Layer::default().with_filter(tracing_subscriber::filter::LevelFilter::INFO))
        .with(get_log_layer().clone().with_filter(tracing_subscriber::filter::LevelFilter::INFO))
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .setup(|app| {
            let base_data_dir = app.path().app_data_dir().expect("failed to get app data dir");
            let app_data_dir = if cfg!(debug_assertions) {
                base_data_dir.with_file_name("com.aiproxy.app-dev")
            } else {
                base_data_dir
            };
            std::fs::create_dir_all(&app_data_dir).expect("failed to create app data dir");

            if cfg!(debug_assertions) {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.set_title("AI Proxy [DEV]");
                }
            }

            let db_path = app_data_dir.join("ai-proxy.db");
            let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");
            rt.block_on(async {
                db::init::init_db(db_path.to_str().unwrap()).await
                    .expect("failed to initialize database");
            });

            {
                let mut guard = APP_RUNTIME.lock().unwrap();
                *guard = Some(rt);
            }

            start_proxy();
            update_timer::start_update_timer(app.handle().clone());

            let check_update_item = MenuItem::with_id(app, "check-update", "Check for Updates", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&check_update_item, &quit_item])?;

            let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;

            TrayIconBuilder::with_id("main-tray")
                .icon(icon)
                .tooltip("AI Proxy")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| {
                    if event.id() == "quit" {
                        stop_proxy();
                        app.exit(0);
                    } else if event.id() == "check-update" {
                        let app_handle = app.clone();
                        let handle = {
                            let guard = APP_RUNTIME.lock().unwrap();
                            guard.as_ref().expect("runtime not initialized").handle().clone()
                        };
                        handle.spawn(async move {
                            match update::check_update(&app_handle).await {
                                Ok(Some(info)) => {
                                    let _ = app_handle.emit("update-available", &info);
                                }
                                Ok(None) => {
                                    let _ = app_handle.emit("up-to-date", ());
                                }
                                Err(e) => {
                                    tracing::warn!("Manual update check failed: {}", e);
                                }
                            }
                        });
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        #[cfg(target_os = "macos")]
                        set_dock_visibility(true);
                        show_main_window(app);
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_api_config, apply_proxy_config, reset_all_data, update::check_for_update, update::download_update, update::open_update_file])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                #[cfg(target_os = "macos")]
                set_dock_visibility(false);
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if should_show_main_window_for_run_event(&event) {
                show_main_window(app_handle);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::should_show_main_window_for_run_event;

    #[test]
    fn ready_event_does_not_request_window_restore() {
        assert!(!should_show_main_window_for_run_event(&tauri::RunEvent::Ready));
    }
}
