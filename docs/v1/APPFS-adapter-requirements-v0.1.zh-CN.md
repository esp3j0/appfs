# AppFS Adapter 层需求 v0.1（中文）

- 版本：`0.1-draft-r5`
- 日期：`2026-03-16`
- 状态：`Draft`
- 依赖：`APPFS-v0.1 (r9)`
- 一致性配置：`APPFS-conformance-v0.1.md`

## 1. 结论

当前 AppFS v0.1 设计已足够启动 adapter 实施。

理由：

1. 核心交互闭环已建立：`.act` 追加 JSONL -> 流事件。
2. Action 模式已定义：`inline` 与 `streaming`。
3. 发现契约已具备：`_meta/manifest.res.json` + schema。
4. 重放基线已具备：`cursor` + `from-seq`。

仍有已知但不阻塞 v0.1 的缺口（多租户共享、统一 cancel 规范、QoS 等级），后续版本处理。

## 2. 范围

本文仅定义 adapter 层需求。

范围内：

1. AppFS 节点到真实应用操作的映射。
2. Action 执行与事件发射。
3. Schema 与能力发布。
4. 校验与错误映射。

范围外：

1. 挂载后端实现（FUSE/WinFsp/NFS）。
2. 通用文件系统元数据内部实现。
3. 跨 App 编排与事务。

## 3. 角色与边界

### 3.1 Runtime 责任

1. 路径路由与文件系统操作分发。
2. 注入 session/principal 上下文。
3. 生成请求 ID（服务端）。
4. 流存储与重放面（`events`、`cursor`、`from-seq`）。
5. 调用 adapter 前执行路径归一化与 unsafe-path 预检查。

### 3.2 Adapter 责任

1. 领域/资源/动作注册。
2. 资源读取实现。
3. Action payload 校验与执行。
4. 按 AppFS schema 产出事件。
5. 应用特定权限与策略执行。

## 4. 功能需求

### AR-001 Manifest 发布

Adapter 必须提供构建 `_meta/manifest.res.json` 所需数据：

1. 节点列表及类型（`resource`/`action`）。
2. `input_mode`、`execution_mode`、schema 引用。
3. Action 限额（`max_payload_bytes`、可选 `rate_limit_hint`）。

### AR-002 资源读取

1. Adapter 必须将 `*.res.json` 节点解析为 UTF-8 JSON 输出（live/page 包装）。
2. Adapter 必须将 `*.res.jsonl` 节点解析为 UTF-8 JSONL 输出（snapshot 全量文件）。
3. 资源不存在必须映射 `ENOENT`。
4. 无权限必须映射 `EACCES`。

### AR-002A Snapshot 物化限额

1. `output_mode=jsonl` 资源必须在 manifest 声明 `snapshot.max_materialized_bytes`。
2. snapshot 资源不得声明分页元数据。
3. 对超限 snapshot 的刷新/物化检查必须返回确定性终态失败：`error.code = "SNAPSHOT_TOO_LARGE"`。

### AR-003 Action 提交（`*.act`）

1. Runtime 对 `*.act` 每条已提交的 JSONL 行调用 adapter。
2. Adapter 必须按 `input_mode` 与声明 schema 校验 payload。
3. 校验失败必须返回确定性错误（`EINVAL`/`EMSGSIZE`），且不得发出 `action.accepted`。
4. 已接受请求必须使用 runtime 提供的 `request_id` 产出流事件。
5. Runtime 必须将“以换行结尾的 JSONL 记录”作为提交边界；尾部未完成行（无 `\n`）必须丢弃且不得产生副作用。
6. Runtime 应暂存请求字节并原子升级为“行级提交输入”，确保 adapter 不会收到截断 payload。

### AR-004 执行模式

#### AR-004A Inline 模式

1. Adapter 应在 `inline_timeout_ms` 内完成。
2. Adapter 可返回同步成功/失败。
3. 即便同步处理，也应发出终态事件（`action.completed` 或 `action.failed`）。
4. 若超时，可降级为异步并发出 `action.accepted`。

#### AR-004B Streaming 模式

1. 提交后必须尽快发出 `action.accepted`。
2. 可按应用定义节奏发出 `action.progress`。
3. 必须且仅能发出一个终态事件（`action.completed` 或 `action.failed`，可选 `action.canceled`）。

### AR-005 事件契约

每行事件必须包含：

1. `seq`（由 runtime 流层分配）
2. `event_id`
3. `ts`
4. `app`
5. `session_id`
6. `request_id`
7. `path`
8. `type`

