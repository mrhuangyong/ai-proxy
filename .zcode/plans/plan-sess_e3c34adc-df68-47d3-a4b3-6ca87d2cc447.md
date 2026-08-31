修复 Linux 上托盘无法恢复窗口的问题（`src-tauri/src/lib.rs`）。

**根因**：tray-icon 0.23.1 的 Linux GTK/libappindicator 后端不发射任何 `TrayIconEvent`（已核实其源码中无点击事件实现），左键点击处理器在 Linux 上永不触发；而托盘菜单只有 "Check for Updates" / "Quit"，没有显示窗口的入口，窗口 `hide()` 后无法恢复。

**改动**：

1. **托盘菜单增加 "Show Main Window" 项**（放在第一位）：
   ```rust
   let show_item = MenuItem::with_id(app, "show-window", "Show Main Window", true, None::<&str>)?;
   let menu = Menu::with_items(app, &[&show_item, &check_update_item, &quit_item])?;
   ```
   在 `on_menu_event` 中处理 `"show-window"`：macOS 上先 `set_dock_visibility(true)`，然后调用 `show_main_window(app)`。这是 Linux 上的主要恢复途径。

2. **加固 `show_main_window`**（lib.rs:206）：先 `unminimize()` 再 `show()` + `set_focus()`，覆盖窗口被最小化的情况。

3. **window-state 插件过滤 VISIBLE 状态**（lib.rs:274 附近）：
   ```rust
   .plugin(tauri_plugin_window_state::Builder::new()
       .with_state_flags(tauri_plugin_window_state::StateFlags::all() & !tauri_plugin_window_state::StateFlags::VISIBLE)
       .build())
   ```
   避免某次以隐藏状态退出后，下次启动窗口保持隐藏。

4. 保留现有左键 Click 处理（macOS/Windows 继续有效）。

**验证**：`pnpm tauri dev` 自动重编译后，关闭窗口 → 右键托盘图标 → "Show Main Window" 应恢复窗口。

**说明**：菜单文案沿用现有英文（与 "Check for Updates"/"Quit" 一致）。完成后按你之前的流程提交推送。