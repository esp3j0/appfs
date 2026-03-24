# APPFS v0.3 Connectorization ADR

- 版本：`v0.3`
- 状态：`Frozen (V3-01, 2026-03-24)`
- 决策范围：`Connectorization`
- 依赖文档：
  - [APPFS-v0.3-实施计划.zh-CN.md](./APPFS-v0.3-实施计划.zh-CN.md)
  - [APPFS-v0.3-Connector接口.zh-CN.md](./APPFS-v0.3-Connector接口.zh-CN.md)
  - [APPFS-v0.2-完成总结-2026-03-22.zh-CN.md](../v2/APPFS-v0.2-完成总结-2026-03-22.zh-CN.md)

## 1. 背景

1. `v0.2.0` 已完成 backend-native 主线与 CT2 required gate，但“真实 app 对接层”仍停留在骨架阶段。
2. 当前 shipping runtime 仍以 `AppAdapterV1` 为中心，`in-process`、`HTTP bridge`、`gRPC bridge` 只是同一套 v0.1 action/control 协议的三种 transport。
3. snapshot read-through 主路径仍保留 runtime 内部 stub 扩容逻辑，导致 bridge 路径即便通过 gate，也不能证明“真实 connector 已可接入真实软件”。
4. 当前尚未形成对外稳定 connector 兼容包袱，因此 `v0.3` 允许进行一次破坏性升级，把 connector 作为正式 shipping surface 冻结下来。

## 2. 问题定义

若继续沿用当前结构，将出现以下问题：

1. runtime 与 connector 的职责混叠，真实 app 接入时边界不清。
2. `HTTP`/`gRPC`/`in-process` 三条路径无法证明语义一致，只能证明 transport 层“能通”。
3. CI 可能被 runtime fallback 掩盖，出现“看起来全绿，但实际上没走 connector 主路径”的假阳性。
4. 无法把 connector 作为真实软件接入的工程接口发布给后续实现者。

## 3. 决策

### 3.1 统一引入 `AppConnectorV2`

`v0.3` 以新的 canonical connector surface 作为唯一主线：

1. `connector_id() -> ConnectorInfoV2`
2. `health(ctx) -> HealthStatusV2`
3. `prewarm_snapshot_meta(resource_path, timeout, ctx) -> SnapshotMetaV2`
4. `fetch_snapshot_chunk(request, ctx) -> FetchSnapshotChunkResponseV2`
5. `fetch_live_page(request, ctx) -> FetchLivePageResponseV2`
6. `submit_action(request, ctx) -> SubmitActionResponseV2`

说明：

1. `AppAdapterV1`、`/v1/submit-action`、`/v1/submit-control-action` 进入 legacy 兼容面。
2. `v0.3` runtime 默认不得再以 `AppAdapterV1` 作为主路径。
3. `HTTP bridge`、`gRPC bridge`、`in-process` 只是 `AppConnectorV2` 的三种承载形态，不得拥有各自独立语义。

### 3.2 明确职责边界

runtime 负责：

1. manifest 解析与 action/resource 路由。
2. `.act` 的 ActionLineV2 解析、submit-time reject、`request_id` 生成。
3. snapshot cache 生命周期、临时区物化、原子发布、journal/recovery。
4. live handle 生命周期、runtime handle 到 upstream cursor 的持久化映射。
5. 事件流、重放、cursor 原子性。
6. AppFS 错误面与 CT2 语义。

connector 负责：

1. 上游 app 协议访问与认证。
2. snapshot 元信息探测与 chunk 拉取。
3. live page 拉取与 upstream cursor 语义保持。
4. action 请求映射、上游错误标准化、健康状态暴露。
5. transport 无关的业务数据转换。

禁止项：

1. runtime core 中不得保留“主路径业务数据 snapshot stub”作为成功路径。
2. transport adapter 中不得补业务逻辑，只能做协议封装、重试、超时、错误映射与序列化。

### 3.3 统一错误码口径

