# APPFS v0.4 Connector 结构接口

- 版本：`v0.4`
- 状态：`Frozen (A2, 2026-03-25)`
- 依赖文档：
  - [APPFS-v0.4-AppStructureSync-ADR.zh-CN.md](./APPFS-v0.4-AppStructureSync-ADR.zh-CN.md)
  - [APPFS-v0.3-Connector接口.zh-CN.md](../v3/APPFS-v0.3-Connector接口.zh-CN.md)

## 1. 目标

1. 冻结动态 app 结构同步的 canonical connector contract。
2. 让 runtime 能在初始化与页面切换时从 connector 获取结构并 reconcile 到 AgentFS。
3. 为后续 multi-app runtime 提供稳定的 per-app structure surface。

## 2. 版本决策

本阶段引入新契约 `AppConnectorV3`，而不是修改 `AppConnectorV2`。

原因：

1. `v0.3` 的 `AppConnectorV2` 方法集已冻结。
2. 结构同步是新的 shipping surface，应通过显式版本升级承载。

## 3. 职责边界

### 3.1 runtime 负责

1. 调用结构接口。
2. 结构 payload 校验。
3. connector-owned 路径 reconcile。
4. structure revision / active scope / journal / recovery。
5. runtime-owned 路径保护。

### 3.2 connector 负责

1. 返回 app 当前结构快照。
2. 页面/模块切换时返回新的结构快照。
3. 提供稳定 revision 与 scope 语义。
4. 标识 connector-owned 节点及其 contract metadata。

### 3.3 明确禁止

1. connector 不得直接写入 AppFS 文件。
2. connector 不得返回 snapshot 业务数据本体来替代 `fetch_snapshot_chunk`。
3. connector 不得删除 runtime-owned 前缀。

## 4. Canonical Trait

```rust
pub trait AppConnectorV3: Send {
    fn connector_id(&self) -> Result<ConnectorInfoV3, ConnectorErrorV3>;

    fn health(
        &mut self,
        ctx: &ConnectorContextV3,
    ) -> Result<HealthStatusV3, ConnectorErrorV3>;

    fn get_app_structure(
        &mut self,
        request: GetAppStructureRequestV3,
        ctx: &ConnectorContextV3,
    ) -> Result<GetAppStructureResponseV3, ConnectorErrorV3>;

    fn refresh_app_structure(
        &mut self,
        request: RefreshAppStructureRequestV3,
        ctx: &ConnectorContextV3,
    ) -> Result<RefreshAppStructureResponseV3, ConnectorErrorV3>;

    fn prewarm_snapshot_meta(
        &mut self,
        resource_path: &str,
        timeout: std::time::Duration,
        ctx: &ConnectorContextV3,
    ) -> Result<SnapshotMetaV3, ConnectorErrorV3>;

    fn fetch_snapshot_chunk(
        &mut self,
        request: FetchSnapshotChunkRequestV3,
        ctx: &ConnectorContextV3,
    ) -> Result<FetchSnapshotChunkResponseV3, ConnectorErrorV3>;

    fn fetch_live_page(
        &mut self,
        request: FetchLivePageRequestV3,
        ctx: &ConnectorContextV3,
    ) -> Result<FetchLivePageResponseV3, ConnectorErrorV3>;

    fn submit_action(
        &mut self,
        request: SubmitActionRequestV3,
        ctx: &ConnectorContextV3,
    ) -> Result<SubmitActionResponseV3, ConnectorErrorV3>;
}
```

冻结说明：

1. `V3` 继承 `v0.3` 的 snapshot/live/action/health 语义。
2. 本阶段新增的唯一方法面是 `get_app_structure` 与 `refresh_app_structure`。

## 5. 基础类型

### 5.1 ConnectorInfoV3

```rust
pub struct ConnectorInfoV3 {
    pub connector_id: String,
    pub version: String,
    pub app_id: String,
    pub transport: ConnectorTransportV3,
    pub supports_structure_sync: bool,
    pub supports_snapshot: bool,
    pub supports_live: bool,
    pub supports_action: bool,
    pub optional_features: Vec<String>,
}
```

约束：

1. `supports_structure_sync` 为 `v0.4` 必需能力。
2. 多 app supervisor 仍按 `app_id` 一实例处理，不要求一个 connector 同时返回多个 app。

### 5.2 ConnectorContextV3

```rust
pub struct ConnectorContextV3 {
    pub app_id: String,
    pub session_id: String,
    pub request_id: String,
    pub client_token: Option<String>,
    pub trace_id: Option<String>,
}
```

与 `v0.3` 保持一致，不新增多 app 复合上下文。

## 6. 结构同步接口

### 6.1 Request 类型

```rust
pub struct GetAppStructureRequestV3 {
    pub app_id: String,
    pub known_revision: Option<String>,
}

pub enum AppStructureSyncReasonV3 {
    Initialize,
    EnterScope,
    Refresh,
    Recover,
}

pub struct RefreshAppStructureRequestV3 {
    pub app_id: String,
    pub known_revision: Option<String>,
    pub reason: AppStructureSyncReasonV3,
    pub target_scope: Option<String>,
    pub trigger_action_path: Option<String>,
}
```

