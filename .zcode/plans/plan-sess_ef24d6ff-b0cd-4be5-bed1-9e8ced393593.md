# 修复「探测结果与 curl 不一致」的感知问题

## 根因（已用运行时证据确认）

后端探测是**正确的**：弹窗顶部「共探测到 X 个模型」的 X 与 curl 返回数量一致（约 10 个），URL 推导（`/api/anthropic/v1/models`）和 x-api-key 认证都与智谱文档一致（生产库 7261 次成功请求也证明 x-api-key 可用）。

用户感知的「不一致」来自导入弹窗的三个设计：

1. **主因：已存在模型被直接隐藏**。弹窗把表单里已有的 3 个模型从下拉选项中剔除（防止重复导入），用户看到下拉只有 ~7 个而 curl 有 10 个 → 以为探测结果不对。顶部小字提示「已排除 N 个已存在」容易被忽略。
2. 次因：后端把模型列表按字母序重排，与 curl 返回的上游顺序（通常新模型在前）不同。
3. 次因：display_name 与 id 仅大小写不同时（如 `glm-4.6` vs `GLM-4.6`），标签显示成重复感的「glm-4.6（GLM-4.6）」。

## 修复方案

### 前端 `src/views/Providers.vue`

1. **`probeOptions` 显示完整列表**：不再过滤已存在模型；已存在的选项标记 `disabled: true`，label 后缀「（已存在）」。
2. **顶部文案**改为：「共 X 个模型，其中 N 个已存在（灰显），请选择要导入的模型」。
3. **「全选」按钮**只选中未禁用的选项：`probeOptions.filter(o => !o.disabled).map(o => o.value)`（computed 需带 disabled 字段）。
4. **label 优化**：display_name 与 id 不区分大小写相同时不附加。
5. `handleProbeModels` 删除过滤逻辑（existing 集合移入 probeOptions computed）；`confirmProbeImport` 不变（disabled 选项选不中）。

### 后端 `src-tauri/src/server/api.rs`（probe_models）

- 删除 `items.sort_by(...)` 一行，保留 `dedup_by` —— 保持上游原始顺序（与 curl 输出顺序一致）。

### 验证与重启

1. `pnpm build` 类型检查通过。
2. 重编后端并重启 7860：`kill <nohup进程> && cargo build --no-default-features --features server --bin ai-proxy-server`，然后带数据目录 .env 环境变量（AI_PROXY_JWT_SECRET、AI_PROXY_DATA_DIR）nohup 重启，`/health` 200 确认。
3. 用户在编辑供应商对话框重新点「探测模型」：应看到 10 个模型（含 3 个灰显「已存在」），顺序与 curl 一致。