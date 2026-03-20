# APPFS v0.2 Connector 接口（骨架）

- 版本：`v0.2-draft`
- 状态：`Frozen (Phase A, 2026-03-20)`
- 依赖文档：
  - [APPFS-v0.2-接口规范.zh-CN.md](./APPFS-v0.2-接口规范.zh-CN.md)
  - [APPFS-v0.2-后端架构.zh-CN.md](./APPFS-v0.2-后端架构.zh-CN.md)
  - [APPFS-v0.2-能力分级.zh-CN.md](./APPFS-v0.2-能力分级.zh-CN.md)

## 1. 目标

1. 定义 v0.2 后端模式中"真实 app 对接层"的统一契约。
2. 解耦 AppFS Core 与上游 app 的传输和业务差异。
3. 支持同一接口下的多形态实现：in-process SDK、HTTP、gRPC。

## 2. 术语与定位

| 术语 | 定义 |
|------|------|
| **Core Backend** | AppFS 核心后端（读拦截、缓存、事件、journal） |
| **Connector** | 对接具体 app 的适配层（每个 app 一个实现） |
| **Transport Adapter** | Connector 的部署/调用形态（SDK 直连、HTTP、gRPC） |

> 说明：v0.2 统一使用 `Connector` 术语，避免与 v0.1 adapter/bridge 混淆。

## 3. Connector 能力接口

### 3.1 最小能力集（Core）

| 方法 | 说明 | 返回 |
|------|------|------|
| `connector_id()` | 返回连接器标识与版本 | `ConnectorInfo` |
| `capabilities()` | 声明支持能力（snapshot/live/action） | `Capabilities` |
| `prewarm_snapshot_meta(resource_path)` | 拉取 snapshot 元信息（size/revision） | `SnapshotMeta` |
| `fetch_snapshot_chunk(resource_path, from_cursor, from_offset, budget)` | 拉取快照增量块 | `FetchSnapshotChunkResponse` |
| `fetch_live_page(resource_path, handle_id, cursor, page_size)` | 拉取 live 分页 | `FetchLivePageResponse` |
| `submit_action(path, payload, context)` | 执行动作请求 | `SubmitActionResponse` |
| `health()` | 连通性与认证状态检查 | `HealthStatus` |

### 3.2 可选能力集（Optional）

| 方法 | 说明 | 返回 |
|------|------|------|
| `ack_event(event_id)` | 上游回执能力 | `AckResult` |
| `refresh_auth()` | 主动刷新令牌 | `AuthStatus` |
| `estimate_cost(request)` | 请求成本估算（限流前置） | `CostEstimate` |

## 4. 类型签名定义（Rust 伪代码）

### 4.1 基础类型

```rust
/// 资源路径（相对于 app 根目录）
pub type ResourcePath = String;

/// 游标（上游分页标识）
pub type Cursor = String;

/// 版本标识
pub type Revision = String;

/// 连接器标识
pub struct ConnectorInfo {
    pub connector_id: String,
    pub version: String,
    pub app_id: String,
    pub language: String,
}
```

### 4.2 上下文类型

```rust
/// 连接器上下文
pub struct ConnectorContext {
    pub app_id: String,
    pub session_id: String,
    pub request_id: String,
    pub client_token: Option<String>,
    pub trace_id: Option<String>,
}

/// 能力声明
pub struct Capabilities {
    pub supports_snapshot: bool,
    pub supports_live: bool,
    pub supports_action: bool,
    pub optional_features: Vec<String>,  // ["ack_event", "refresh_auth", ...]
}
```

### 4.3 Snapshot 相关类型

```rust
/// Snapshot 元信息
pub struct SnapshotMeta {
    pub size_bytes: Option<u64>,
    pub revision: Option<Revision>,
    pub last_modified: Option<DateTime<Utc>>,
    pub item_count: Option<u64>,
}

/// Snapshot 块拉取请求
pub struct FetchSnapshotChunkRequest {
    pub resource_path: ResourcePath,
    pub from_cursor: Option<Cursor>,
    pub from_offset: Option<u64>,
    pub budget_bytes: u64,  // 最大拉取字节数
}

/// Snapshot 块拉取响应
pub struct FetchSnapshotChunkResponse {
    pub lines: Vec<serde_json::Value>,  // JSONL 行数组
    pub bytes: u64,
    pub next_cursor: Option<Cursor>,
    pub has_more: bool,
    pub revision: Option<Revision>,
}
```

