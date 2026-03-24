# APPFS v0.3 Connector 接口

- 版本：`v0.3`
- 状态：`Frozen (V3-01, 2026-03-24)`
- 依赖文档：
  - [APPFS-v0.3-Connectorization-ADR.zh-CN.md](./APPFS-v0.3-Connectorization-ADR.zh-CN.md)
  - [APPFS-v0.3-实施计划.zh-CN.md](./APPFS-v0.3-实施计划.zh-CN.md)
  - [APPFS-v0.2-接口规范.zh-CN.md](../v2/APPFS-v0.2-接口规范.zh-CN.md)

## 1. 目标

1. 冻结 `v0.3` shipping connector 的 canonical contract。
2. 保证 `in-process`、`HTTP bridge`、`gRPC bridge` 三条路径的有效语义一致。
3. 让后续实现者可直接围绕本文编码，不再做接口级决策。

## 2. 兼容声明

1. 本文是 `v0.3` 的破坏性升级接口。
2. `AppAdapterV1`、HTTP bridge v1、gRPC bridge v1 仅保留为 legacy baseline，不属于 `v0.3` shipping surface。
3. `v0.3` runtime 默认必须走本文定义的 V2 connector 契约。

## 3. 术语

| 术语 | 定义 |
|------|------|
| `runtime` | AppFS 核心运行时，负责文件语义、缓存、事件、recovery |
| `connector` | 对接真实 app 的统一契约实现 |
| `transport adapter` | `in-process` / `HTTP` / `gRPC` 的承载封装层 |
| `record_key` | snapshot 记录级唯一键，用于 dedupe |
| `ordering_key` | snapshot 稳定排序键，用于断点续传与恢复 |
| `upstream cursor` | live 分页时由 connector 保存或返回的上游游标 |

## 4. 职责边界

### 4.1 runtime 负责

1. ActionLineV2 解析、submit-time reject、`request_id` 生成。
2. snapshot cache 生命周期、物化、原子发布、journal 与 recovery。
3. live handle 生命周期与 runtime handle 持久化。
4. 事件流、重放、CT2 语义与 AppFS error surface。

### 4.2 connector 负责

1. 上游协议访问、认证与数据映射。
2. snapshot 元信息与 chunk 获取。
3. live page 获取、cursor 演进与失效错误识别。
4. action 提交语义、健康检查与上游错误标准化。

### 4.3 明确禁止

1. runtime core 不得生成业务数据 snapshot stub 作为成功路径。
2. transport adapter 不得补业务数据，只能补 transport 相关逻辑。
3. connector 不得绕过 runtime 直接写 AppFS event stream 或 cache 文件。

## 5. Canonical Trait

Rust 伪代码如下：

```rust
pub trait AppConnectorV2: Send {
    fn connector_id(&self) -> Result<ConnectorInfoV2, ConnectorErrorV2>;

    fn health(
        &mut self,
        ctx: &ConnectorContextV2,
    ) -> Result<HealthStatusV2, ConnectorErrorV2>;

    fn prewarm_snapshot_meta(
        &mut self,
        resource_path: &str,
        timeout: std::time::Duration,
        ctx: &ConnectorContextV2,
    ) -> Result<SnapshotMetaV2, ConnectorErrorV2>;

    fn fetch_snapshot_chunk(
        &mut self,
        request: FetchSnapshotChunkRequestV2,
        ctx: &ConnectorContextV2,
    ) -> Result<FetchSnapshotChunkResponseV2, ConnectorErrorV2>;

    fn fetch_live_page(
        &mut self,
        request: FetchLivePageRequestV2,
        ctx: &ConnectorContextV2,
    ) -> Result<FetchLivePageResponseV2, ConnectorErrorV2>;

    fn submit_action(
        &mut self,
        request: SubmitActionRequestV2,
        ctx: &ConnectorContextV2,
    ) -> Result<SubmitActionResponseV2, ConnectorErrorV2>;
}
```

冻结说明：

1. 方法集在 `V3-01` 冻结。
2. 后续实现只允许向后兼容字段追加，不允许删除、重命名、改语义。

## 6. 基础类型

### 6.1 ConnectorInfoV2

```rust
pub struct ConnectorInfoV2 {
    pub connector_id: String,
    pub version: String,
    pub app_id: String,
    pub transport: ConnectorTransportV2,
    pub supports_snapshot: bool,
    pub supports_live: bool,
    pub supports_action: bool,
    pub optional_features: Vec<String>,
}

pub enum ConnectorTransportV2 {
    InProcess,
    HttpBridge,
    GrpcBridge,
}
```

约束：

1. `connector_id` 在同一 connector 实现内必须稳定。
2. `optional_features` 仅用于声明附加能力，不得替代 core 方法。

### 6.2 ConnectorContextV2

```rust
pub struct ConnectorContextV2 {
    pub app_id: String,
    pub session_id: String,
    pub request_id: String,
    pub client_token: Option<String>,
    pub trace_id: Option<String>,
}
```

约束：

1. `request_id` 由 runtime 生成。
2. `client_token` 由 ActionLineV2 提供时透传。