对 `action.failed`，必须包含 `error.code` 与 `error.message`。  
`event_id` 必须在重放时稳定，并在 app 流保留窗口内唯一。

### AR-006 关联能力

1. Adapter 必须支持服务端生成 `request_id`。
2. 若 payload 含 `client_token`，adapter 应在事件 payload 回显该 token 便于关联。

### AR-007 与重放支持协作

1. Adapter 必须按每个请求的因果顺序发射事件。
2. Adapter 必须容忍读取端从较老 `seq` 重新连接重放。

### AR-008 搜索支持

1. 适用时应提供简单投影资源（如 `by-name/.../index.res.json`）。
2. 可暴露复杂搜索 action sink（`search.act`），并以 cursor 化输出写入事件。

### AR-009 错误映射

Adapter 必须将应用错误映射为：

1. 文件系统 errno 类别。
2. 结构化事件错误 payload（`code`、`message`，可选 `retryable`、`details`）。

### AR-010 路径安全防护

1. Runtime + adapter 链路必须在副作用前拒绝 traversal 与 unsafe 路径。
2. 至少拒绝：`.`/`..` 段、盘符注入（`C:`）、反斜杠穿越、NUL 字节。
3. unsafe 输入时，不得触发 adapter 业务处理器（不得产生 app/backend 副作用）。

### AR-011 文件名/ID 可移植防护

1. Adapter 必须执行 AppFS 分段字符策略与保留名策略。
2. 对 runtime 生成且超过 255 UTF-8 字节的分段，必须执行确定性缩短并追加 hash 后缀。
3. 同一输入必须得到同一缩短输出。

### AR-012 流投递语义

1. 事件投递语义为 `at-least-once`。
2. Adapter 必须将重放与重复消费视为常态。
3. Runtime 必须分配稳定 `event_id`；adapter 与重放层必须原样保留。
4. Adapter 事件 payload 应包含稳定关联提示（`request_id`、可选 `client_token`）。

### AR-013 Observer 发布

Adapter 应暴露或提供 `/app/<app_id>/_meta/observer.res.json` 数据：

1. 动作计数（`accepted_total`、`completed_total`、`failed_total`）
2. 延迟聚合（`p95_accept_ms`、`p95_end_to_end_ms`）
3. 流压力（`stream_backlog`）
4. 最后错误时间（`last_error_ts`）

### AR-014 分页句柄错误契约

仅对 live 可分页资源，`/_paging/fetch_next.act` 与 `/_paging/close.act` 必须满足确定性映射：

1. `handle_id` 格式错误：提交时失败，返回 `EINVAL`，且不得发出 `action.accepted`。
2. 未知 handle：发 `action.failed`，`error.code = "PAGER_HANDLE_NOT_FOUND"`。
3. 过期 handle：发 `action.failed`，`error.code = "PAGER_HANDLE_EXPIRED"`。
4. 已关闭 handle：发 `action.failed`，`error.code = "PAGER_HANDLE_CLOSED"`。
5. 跨会话访问 handle：发 `action.failed`，`error.code = "PERMISSION_DENIED"`（可附应用细节码）。

### AR-015 并发提交顺序

1. Runtime 必须在每个 `request_id` 内保持因果顺序。
2. 对同一 `(app_id, session_id, action_path)`，`action.accepted` 顺序必须与观测到的 append JSONL 行顺序一致。
3. 并发提交下，每个已接受请求仍必须且仅有一个终态事件。

### AR-016 流表面原子性

对每个已提交事件 `seq = N`，runtime 必须原子保持以下表面一致：

1. `_stream/events.evt.jsonl` 包含 `N` 对应事件行。
2. `_stream/cursor.res.json` 的 `max_seq >= N`。
3. `_stream/from-seq/N.evt.jsonl` 可读且包含 `seq >= N`。

崩溃/重启后，不得暴露 `cursor` 超前于持久事件数据的部分发布状态。

### AR-017 适配器生命周期与健康

1. Adapter runtime 必须在接受 `.act` 前暴露 readiness。
2. Adapter runtime 必须暴露 liveness/health（直接 endpoint 或 observer 指标映射）。
3. 优雅关闭时，必须先停止新请求，再对进行中请求 drain 或确定性标记。
4. 重启恢复时，runtime 必须对已接受但未终态请求进行对账，并补发确定性终态。

### AR-018 Adapter SDK 抽象层与接口冻结

