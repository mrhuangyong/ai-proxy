// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // NVIDIA + WebKit2GTK: DMABUF renderer fails to allocate GBM buffers and
    // renders a blank window; fall back to the shared-memory renderer.
    #[cfg(target_os = "linux")]
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    ai_proxy_lib::run()
}
