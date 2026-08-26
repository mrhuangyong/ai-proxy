修复 dev/生产双构建启动 codex 时的"端口互踩"——昨天修好的功能实际被今早 08:45 的 dev 构建覆盖 config.toml（base_url 从 17860 变 7860，随后 dev 进程退出、7860 无人监听）弄失效的。昨天的两处代码修复本身没有回归（生产库 WAL 显示昨晚 23:03 与今早 08:41 两次生产启动均正常，当天 codex 请求全部成功路由到 glm-5.3）。

## 改动：dev 构建写 codex 配置时加防串扰措施

文件：`src-tauri/src/apps/handlers.rs` `launch_app`（读 `http_port` 处，约 219-236 行）+ `src-tauri/src/apps/config.rs`（如需）

方案（按优先级递进，实施时全做）：

1. **dev 构建使用独立端口段**：`cfg!(debug_assertions)` 且 settings 未显式配置 `http_port` 时，dev 默认端口从 7860 改为 17860 之外的独立值（如 7861），避免与生产默认/常用值撞车。仅改默认值，不动已显式配置的用户设置。

2. **launch 前做端口存活检测并拒绝写坏配置**：`launch_app` 写 config.toml 前，对将写入的 `proxy_base` 做一次 TCP 连通性探测（tokio 连 `127.0.0.1:port`，300ms 超时）。探测失败则返回明确错误："代理服务未在端口 {port} 监听，已阻止改写 codex 配置（当前 config.toml 指向的代理仍有效）"，**不写文件**。这样 dev 构建若没在跑代理或端口不对，就不会把生产配置覆盖坏。

3. **写前备份**：覆盖 `~/.codex/config.toml` 前复制一份 `config.toml.aiproxy.bak`，出问题时用户可手工恢复（与现有 config.toml.bak 模式一致，只在内容变化时备份）。

## 验证

1. `cd src-tauri && cargo test` + `cargo fmt`
2. 手动：生产 app 正常启动 codex 后，另跑 `pnpm tauri dev` 并尝试从 dev 启动 codex → 应被拦截报"端口未监听"或写独立端口，且 `~/.codex/config.toml` 的 base_url 仍指向生产 17860
3. 立即恢复措施（无需等代码）：从生产 app Apps 页重新启动 codex，base_url 会写回 17860

## 明确排除

- 不改昨天的两处修复（model 键写入 + gpt* 规则收窄）——它们工作正常
- 不动 settings 表已有数据
- failover /models 缺 slug 的遗留问题本次仍不处理