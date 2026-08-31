//! Linux system tray via the StatusNotifierItem D-Bus protocol (ksni/zbus).
//!
//! Tauri's tray (libappindicator backend) never emits icon click events on
//! Linux, which makes restoring a hidden window impossible without the
//! context menu. ksni talks to the spec directly, so `activate` (left click)
//! reaches us.

use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, ToolTip, Tray, TrayMethods};

struct AiProxyTray {
    app: tauri::AppHandle,
    icon: Vec<Icon>,
}

pub(crate) fn spawn_tray(app: tauri::AppHandle) {
    let tray = AiProxyTray {
        app,
        icon: decode_argb32(include_bytes!("../icons/32x32.png")),
    };
    let handle = {
        let guard = crate::APP_RUNTIME.lock().unwrap();
        guard
            .as_ref()
            .expect("runtime not initialized")
            .handle()
            .clone()
    };
    handle.spawn(async move {
        if let Err(e) = tray.spawn().await {
            tracing::error!("Failed to start system tray: {}", e);
        }
    });
}

/// Convert a PNG to the ARGB32 network-byte-order pixmap the spec requires.
fn decode_argb32(png: &[u8]) -> Vec<Icon> {
    match image::load_from_memory(png) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (width, height) = rgba.dimensions();
            let mut data = Vec::with_capacity((width * height * 4) as usize);
            for pixel in rgba.pixels() {
                let [r, g, b, a] = pixel.0;
                data.extend_from_slice(&[a, r, g, b]);
            }
            vec![Icon {
                width: width as i32,
                height: height as i32,
                data,
            }]
        }
        Err(e) => {
            tracing::warn!("Failed to decode tray icon: {}", e);
            Vec::new()
        }
    }
}

impl Tray for AiProxyTray {
    fn id(&self) -> String {
        "ai-proxy".into()
    }

    fn title(&self) -> String {
        "AI Proxy".into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "AI Proxy".into(),
            ..Default::default()
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icon.clone()
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            MenuItem::Standard(StandardItem {
                label: "Show Main Window".into(),
                activate: Box::new(|tray: &mut Self| {
                    crate::show_main_window(&tray.app);
                }),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: "Check for Updates".into(),
                activate: Box::new(|tray: &mut Self| {
                    crate::spawn_update_check(&tray.app);
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| {
                    crate::stop_proxy();
                    tray.app.exit(0);
                }),
                ..Default::default()
            }),
        ]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        crate::show_main_window(&self.app);
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        crate::show_main_window(&self.app);
    }
}