## 7. Health 接口

```rust
pub struct HealthStatusV2 {
    pub healthy: bool,
    pub auth_status: AuthStatusV2,
    pub message: Option<String>,
    pub checked_at: String,
}

pub enum AuthStatusV2 {
    Valid,
    Expired,
    Refreshing,
    Invalid,
}
```

约束：

1. `health` 必须可区分连通性失败与认证失败。
2. `HTTP` / `gRPC` transport 的 retry/backoff/circuit 行为必须以 `health` 或真实请求失败证据可验证。

## 8. Snapshot 接口

### 8.1 SnapshotMetaV2

```rust
pub struct SnapshotMetaV2 {
    pub size_bytes: Option<u64>,
    pub revision: Option<String>,
    pub last_modified: Option<String>,
    pub item_count: Option<u64>,
}
```

约束：

1. `prewarm_snapshot_meta` 只负责探测元信息，不负责物化数据。
2. timeout 由 runtime 传入，connector 必须按 timeout 约束返回成功或 `TIMEOUT`。

### 8.2 FetchSnapshotChunkRequestV2

```rust
pub enum SnapshotResumeV2 {
    Start,
    Cursor(String),
    Offset(u64),
}

pub struct FetchSnapshotChunkRequestV2 {
    pub resource_path: String,
    pub resume: SnapshotResumeV2,
    pub budget_bytes: u64,
}
```

### 8.3 FetchSnapshotChunkResponseV2

```rust
pub struct SnapshotRecordV2 {
    pub record_key: String,
    pub ordering_key: String,
    pub line: serde_json::Value,
}

pub struct FetchSnapshotChunkResponseV2 {
    pub records: Vec<SnapshotRecordV2>,
    pub emitted_bytes: u64,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub revision: Option<String>,
}
```

冻结约束：

1. `line` 必须是可序列化为单行 JSONL 的对象。
2. `record_key` 必须在同一 `resource_path + revision` 范围内唯一。
3. `ordering_key` 必须可稳定排序，且同一请求重试时不得改变相对顺序。
4. `resume=Cursor(x)` 时，connector 必须从 `x` 之后继续，不得静默重复或漏掉记录。
5. `resume=Offset(n)` 仅允许在上游确有稳定偏移语义时使用；否则应返回 `NOT_SUPPORTED`。
6. `emitted_bytes` 必须等于本次 `records` 实际可物化字节数，不得虚报。

runtime 约束：

1. runtime 根据 `records` 物化 JSONL 文件。
2. runtime 可使用 `record_key` / `ordering_key` 做 journal 与 dedupe，但不得把它们写入 agent 可见的 snapshot 文件。

## 9. Live 接口

### 9.1 FetchLivePageRequestV2

```rust
pub struct FetchLivePageRequestV2 {
    pub resource_path: String,
    pub handle_id: Option<String>,
    pub cursor: Option<String>,
    pub page_size: u32,
}
```

### 9.2 FetchLivePageResponseV2

```rust
pub struct LivePageInfoV2 {
    pub handle_id: String,
    pub page_no: u32,
    pub has_more: bool,
    pub mode: LiveModeV2,
    pub expires_at: Option<String>,
    pub next_cursor: Option<String>,
    pub retry_after_ms: Option<u32>,
}

pub enum LiveModeV2 {
    Live,
}

pub struct FetchLivePageResponseV2 {
    pub items: Vec<serde_json::Value>,
    pub page: LivePageInfoV2,
}
```

冻结约束：

1. `mode` 在 `v0.3` 固定为 `live`。
2. `page.handle_id` 是 runtime 侧 handle 的 canonical id；connector 不得在一次分页链路中无故变更该值。
3. `page.next_cursor` 用于 runtime 持久化 upstream cursor，不要求直接暴露给 agent。
4. 同一 `cursor` 的重试必须满足幂等读取预期。
5. upstream cursor 失效必须显式返回 `CURSOR_INVALID` 或 `CURSOR_EXPIRED`，不得静默回退到第一页。

## 10. Action 接口

### 10.1 SubmitActionRequestV2

```rust
pub enum ActionExecutionModeV2 {
    Inline,
    Streaming,
}

pub struct SubmitActionRequestV2 {
    pub path: String,
    pub payload: serde_json::Value,
    pub execution_mode: ActionExecutionModeV2,
}
```

### 10.2 SubmitActionResponseV2

```rust
pub struct ActionStreamingPlanV2 {
    pub accepted_content: Option<serde_json::Value>,
    pub progress_content: Option<serde_json::Value>,
    pub terminal_content: serde_json::Value,
}

pub enum SubmitActionOutcomeV2 {
    Completed {
        content: serde_json::Value,
    },
    Streaming {
        plan: ActionStreamingPlanV2,
    },
}

pub struct SubmitActionResponseV2 {
    pub request_id: String,
    pub estimated_duration_ms: Option<u32>,
    pub outcome: SubmitActionOutcomeV2,
}
```

冻结约束：