### 4.4 Live 分页相关类型

```rust
/// Live 分页请求
pub struct FetchLivePageRequest {
    pub resource_path: ResourcePath,
    pub handle_id: Option<String>,  // 已有句柄
    pub cursor: Option<Cursor>,
    pub page_size: u32,
}

/// Live 分页响应
pub struct FetchLivePageResponse {
    pub items: Vec<serde_json::Value>,
    pub page: LivePageInfo,
}

/// Live 分页信息
pub struct LivePageInfo {
    pub handle_id: String,
    pub page_no: u32,
    pub has_more: bool,
    pub mode: LiveMode,
    pub expires_at: Option<DateTime<Utc>>,
    pub retry_after_ms: Option<u32>,
}

/// Live 模式（v0.2 仅支持 Live）
pub enum LiveMode {
    Live,
}
```

### 4.5 Action 相关类型

```rust
/// Action 提交请求
pub struct SubmitActionRequest {
    pub path: ResourcePath,
    pub payload: serde_json::Value,
    pub context: ConnectorContext,
}

/// Action 提交响应
pub struct SubmitActionResponse {
    pub request_id: String,
    pub accepted: bool,
    pub estimated_duration_ms: Option<u32>,  // 可选：预估完成时间
}

/// Action 进度（用于 streaming 类型）
pub struct ActionProgress {
    pub request_id: String,
    pub percent: Option<u8>,
    pub stage: Option<String>,
    pub message: Option<String>,
}
```

### 4.6 错误类型

```rust
/// 连接器错误
pub struct ConnectorError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<String>,  // 上游原始错误（脱敏）
    pub retryable: bool,
}

/// 标准错误码
pub enum ErrorCode {
    // 标准错误码（Core）
    InvalidArgument,
    InvalidPayload,
    NotSupported,
    SnapshotTooLarge,
    CacheMissExpandFailed,
    Internal,

    // 扩展错误码（Optional）
    UpstreamUnavailable,
    RateLimited,
    AuthExpired,
    PermissionDenied,
    ResourceExhausted,
}
```

### 4.7 健康检查类型

```rust
/// 健康状态
pub struct HealthStatus {
    pub healthy: bool,
    pub auth_status: AuthStatus,
    pub last_check: DateTime<Utc>,
    pub message: Option<String>,
}

/// 认证状态
pub enum AuthStatus {
    Valid,
    Expired,
    Refreshing,
    Invalid,
}
```

## 5. 接口调用示例

### 5.1 Snapshot 预热

```rust
// Core Backend 调用 Connector
let meta = connector.prewarm_snapshot_meta("/chats/chat-001/messages.res.jsonl")?;

match meta {
    SnapshotMeta { size_bytes: Some(size), revision: Some(rev), .. } => {
        // 初始化缓存状态
        cache_manager.init_resource(path, size, rev);
    }
    _ => {
        // 无法获取元信息，标记为 cold
        cache_manager.set_state(path, CacheState::Cold);
    }
}
```

### 5.2 Snapshot 块拉取

```rust
let request = FetchSnapshotChunkRequest {
    resource_path: "/chats/chat-001/messages.res.jsonl".to_string(),
    from_cursor: Some("cursor_001".to_string()),
    from_offset: None,
    budget_bytes: 1024 * 1024,  // 1MB
};

let response = connector.fetch_snapshot_chunk(request)?;

// 物化到缓存
for line in response.lines {
    cache.append_line(path, line)?;
}

if response.has_more {
    // 继续拉取
    let next_request = FetchSnapshotChunkRequest {
        from_cursor: response.next_cursor,
        ..
    };
}
```

### 5.3 Action 提交

```rust
let request = SubmitActionRequest {
    path: "/contacts/zhangsan/send_message.act".to_string(),
    payload: json!({"text": "hello"}),
    context: ConnectorContext {
        app_id: "aiim".to_string(),
        session_id: "sess_001".to_string(),
        request_id: "req_001".to_string(),
        client_token: Some("msg_001".to_string()),
        trace_id: None,
    },
};

let response = connector.submit_action(request)?;

if response.accepted {
    // 等待事件流返回结果
} else {
    // 立即返回错误
}
```