1. Runtime 必须通过稳定 adapter SDK 合约调用业务逻辑，而非硬编码 demo 分支。
2. AppFS Adapter SDK v0.1 接口必须显式冻结（`v0.1.x` 仅增量；破坏性变更需 `v0.2`）。
3. 第三方可用任意语言实现，但必须保持 AppFS 协议语义并通过一致性测试。
4. 一致性文档必须发布冻结的方法级合约及兼容声明标准。

### AR-019 CI 一致性门禁

1. 仓库 CI 必须执行 AppFS 静态合约检查（`APPFS_STATIC_FIXTURE=1`）。
2. 仓库 CI 必须在 Linux 执行 AppFS live 合约检查（`run-live-with-adapter.sh`）。
3. 任一破坏 Core 合约测试的变更必须导致 CI 失败。
4. CI 门禁定义必须版本化（workflow 文件），禁止仅靠本地脚本约定。
5. 参考 CI 应在同一 live 套件下覆盖 in-process 与 out-of-process（HTTP、gRPC）模式。

## 5. 非功能需求

### ANR-001 延迟

1. `inline` 目标：P95 <= 2s（应用相关）。
2. `streaming` 接受目标：P95 <= 1s 到 `action.accepted`。

### ANR-002 可靠性

1. `action.accepted` 后必须最终出现终态事件（进程崩溃除外）。
2. Adapter 应通过将流持久化委托 runtime 来保障崩溃安全。
3. 恢复路径必须保持每请求内事件顺序。

### ANR-003 可观测性

Adapter 必须输出结构化日志，至少包括：

1. `request_id`
2. action path
3. execution mode
4. latency
5. result status
6. normalized error code（失败时）

## 6. Adapter SDK 接口（Rust 参考，v0.1 冻结面）

以下 trait 形状是 v0.1 冻结的逻辑契约（协议语义语言无关，Rust 仅为参考）：

```rust
pub trait AppAdapterV1: Send {
    fn app_id(&self) -> &str;

    fn submit_action(
        &mut self,
        path: &str,
        payload: &str,
        input_mode: AdapterInputModeV1,
        execution_mode: AdapterExecutionModeV1,
        ctx: &RequestContextV1,
    ) -> Result<AdapterSubmitOutcomeV1, AdapterErrorV1>;

    fn submit_control_action(
        &mut self,
        path: &str,
        action: AdapterControlActionV1,
        ctx: &RequestContextV1,
    ) -> Result<AdapterControlOutcomeV1, AdapterErrorV1>;
}
```

其中：

1. `RequestContextV1` 由 runtime 提供（`app_id`、`session_id`、`request_id`、可选 `client_token`）。
2. `submit_action` 返回：
   1. `AdapterSubmitOutcomeV1::Completed`（inline 风格终态内容）
   2. `AdapterSubmitOutcomeV1::Streaming`（accepted/progress/terminal 计划，由 runtime 发射）
3. `submit_control_action` 用于控制通道（当前为分页 `fetch_next` / `close`）。
4. 流持久化（`events`、`cursor`、`from-seq`）与顺序/原子性保障由 runtime 负责。

### 6.1 v0.1 冻结策略

1. `v0.1.x` 仅允许向后兼容的增量变更。
2. 删除/重命名/变更现有必需方法行为属于破坏性变更，必须等待 `v0.2`。
3. 实现特定扩展必须标注为可选，且不得改变 Core 语义。
4. manifest 应发布适配器兼容元数据（如 `adapter_sdk_version`）。
5. SDK 对外提供 `APPFS_ADAPTER_SDK_VERSION` 与 `is_appfs_adapter_sdk_v01_compatible(...)` 作为规范版本检查助手。

### 6.2 一致性夹具建议

1. SDK 应提供可复用夹具测试：
   1. 必需 submit/control case matrix
   2. error case matrix
2. 不同 adapter 实现应可插入同一 matrix，而无需修改 runtime 逻辑。
3. Rust SDK 参考实现已在 `sdk/rust/src/appfs_adapter.rs` 提供 matrix 风格 trait 测试。
4. Rust SDK 参考 demo 在 `sdk/rust/src/appfs_demo_adapter.rs`。
5. CLI runtime 提供可选 HTTP bridge 传输，映射见 `APPFS-adapter-http-bridge-v0.1.md`。
6. gRPC 传输参考（proto + 示例）见 `APPFS-adapter-grpc-bridge-v0.1.md` 与 `examples/appfs/grpc-bridge/`。
7. CLI runtime 已提供原生 gRPC bridge 传输（`--adapter-grpc-endpoint`），语义映射同冻结 `AppAdapterV1`。
8. Rust SDK 在 `sdk/rust/src/appfs_adapter_testkit.rs` 发布可复用 matrix runners。
9. 仓库应提供适配器开发体验入口：
   1. 一键 conformance runner（`examples/appfs/run-conformance.sh`）
   2. bridge 专用 conformance runners
   3. 最小 adapter 模板 + quickstart
