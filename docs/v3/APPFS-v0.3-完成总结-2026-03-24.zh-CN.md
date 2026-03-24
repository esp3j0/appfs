# APPFS v0.3 完成总结（2026-03-24）

- 版本：`v0.3`
- 日期：`2026-03-24`
- 结论：`Closed (repository-level connectorization closeout)`

## 1. 本次收口范围（已完成）

1. Connector 主路径完成升级：runtime 默认走 `AppConnectorV2`，不再以 `AppAdapterV1` 作为 v0.3 默认入口。
2. runtime 主调用链完成接入：
   - `prewarm_snapshot_meta`
   - `fetch_snapshot_chunk`
   - `fetch_live_page`
   - `submit_action`
   - `health`
3. 三种 transport 主路径对齐到 V2 connector 语义：
   - in-process
   - HTTP bridge
   - gRPC bridge
4. demo connector parity 收口：snapshot/live/action 的核心行为、错误码口径、cursor 语义完成对齐（仅允许 transport 壳层差异）。
5. CT2/CI 门禁升级：
   - required gate 断言 runtime-derived connector evidence（而非测试脚本自报）。
   - HTTP/gRPC bridge 维持 informational signal 语义。
6. runner/CI 语义迁移方案落地：
   - `APPFS_V3_*` 为 canonical。
   - `APPFS_V2_*` 保留兼容窗口。

## 2. 本次不宣称完成（明确边界）

1. 多真实 app 的生产 rollout 与规模化接入。
2. “真实 app 已全面接入”类对外声明。
3. 对既有 branch-protection/ruleset 的一次性清理切换（本次仅提供兼容迁移窗口）。

## 3. 破坏性变更与迁移说明

1. v0.3 默认协议主路径已切换到 connector V2（发布语义以 V3 管理）。
2. legacy `AppAdapterV1` 仅作为兼容/回归表面，不是 v0.3 默认接入面。
3. bridge 默认协议面：
   - HTTP：`/v2/connector/*` 六个端点。
   - gRPC：V2 connector service。
4. runner/CI 变量迁移：
   - 首选 `APPFS_V3_*`。
   - `APPFS_V2_*` 仍可用作别名。
   - 若同一键同时设置，`APPFS_V3_*` 优先。

## 4. CI 与 check-run 策略（迁移窗口）

为避免 branch protection / merge queue / ruleset 的 expected-check pending drift，迁移窗口内冻结以下 check-run 名称：

1. `AppFS Contract Gate (required, linux, inprocess v2)`
2. `AppFS Contract Signal (informational, linux, http bridge v2)`
3. `AppFS Contract Signal (informational, linux, http bridge v2 high-risk)`
4. `AppFS Contract Signal (informational, linux, grpc bridge v2)`

说明：

1. 名称冻结不代表语义停留在 v0.2；runner/env 语义已切到 v0.3（`APPFS_V3_*` 优先）。
2. 后续在独立清理议题中再做 check-run 名称统一切换，避免一次性破坏既有保护规则。

## 5. 与计划文档对齐

1. 本次仓库级收口完成了 v0.3 connectorization 主线与门禁/文档迁移目标。
2. 真实 app pilot 仍作为后续专项，不纳入本次“仓库发布已完成”声明。

参见：

1. `docs/v3/APPFS-v0.3-实施计划.zh-CN.md`
2. `docs/v3/APPFS-v0.3-Connectorization-ADR.zh-CN.md`
3. `docs/v3/APPFS-v0.3-Connector接口.zh-CN.md`