## 6. 错误码映射要求

Connector 必须把上游错误映射到统一最小集：

| 上游场景 | 映射错误码 | retryable |
|----------|------------|-----------|
| 参数格式错误 | `InvalidArgument` | false |
| payload 不满足 schema | `InvalidPayload` | false |
| 操作不支持 | `NotSupported` | false |
| 数据超限 | `SnapshotTooLarge` | false |
| 上游服务不可用 | `UpstreamUnavailable` | true |
| 被限流 | `RateLimited` | true |
| 认证过期 | `AuthExpired` | false |
| 权限不足 | `PermissionDenied` | false |
| 资源耗尽 | `ResourceExhausted` | true |
| 内部错误 | `Internal` | true |

**补充要求**：
1. 必须保留上游原始错误信息到 `details`（脱敏后）。
2. 必须显式标注 `retryable`，供 Core 决策重试策略。

## 7. 与 Core Backend 的边界

### 7.1 Core 负责

| 职责 | 说明 |
|------|------|
| `.act` JSONL 解析 | 解析 ActionLineV2，校验格式 |
| 提交边界 | 换行符为提交边界，处理中断恢复 |
| 读拦截 | 拦截读请求，判断缓存命中/miss |
| 缓存状态机 | 管理 cold/warming/hot/partial/stale/error 状态 |
| 原子发布 | 缓存扩展使用 tmp + rename |
| 事件写入 | 写入事件流，保证顺序性 |
| replay/cursor | 事件重放，游标管理 |
| journal | 持久化请求状态，支持恢复 |

### 7.2 Connector 负责

| 职责 | 说明 |
|------|------|
| 上游 API/SDK 调用 | 调用具体 app 的 API |
| 协议转换 | REST/gRPC/SDK 转换为统一模型 |
| 分页/增量映射 | 上游分页模型向 Core 统一模型映射 |
| 认证 | 处理认证、令牌刷新 |
| 限流 | 上游限流处理，返回 retryable |
| 错误标准化 | 上游错误码映射到标准错误码 |
| 脱敏 | 敏感字段脱敏后返回 |

## 8. 传输形态要求

### 8.1 支持的部署形态

| 形态 | 说明 | 推荐场景 |
|------|------|----------|
| **in-process SDK** | 直接链接，延迟最低 | 推荐，生产环境 |
| **HTTP Connector Service** | HTTP REST API | 多语言对接 |
| **gRPC Connector Service** | gRPC 协议 | 高性能场景 |

### 8.2 一致性要求

1. 不同传输形态的语义必须一致。
2. CT2 同一用例在不同传输下预期一致。
3. 类型签名可通过 OpenAPI/Protobuf 描述。

## 9. 对接任意真实 app 的兼容要点

| 要点 | 说明 |
|------|------|
| 字段映射 | 不强制上游字段命名，Connector 内部映射 |
| 分页转换 | 上游"分页 API"转换成 snapshot 增量物化输入 |
| 无限流转换 | 上游"无限流"转换成 live 分页输入 |
| ID 映射 | 支持上游强一致 ID 或弱一致 ID 的稳定映射策略 |
| 认证适配 | 支持 OAuth/API Key/自定义认证 |

## 10. 约束

1. 本文定义接口骨架和类型签名，实现者可根据语言特性调整。
2. 不限制上游鉴权方案，但必须可声明认证状态。
3. 任何破坏性接口改动必须走 v0.2 文档评审流程。

## 11. 验收

1. 任一实现者可据此完成 Connector 最小实现。
2. 任一 app 可通过 Connector 接入 v0.2 Core，而无需改 Core 协议。
3. 可直接映射到 CT2 与后续真实 app 认证用例。

## 12. 关联文档

1. [总览](./APPFS-v0.2-总览.zh-CN.md)
2. [接口规范](./APPFS-v0.2-接口规范.zh-CN.md)
3. [后端架构](./APPFS-v0.2-后端架构.zh-CN.md)
4. [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)
5. [真实 App 对接规范](./APPFS-v0.2-真实App对接规范.zh-CN.md)