10. Runtime 应提供原生 bridge 韧性参数：
    1. 有界重试 + 退避
    2. 断路器保护
    3. 传输层可观测性指标
11. 仓库应提供最小进程外 backend 参考（基于 `uv`）：
    1. 协议层/业务层/测试钩子分层
    2. 校验/错误映射/故障注入单元测试
    3. 一键 live conformance 入口

## 7. 安全需求

1. Adapter 必须消费 runtime 提供的 principal/session 上下文（来自 `_meta/context` 模型）。
2. Adapter 必须在副作用前执行应用级 scope 检查。
3. 对需要审批的动作，adapter 必须发出 `action.awaiting_approval` 并延后终态结果。

## 8. 验证与验收清单

当以下检查全部通过时，adapter 实现可验收：

1. Manifest 完整：节点/动作/schema 字段齐全。
2. `.act` 校验路径：坏 payload 返回同步错误且无 `action.accepted`。
3. Inline 路径：同步结果正确且有终态事件。
4. Streaming 路径：accepted -> progress（可选）-> terminal 流程正确。
5. 失败路径：`action.failed` 含结构化 `error`。
6. 关联性：始终有 `request_id`；提供 `client_token` 时会回显。
7. 重放兼容：可通过 `from-seq` 消费事件。
8. 路径安全防护：traversal/盘符注入/反斜杠载荷在副作用前被拒绝。
9. 分段可移植：超长生成名被确定性 hash 缩短且 <= 255 字节。
10. 投递语义：集成测试验证消费者侧重复处理。
11. 分页句柄错误：malformed/unknown/expired/closed/cross-session 对应错误码正确。
12. 提交原子性：中断写入不产生请求或事件。
13. 并发顺序：同 action path 保持 accept 顺序，且每请求单终态。
14. 流原子性：每个提交 `seq` 下 `events`/`cursor`/`from-seq` 一致。
15. 所有事件含 `event_id` 且重放稳定。
16. 生命周期：readiness/liveness/shutdown/recovery 在集成测试中验证。
17. SDK 冻结：runtime 经冻结接口分发，兼容策略文档化。
18. CI 门禁：Linux CI 执行 static + live AppFS 合约套件。
19. 传输一致性门禁：Linux CI 在 HTTP 与 gRPC bridge 模式执行同一 live 套件。
20. 开发体验入口：第三方实现可用 quickstart + 一键 conformance + 最小模板接入。
21. Bridge 韧性基线：runtime bridge 路径提供可配置重试/退避/断路器并输出传输指标日志。

### 8.1 Phase 1 验证快照（`2026-03-16`）

使用的证据来源：

1. Build + static + live 日志：`/home/yxy/rep/agentfs/cli/appfs-phase1-validation.log`
2. Live harness：`cli/tests/appfs/run-live-with-adapter.sh`
3. Runtime 实现：`cli/src/cmd/appfs.rs`
4. Live 合约增量脚本：`cli/tests/appfs/test-streaming-lifecycle.sh`、`cli/tests/appfs/test-submit-reject.sh`、`cli/tests/appfs/test-submit-order.sh`、`cli/tests/appfs/test-paging-errors.sh`、`cli/tests/appfs/test-submit-atomicity.sh`、`cli/tests/appfs/test-submit-interrupt.sh`、`cli/tests/appfs/test-path-safety.sh`、`cli/tests/appfs/test-duplicate-consumption.sh`、`cli/tests/appfs/test-concurrent-submit-stress.sh`
5. Harness 生命周期探针：`cli/tests/appfs/run-live-with-adapter.sh`（停/启 adapter + 重启后提交）
6. SDK matrix fixture：`sdk/rust/src/appfs_adapter.rs`（`sdk_trait_required_case_matrix_is_adapter_pluggable`、`sdk_trait_error_case_matrix`）
7. CI workflow 门禁：`.github/workflows/rust.yml`（`appfs-contract-gate`）