`v0.3` connector 继续对齐 `v0.2` 的大写错误码命名，不采用 CamelCase 枚举名作为外部协议：

1. Core：`INVALID_ARGUMENT`、`INVALID_PAYLOAD`、`NOT_SUPPORTED`、`SNAPSHOT_TOO_LARGE`、`CACHE_MISS_EXPAND_FAILED`、`INTERNAL`
2. Extended：`UPSTREAM_UNAVAILABLE`、`RATE_LIMITED`、`AUTH_EXPIRED`、`PERMISSION_DENIED`、`RESOURCE_EXHAUSTED`、`TIMEOUT`
3. Connector-internal paging/cursor：`CURSOR_INVALID`、`CURSOR_EXPIRED`

边界要求：

1. connector 对上游错误做标准化。
2. runtime 可以把 `CURSOR_INVALID` / `CURSOR_EXPIRED` 再映射为 AppFS 的 `PAGER_HANDLE_*` 错误面。

### 3.4 数据一致性要求进入冻结约束

1. `fetch_snapshot_chunk` 必须返回稳定排序键与记录级去重键。
2. `fetch_live_page` 必须返回可恢复 cursor 语义，且失效时显式报错。
3. `submit_action` 必须保留 `inline` / `streaming` 执行模式语义，但通过 V2 类型承载。
4. `health` 必须可暴露认证状态，供 runtime 与 CI 验证降级行为。

## 4. 兼容性决策

### 4.1 明确破坏性升级

`v0.3` 明确打破以下兼容面：

1. `AppAdapterV1` 不再作为默认 runtime 契约。
2. HTTP bridge v1 端点不再作为 v0.3 shipping 协议。
3. gRPC bridge v1 proto 不再作为 v0.3 shipping 协议。
4. runtime 内部 snapshot expansion stub 不再允许作为 gate 成功路径。

### 4.2 保留的兼容窗口

1. `AppAdapterV1`、HTTP/gRPC v1 仅作为迁移窗口内 legacy baseline 与回归对照。
2. `APPFS_V2_*` runner/env 在迁移窗口内保留别名，后续由 `V3-10` 统一迁移到 `APPFS_V3_*`。
3. README 与 CI 在 v0.3 收口前必须同时标注“legacy path”与“shipping path”。

## 5. 影响

### 5.1 正向影响

1. 真实 app 接入的工程接口被固定下来，后续 issue 不再做接口级决策。
2. 三种 transport 将共享同一语义源，测试与 CI 更容易收口。
3. runtime fallback 会被显式排出主路径，gate 更可信。

### 5.2 成本

1. 需要改 SDK trait、runtime、HTTP bridge、gRPC bridge、demo connector 和 CT2/CI。
2. 需要在迁移窗口内同时维护 legacy baseline 与 V2 shipping path 的差异说明。

## 6. 不在本 ADR 内解决

1. 多真实 app 同时上线。
2. 大规模 rollout 策略。
3. Level 3 optional 能力的大规模扩展。
4. 长期稳定性证据本身；该项由 `V3-09` pilot 验收承担。

## 7. 落地要求

1. 文档冻结后，`V3-02` 及后续 issue 不得再修改方法集与 payload 结构；若需修改，必须 reopen `V3-01`。
2. `V3-07` 必须增加 connector call evidence 校验，确保 CI 不能被 legacy fallback 掩盖。
3. 在宣布 `v0.3 ready` 前，必须完成至少一个真实 app sandbox pilot。

## 8. 关联 Issue 映射

1. `V3-01`：ADR + V2 契约冻结
2. `V3-02`：Rust SDK V2 trait/type
3. `V3-03`：runtime 接 V2 connector
4. `V3-04`：HTTP bridge V2
5. `V3-05`：gRPC bridge V2
6. `V3-06`：demo connector parity
7. `V3-07`：CT2/CI gate 升级
8. `V3-08`：文档与 release 收口
9. `V3-09`：真实 app pilot
10. `V3-10`：runner/CI 版本语义迁移