1. `payload` 必须与 ActionLineV2 中的 `payload` 对象一一对应。
2. `execution_mode` 由 runtime 基于 manifest 决定并传入 connector。
3. `Completed` 表示 runtime 可直接发 `action.completed`。
4. `Streaming` 表示 runtime 可按计划发 `action.accepted`、可选 `action.progress`、最终 `action.completed`。
5. connector 不得跳过 runtime 直接写事件。

## 11. 错误类型

```rust
pub struct ConnectorErrorV2 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Option<String>,
}
```

### 11.1 冻结错误码集合

| 错误码 | 语义 | retryable |
|--------|------|-----------|
| `INVALID_ARGUMENT` | 参数格式错误 | false |
| `INVALID_PAYLOAD` | payload/schema 不合法 | false |
| `NOT_SUPPORTED` | connector 不支持该能力或 resume 语义 | false |
| `SNAPSHOT_TOO_LARGE` | 单次或目标物化超限 | false |
| `CACHE_MISS_EXPAND_FAILED` | snapshot 扩容失败 | true |
| `INTERNAL` | 未分类内部错误 | true |
| `UPSTREAM_UNAVAILABLE` | 上游服务不可用 | true |
| `RATE_LIMITED` | 上游限流 | true |
| `AUTH_EXPIRED` | 认证过期 | false |
| `PERMISSION_DENIED` | 权限不足 | false |
| `RESOURCE_EXHAUSTED` | 上游或 connector 资源耗尽 | true |
| `TIMEOUT` | connector 或上游超时 | true |
| `CURSOR_INVALID` | live cursor 非法 | false |
| `CURSOR_EXPIRED` | live cursor 过期 | false |

### 11.2 映射要求

1. 必须保留脱敏后的 `details`。
2. transport 层失败若无法解析上游错误，应映射为 `INTERNAL` 或 `UPSTREAM_UNAVAILABLE`，不得 silently swallow。
3. runtime 可把 `CURSOR_INVALID` / `CURSOR_EXPIRED` 再映射到 AppFS `PAGER_HANDLE_*` 错误面。

## 12. HTTP Bridge V2 映射

### 12.1 端点

1. `POST /v2/connector/info`
2. `POST /v2/connector/health`
3. `POST /v2/connector/snapshot/prewarm`
4. `POST /v2/connector/snapshot/fetch-chunk`
5. `POST /v2/connector/live/fetch-page`
6. `POST /v2/connector/action/submit`

### 12.2 请求包装

除 `info` 外，HTTP bridge V2 统一使用如下包装：

```json
{
  "context": {
    "app_id": "aiim",
    "session_id": "sess-001",
    "request_id": "req-001",
    "client_token": "tok-001",
    "trace_id": "trace-001"
  },
  "request": {}
}
```

特殊情况：

1. `snapshot/prewarm` 的 `request` 形状为 `{ "resource_path": "...", "timeout_ms": 5000 }`
2. `health` 只需要 `{ "context": { ... } }`

### 12.3 响应约束

1. 成功返回 `200` + 对应 V2 payload。
2. 失败返回非 `2xx` + `ConnectorErrorV2` JSON。
3. 不再使用 `/v1/submit-action` 与 `/v1/submit-control-action` 作为 v0.3 主路径。

## 13. gRPC Bridge V2 映射

### 13.1 服务名与 RPC

```proto
service AppfsConnectorV2 {
  rpc GetConnectorInfo(GetConnectorInfoRequest) returns (GetConnectorInfoResponse);
  rpc Health(HealthRequest) returns (HealthResponse);
  rpc PrewarmSnapshotMeta(PrewarmSnapshotMetaRequest) returns (PrewarmSnapshotMetaResponse);
  rpc FetchSnapshotChunk(FetchSnapshotChunkRequest) returns (FetchSnapshotChunkResponse);
  rpc FetchLivePage(FetchLivePageRequest) returns (FetchLivePageResponse);
  rpc SubmitAction(SubmitActionRequest) returns (SubmitActionResponse);
}
```

### 13.2 约束

1. proto message 字段必须与本文 JSON 字段一一对应。
2. gRPC bridge 不得保留仅存在于 v1 proto 的 `SubmitControlAction` 主路径。
3. transport 级状态码与 payload 内 `ConnectorErrorV2` 的映射必须稳定，可在 CI 验证。

## 14. 运行与测试要求

1. `in-process`、`HTTP`、`gRPC` 三条路径必须通过同一组 V2 语义断言。
2. CT2/CI 必须校验 connector call evidence，避免 runtime fallback 掩盖失败。
3. demo connector 必须覆盖 `info + health + prewarm + snapshot chunk + live page + action submit` 全能力面。

## 15. 关联文档

1. [APPFS-v0.3-Connectorization-ADR.zh-CN.md](./APPFS-v0.3-Connectorization-ADR.zh-CN.md)
2. [APPFS-v0.3-实施计划.zh-CN.md](./APPFS-v0.3-实施计划.zh-CN.md)
3. [APPFS-v0.2-接口规范.zh-CN.md](../v2/APPFS-v0.2-接口规范.zh-CN.md)
