## 目标

`/v1/models`（公开模型发现端点）响应中的模型名称只保留 `provider/model_name`（qualified）形式，去掉所有裸 `model_name`。同步统一 Gemini 形状的 `/v1beta/models` 与 codex 形状的 `models[]`，保持三个列表行为一致。

## 改动内容（均在 `src-tauri/src/server/handlers.rs`）

1. **`handle_list_models`（handlers.rs:2271-2364）**
   - `data[]`：`flat_map` 产出 `[bare, qualified]` 两条 → 改为 `map` 只产出 qualified 一条（`id` = `provider/model_name`），条数从 2N 变 N。
   - `models[]`（codex shapes）：`slug` 与 `display_name` 从裸 `model_name` 改为 `qualified_model_id(...)`；`name` 本来就是 qualified，不动。
   - 同步更新 2296-2299 处关于双形状的注释。

2. **`handle_gemini_list_models`（handlers.rs:2388-2416）**
   - 同样去掉 bare 条目，只保留 `name` = `models/<provider>/<model_name>` 的一条。

3. **不改的部分（保证兼容）**
   - `handle_get_model`、`route_matches_model`、`handle_gemini_get_model`：请求/按 id 查询仍同时接受裸名和 qualified 名，旧客户端配置不断。
   - 请求侧路由 `ProviderManager::find_for_model`：裸名与 qualified 双形式解析保持原样。codex 之后用 qualified slug 发请求可正常路由（已有 `qualified_and_bare_model_routing` 测试覆盖）。
   - 管理端 `/api/providers`、`/api/virtual-models`、Apps 启动配置写入的裸名 model：与本端点无关，不动。

4. **新增集成测试 `src-tauri/tests/models_endpoint.rs`**
   - 复用 `provider_routing.rs` 的 setup 模式（tempdir + `init_db` + 直接插 SQLite：含两个 provider 共享同名模型、一个名字含 `/` 的模型）。
   - 因全局 pool（OnceLock）限制，所有断言放在单个 `#[tokio::test]` 内：直接调用 `handle_list_models().await` / `handle_gemini_list_models().await`，用 `axum::body::to_bytes` 取 body 解析 JSON，断言：
     - `data[]` 每个 `id` 均为 `provider/model_name`，无裸名条目，条数 == 模型路由数；
     - `models[]` 的 `slug`/`display_name` 均为 qualified；
     - Gemini `models[]` 的 `name` 均为 `models/<provider>/<model_name>`。

## 验证

1. `cd src-tauri && cargo fmt && cargo test`（全部通过）。
2. 若本地 dev server（端口 7860）在跑：重启后端后 `curl -s http://127.0.0.1:7860/v1/models` 确认输出只剩 qualified 形式（改 Rust 必须重启后端才生效）。