约束：

1. `known_revision` 用于 no-op / unchanged 响应。
2. `target_scope` 是 connector 定义的页面/模块标识。
3. `trigger_action_path` 仅作为诊断和 connector hint，不得取代 `target_scope`。

### 6.2 Response 类型

```rust
pub struct AppStructureSnapshotV3 {
    pub app_id: String,
    pub revision: String,
    pub active_scope: Option<String>,
    pub ownership_prefixes: Vec<String>,
    pub nodes: Vec<AppStructureNodeV3>,
}

pub enum AppStructureNodeKindV3 {
    Directory,
    ActionFile,
    SnapshotResource,
    LiveResource,
    StaticJsonResource,
}

pub struct AppStructureNodeV3 {
    pub path: String,
    pub kind: AppStructureNodeKindV3,
    pub manifest_entry: Option<serde_json::Value>,
    pub seed_content: Option<serde_json::Value>,
    pub mutable: bool,
    pub scope: Option<String>,
}

pub enum AppStructureSyncResultV3 {
    Unchanged {
        app_id: String,
        revision: String,
        active_scope: Option<String>,
    },
    Snapshot(AppStructureSnapshotV3),
}

pub struct GetAppStructureResponseV3 {
    pub result: AppStructureSyncResultV3,
}

pub struct RefreshAppStructureResponseV3 {
    pub result: AppStructureSyncResultV3,
}
```

## 7. 结构语义约束

### 7.1 路径约束

1. `path` 必须是 app-root 相对路径，例如 `chats/chat-001/messages.res.jsonl`。
2. connector 不得返回绝对路径。
3. connector 不得返回 `..`、空段或平台保留前缀逃逸。

### 7.2 ownership 约束

1. `ownership_prefixes` 定义本次 snapshot 允许 runtime reconcile/prune 的 connector-owned 前缀。
2. runtime 必须保护内部前缀：
   - `_stream`
   - `_paging`
   - snapshot temp/journal/runtime control 前缀
3. connector 不得把 runtime internal prefix 声明为自身 ownership。

### 7.3 manifest 约束

1. `manifest_entry` 是单节点 contract 片段。
2. runtime 负责把节点集合组装成最终 `_meta/manifest.res.json`。
3. connector 不直接返回完整 manifest 文件字节。

### 7.4 seed content 约束

1. `seed_content` 仅用于小型 bootstrap 内容。
2. `SnapshotResource` 不得通过 `seed_content` 返回完整 snapshot 数据。
3. 大型或连续数据仍必须走 snapshot/live 专用接口。

### 7.5 revision 约束

1. 同一结构语义下 `revision` 必须稳定。
2. 若 `known_revision` 未变化，connector 应优先返回 `Unchanged`。
3. connector 不得在无结构变化时随机生成新 revision。

## 8. 错误类型

```rust
pub struct ConnectorErrorV3 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Option<String>,
}
```

新增错误码：

| 错误码 | 语义 | retryable |
|--------|------|-----------|
| `STRUCTURE_INVALID` | 结构 payload 不合法 | false |
| `STRUCTURE_SCOPE_INVALID` | 请求的 scope 不存在或不合法 | false |
| `STRUCTURE_REVISION_CONFLICT` | 已知 revision 与上游状态冲突 | true |
| `STRUCTURE_SYNC_FAILED` | 结构获取失败 | true |

保留 `v0.3` 既有错误码集合用于 health/snapshot/live/action。

## 9. 运行约束

1. `get_app_structure` 必须支持初始冷启动。
2. `refresh_app_structure` 必须支持显式 scope refresh。
3. runtime 必须基于 `revision` 做 no-op 优化。
4. runtime 必须对 structure sync 做 journal/recovery。

## 10. Multi-App 约束

1. `AppConnectorV3` 仍是 per-app 契约，单次请求只处理一个 `app_id`。
2. 多 app 由 runtime supervisor 组合，不由 connector 返回“全局树”。
3. 不在 `v0.4` 首版引入 `list_apps()` 或 app discovery 协议。

## 11. HTTP / gRPC 映射要求

新增 transport 端点或 RPC：

1. `POST /v3/connector/structure/get`
2. `POST /v3/connector/structure/refresh`
3. `rpc GetAppStructure(...)`
4. `rpc RefreshAppStructure(...)`

要求：

1. 字段与本文一一对应。
2. 不允许 transport 自行扩展结构语义。
3. `Unchanged` 与 `Snapshot` 两类结果都必须可在协议层无歧义表示。

## 12. 落地要求

1. `A3` 前不得修改本文方法集与 payload 形状。
2. `A4` 必须以显式 scope refresh 为首版，不得偷渡自动 page inference。
3. `A6` 多 app supervisor 必须复用本文 per-app 结构接口，不得另开多 app 私有协议。