| 条目 | 状态 | 证据 | 备注 |
|---|---|---|---|
| 1 | PASS | 验证日志中的 `CT-001/CT-005` | Manifest 节点与 schema 齐全 |
| 2 | PASS | 验证日志中的 `CT-007` | 坏 JSON 与坏 handle 在无 `action.accepted` 前被拒绝 |
| 3 | PASS | 验证日志中的 `CT-002` | Inline 动作发出终态事件 |
| 4 | PASS | 验证日志中的 `CT-006` | Streaming 发出 `accepted/progress/completed` 且单终态 |
| 5 | PASS | `CT-004` + `emit_failed` 路径 | `action.failed.error` 结构已发出 |
| 6 | PASS | `CT-002` + token 提取逻辑 | `request_id` 始终存在；支持 `client_token` 回显 |
| 7 | PASS | 验证日志中的 `CT-003` | `from-seq` 重放可用 |
| 8 | PASS | 验证日志中的 `CT-012` + `cli/src/cmd/appfs.rs` (`is_safe_action_rel_path`) | 盘符/保留名/反斜杠 unsafe 路径被拒绝且无流副作用 |
| 9 | PASS | 验证日志中的 `CT-015` + `cli/src/cmd/appfs.rs` (`normalize_runtime_handle_id`) | 超长分页 handle 被确定性缩短至 <=255 字节并保留别名查找 |
| 10 | PASS | 验证日志中的 `CT-013` | 同一事件可在 live 与 replay 被消费，要求消费者去重 |
| 11 | PASS | 验证日志中的 `CT-009` + `cli/src/cmd/appfs.rs` | malformed/unknown/expired/closed/cross-session 错误映射正确 |
| 12 | PASS | 验证日志中的 `CT-010/CT-011` + `cli/src/cmd/appfs.rs` stable-submit gate | 进行中/中断写入被覆盖，合法完整提交前无副作用 |
| 13 | PASS | 验证日志中的 `CT-014` | 并发提交压测验证每提交单终态与唯一 request_id |
| 14 | PASS | `CT-003` + 代码发布序列 | 常规发布路径下 `events/cursor/from-seq` 一致 |
| 15 | PASS | `CT-002/CT-003` + 基于 seq 的 `event_id` | `event_id` 存在且重放稳定 |
| 16 | PASS | `run-live-with-adapter.sh` 日志中的 `CT-016` + `cli/src/cmd/appfs.rs` (`inflight.jobs.res.json`) | 优雅停机/重启与 accepted-but-not-terminal 对账端到端通过 |
| 17 | PASS | `sdk/rust/src/appfs_adapter.rs` + `cli/src/cmd/appfs.rs` + `run-live-with-adapter.sh` (`CT-001` 到 `CT-016`) | Runtime 通过冻结 `AppAdapterV1` 分发业务处理，且保持 live 一致性 |
| 18 | PASS | `.github/workflows/rust.yml` (`appfs-contract-gate`) + `cli/tests/appfs/run-live-with-adapter.sh` | Linux CI 将 static + live 作为合并门禁 |
| 19 | PASS | `.github/workflows/rust.yml` (`appfs-contract-gate-http-bridge`, `appfs-contract-gate-grpc-bridge`) | CI 对 HTTP/gRPC bridge 执行同一 live 套件验证传输一致性 |
| 20 | PASS | `examples/appfs/ADAPTER-QUICKSTART.md` + `examples/appfs/run-conformance.sh` + `examples/appfs/legacy/v1/templates/rust-minimal/` | 提供最小可复现接入路径（quickstart + 一键 conformance + SDK 模板） |
| 21 | PASS | `cli/src/cmd/appfs/bridge_resilience.rs` + `cli/src/cmd/appfs/http_bridge_adapter.rs` + `cli/src/cmd/appfs/grpc_bridge_adapter.rs` + `run-live-with-adapter.sh` (`CT-017`) | Bridge 路径支持可配置重试/退避/断路器并输出指标，live 契约覆盖 retry/circuit/cooldown 恢复 |

## 9. 交付计划

### Phase 1（Core Adapter Skeleton）

1. Manifest 生成。
2. 资源读取处理器。
3. 含校验的 action 提交流水线。

### Phase 2（模式语义）

1. Inline 模式行为与超时降级。
2. Streaming 模式进度与终态保障。

### Phase 3（加固）

1. 错误映射一致性。
2. 权限检查与审批流。
3. 合约测试与性能基线。
