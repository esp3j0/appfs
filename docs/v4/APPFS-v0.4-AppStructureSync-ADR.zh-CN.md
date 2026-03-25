# APPFS v0.4 App Structure Sync ADR

- 版本：`v0.4`
- 状态：`Frozen (A1, 2026-03-25)`
- 决策范围：`App structure sync + multi-app runtime`
- 依赖文档：
  - [APPFS-v0.4-Connector结构接口.zh-CN.md](./APPFS-v0.4-Connector结构接口.zh-CN.md)
  - [2026-03-25-app-structure-sync-and-multi-app.md](../plans/2026-03-25-app-structure-sync-and-multi-app.md)
  - [APPFS-v0.3-Connectorization-ADR.zh-CN.md](../v3/APPFS-v0.3-Connectorization-ADR.zh-CN.md)

## 1. 背景

1. `v0.3` 已完成 connectorization 主线，snapshot/live/action 已通过统一 connector 主路径承载。
2. 当前 app 目录结构仍主要依赖本地 fixture / `--base` 与静态 `manifest.res.json`。
3. 真实软件接入时，目录结构往往依赖当前页面、模块、权限或服务端状态，不能假设在挂载前已完整存在。
4. 当前单 app 模式也不足以承载“一个挂载根下同时暴露多个 app”的目标。

## 2. 问题定义

若继续沿用现状，将出现以下问题：

1. app 结构的真相源不清晰：一部分来自 connector，一部分来自本地 fixture。
2. 页面切换后新增/移除节点没有统一刷新语义。
3. runtime 无法判断哪些路径可以被 connector 刷新删除，哪些必须保留。
4. 多 app 共挂载时，结构状态、session、journal 与 cache 容易混淆。
5. 若先做 Windows 原生 overlay，再回头设计动态结构刷新，容易把底层做对、上层语义做错。

## 3. 决策

### 3.1 结构真相源

`connector` 成为 **connector-owned app structure** 的真相源。

runtime 继续作为以下内容的真相源：

1. `_stream`、paging、snapshot materialization、journal、recovery 等 runtime-owned 文件。
2. agent / user 在显式允许前缀中的本地写入。
3. connector 结构同步过程中的 staging / publish 状态。

### 3.2 引入 `AppTreeSyncService`

`v0.4` 新增 runtime 内部服务 `AppTreeSyncService`，负责：

1. 调用 connector 获取 app 结构。
2. 验证结构 payload。
3. 把 connector-owned 结构 reconcile 到 AgentFS。
4. 维护 per-app `revision`、`active_scope`、同步 journal。
5. 对初始化、页面切换、显式刷新和恢复路径提供统一入口。

禁止项：

1. connector 不得直接写 AppFS tree。
2. connector 不得直接写 `_stream`、snapshot 文件、paging 状态。
3. 不得依赖平台特定 host overlay 才能完成结构同步。

### 3.3 Ownership 模型冻结

`/app root` 下的可见路径分三类：

1. `connector-owned`
2. `runtime-owned`
3. `agent-owned`

约束：

1. structure refresh 只能增删改 `connector-owned` 节点。
2. `runtime-owned` 前缀永远不可被 connector refresh 删除。
3. `agent-owned` 内容只有在显式声明安全策略时才允许被 reconcile 覆盖或清理。

### 3.4 初始化与页面刷新

首版 shipping 路径采用显式触发，不做隐式推断：

1. `initialize`：获取 app 初始结构并发布。
2. `enter_scope`：进入页面/模块时刷新结构。
3. `refresh`：显式重拉当前结构。
4. `recover`：重启后根据 journal/revision 恢复或重试。

不在首版解决：

1. 任意 action 成功后自动猜测页面切换。
2. 基于普通文件读取自动推断 scope 跳转。

### 3.5 多 app 命名空间

单个挂载根下允许多个 app 作为并列目录存在：

```text
/aiim
/notion
/slack
```

为此新增 `AppRuntimeSupervisor`，负责：

1. 维护多 app 的 runtime 实例。
2. 按 `app_id` 隔离 structure revision、session、journal、snapshot/live state。
3. 路由多 app connector 调用与 evidence。

### 3.6 Windows 原生 overlay 的排序

`Windows-native overlay parity` 是后续基础设施项，但不是本阶段前置条件。

原因：

1. 结构同步直接写入 AgentFS，可跨平台工作。
2. 本阶段的关键问题是 structure ownership 与 reconciliation，不是 host overlay。
3. 若先做 Windows overlay，对多 app / page-scope refresh 仍然没有帮助。

结论：

1. 先做 `AppTreeSyncService + connector structure contract + multi-app runtime`。
2. 之后再做 Windows 原生 overlay，用于 generic `--base` 语义对齐与替换 hydration workaround。

## 4. 兼容性决策

### 4.1 不修改 `AppConnectorV2`

`v0.3` 已冻结 `AppConnectorV2` 方法集。本阶段不 reopen `V3-01`，而是引入新契约：

`AppConnectorV3`

理由：

1. `v0.3` 文档已明确方法集冻结。
2. 结构同步是新的 shipping surface，不应偷偷塞进 V2。
3. transport 协议升级、CI 与 pilot 更容易按版本切分。

### 4.2 迁移窗口

1. `AppConnectorV2` 继续支撑当前单 app 静态结构路径。
2. `AppConnectorV3` 承担动态结构与多 app 路径。
3. 在 `v0.4` 收口前，README 与实现应明确区分：
   - `v0.3 shipping path`
   - `v0.4 structure-sync path`

## 5. 影响

### 5.1 正向影响

1. app 目录结构不再依赖静态 fixture。
2. 页面/模块驱动的目录刷新有正式协议。
3. 多 app 共挂载拥有统一运行时模型。
4. 平台差异被控制在 mount/backend 层，structure 语义保持统一。

### 5.2 成本

1. 需要新增 connector V3 类型、bridge 协议与 reconciler。
2. 需要为 ownership / journal / recovery 写新合同测试。
3. 需要增加 supervisor 层，而不是仅靠单 app runtime 拼接。

## 6. 不在本 ADR 内解决

1. 自动 page inference 策略。
2. 复杂权限模型与 tenant sharing。
3. 所有平台的 host overlay parity。
4. 大规模 app catalog 管理与 discovery UI。

## 7. 落地要求

1. `A2` 必须冻结 `AppConnectorV3` 的结构接口与 payload。
2. `A3` 必须先做单 app `AppTreeSyncService`，不提前做多 app supervisor。
3. `A4` 的首版触发仅支持 `initialize / enter_scope / refresh / recover`。
4. `A6` 多 app supervisor 合入前，必须已有 per-app isolation 测试。
5. `A7` Windows 原生 overlay parity 不得反向改变 structure-sync ownership 语义。
