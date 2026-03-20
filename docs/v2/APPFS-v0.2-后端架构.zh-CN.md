# APPFS v0.2 后端架构（Backend-native）

- 版本：`v0.2-draft`
- 状态：`Frozen (Phase A, 2026-03-20)`
- 依赖文档：[接口规范](./APPFS-v0.2-接口规范.zh-CN.md), [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)

## 1. 目标

1. 固定 v0.2 组件边界与数据流。
2. 支持 snapshot（全文件）与 live（分页）双语义并存。
3. 为任意 app 连接器提供统一执行骨架。

## 2. 核心组件

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           AppFS v0.2 Core Backend                       │
├─────────────────────────────────────────────────────────────────────────┤
│  ┌─────────────────┐    ┌─────────────────────┐    ┌─────────────────┐  │
│  │ Read Interceptor│───>│Snapshot Cache Manager│<──>│ Journal/State   │  │
│  │ (读拦截层)      │    │ (快照缓存管理器)     │    │ Store           │  │
│  └─────────────────┘    └─────────────────────┘    └─────────────────┘  │
│           │                        │                         │          │
│           v                        v                         v          │
│  ┌─────────────────┐    ┌─────────────────────┐    ┌─────────────────┐  │
│  │ Upstream        │<──>│ Event Engine        │<──>│ Action          │  │
│  │ Connector       │    │ (事件引擎)           │    │ Dispatcher      │  │
│  │ (上游连接器)    │    │                     │    │ (动作分发器)    │  │
│  └─────────────────┘    └─────────────────────┘    └─────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
```

### 2.1 组件职责

| 组件 | 职责 |
|------|------|
| **Read Interceptor** | 拦截 `*.res.jsonl` 的读请求（`offset`, `length`），判断缓存命中/miss |
| **Snapshot Cache Manager** | 管理缓存元数据、物化状态、状态转换、并发去重 |
| **Upstream Connector** | 调用上游 API（分页或区间拉取），协议转换 |
| **Action Dispatcher** | 解析 ActionLineV2，校验，分发动作执行 |
| **Event Engine** | 写入事件流与重放索引，保证顺序性 |
| **Journal/State Store** | 持久化请求状态、缓存状态、恢复所需信息 |

## 3. 数据流

### 3.1 启动路径（Prewarm）

```
┌─────────────┐     ┌──────────────────┐     ┌─────────────────┐
│ AppFS Init  │────>│ Manifest Scan    │────>│ 枚举 snapshot   │
└─────────────┘     └──────────────────┘     │ 节点            │
                                            └────────┬────────┘
                                                     │
                                                     v
┌─────────────┐     ┌──────────────────┐     ┌─────────────────┐
│ 初始化缓存  │<────│ 获取 metadata    │<────│ Connector       │
│ state=cold  │     │ (size/revision)  │     │ prewarm_snapshot│
└─────────────┘     └──────────────────┘     │ _meta()         │
                                           └─────────────────┘
```

**步骤**：
1. `manifest scan`：扫描 manifest.res.json
2. 枚举 snapshot 节点（`prewarm: true`）
3. 调用 Connector 的 `prewarm_snapshot_meta()`
4. 初始化 cache state（`cold` -> `warming` -> `hot` 或失败 -> `error`）

### 3.2 读路径闭环（snapshot）

```
open/read
  │
  v
┌─────────────────────┐
│ Read Interceptor    │
│ 检查 offset/length  │
└──────────┬──────────┘
           │
           v
     ┌─────┴─────┐
     │ 缓存命中？ │
     └─────┬─────┘
       ┌───┴───┐
      Yes      No
       │        │
       v        v
   ┌───────┐  ┌──────────────────────┐
   │直接返回│  │ expand_snapshot_cache│
   └───────┘  │ 1. 获取资源锁        │
              │ 2. 检查 in-flight    │
              │ 3. 调用 Connector    │
              │ 4. 物化到临时区      │
              │ 5. 原子发布          │
              │ 6. 更新状态          │
              │ 7. 返回数据          │
              └──────────────────────┘
```

### 3.3 动作路径（.act）

```
append JSONL
  │
  v
┌─────────────────────┐
│ Action Dispatcher   │
│ 1. 解析 JSONL 行    │
│ 2. 校验 ActionLineV2│
│ 3. 生成 request_id  │
└──────────┬──────────┘
           │
           v
     ┌─────┴─────┐
     │ 校验通过？ │
     └─────┬─────┘
       ┌───┴───┐
      Yes      No
       │        │
       v        v
┌──────────────┐  ┌──────────────┐
│ 执行动作     │  │ 返回错误     │
│ emit accepted│  │ 不 emit 事件 │
└──────┬───────┘  └──────────────┘
       │
       v
┌──────────────┐
│ Event Engine │
│ emit terminal│
│ (completed/  │
│  failed)     │
└──────────────┘
```

## 4. Snapshot 缓存状态机

### 4.1 状态定义

| 状态 | 含义 | 可读 | 说明 |
|------|------|------|------|
| `cold` | 初始状态，无缓存，未预热 | 否 | 资源已声明但未初始化 |
| `warming` | 正在从上游拉取数据 | 否 | 拉取进行中，等待完成 |
| `hot` | 缓存可用，数据完整 | 是 | 可正常读取 |
| `partial` | 缓存部分可用（读穿扩展中） | 部分 | 部分数据已物化，可继续扩展 |
| `stale` | 缓存可用但可能过期 | 是 | 版本检测发现变化或 TTL 过期 |
| `error` | 上游拉取失败，缓存不可用 | 否 | 需要重试或人工干预 |

### 4.2 状态转换图

```
                         ┌──────────────────────────────────────┐
                         │                                      │
                         v                                      │
┌──────┐  prewarm_start ┌─────────┐  fetch_success  ┌─────┐    │
│ cold │───────────────>│ warming │────────────────>│ hot │────┘
└──────┘                └────┬────┘                 └──┬──┘ version_changed
     ^                       │                         │   ttl_expired
     │                       │                         │
     │              fetch_   │                         v
     │              fail     │              ┌────────────────┐
     │                │      v              │      stale     │
     │                │  ┌─────────┐        └────────┬───────┘
     │                └─>│  error  │                 │
     │                   └────┬────┘                 │
     │                        │                      │
     │              retry     │      read_access    │
     └────────────────────────┘<─────────────────────┘
                              │
                              │ read_access
                              │ (有旧缓存时返回 stale 数据)
                              v
                        [返回错误/旧缓存]
```

### 4.3 状态转换表

| 当前状态 | 触发事件 | 目标状态 | 副作用 | 备注 |
|----------|----------|----------|--------|------|
| `cold` | `prewarm_start` | `warming` | 调用 Connector.metadata() | 启动预热 |
| `cold` | `read_miss` | `warming` | 触发上游拉取 | 首次读取 |
| `warming` | `fetch_success` | `hot` | 原子发布缓存 | 拉取完成 |
| `warming` | `fetch_partial` | `partial` | 发布部分缓存 | 分批拉取 |
| `warming` | `fetch_fail` | `error` | 记录错误信息 | 拉取失败 |
| `warming` | `timeout` | `error` | 记录超时 | 预热/拉取超时 |
| `hot` | `version_changed` | `stale` | 标记需要刷新 | 版本检测 |
| `hot` | `ttl_expired` | `stale` | 标记需要刷新 | TTL 过期 |
| `hot` | `explicit_refresh` | `warming` | 重新拉取 | 显式刷新 |
| `partial` | `continue_fetch` | `partial` | 扩展缓存 | 继续拉取 |
| `partial` | `fetch_complete` | `hot` | 缓存完整 | 拉取完成 |
| `partial` | `fetch_fail` | `error` | 保留部分+标记 | 拉取失败 |
| `stale` | `read_access` | `warming` | 后台刷新 | 读取触发 |
| `stale` | `explicit_refresh` | `warming` | 强制刷新 | 显式刷新 |
| `error` | `retry` | `warming` | 重试拉取 | 重试 |
| `error` | `read_access` | `error` | 返回错误/旧缓存 | 按策略处理 |

### 4.4 并发处理规则

```
┌─────────────────────────────────────────────────────────────────┐
│                     并发读 miss 处理流程                         │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  请求 A ──┐                                                     │
│           │     ┌─────────────┐     ┌─────────────────┐        │
│  请求 B ──┼────>│ 资源级锁    │────>│ 第一个请求进入  │        │
│           │     │ (resource   │     │ warming 状态    │        │
│  请求 C ──┘     │  _lock)     │     │ 触发上游拉取    │        │
│                 └──────┬──────┘     └────────┬────────┘        │
│                        │                     │                 │
│                        v                     v                 │
│                 ┌─────────────┐     ┌─────────────────┐        │
│                 │ 其他请求    │     │ 上游拉取完成    │        │
│                 │ 等待条件变量│<────│ 原子发布        │        │
│                 └──────┬──────┘     │ 通知所有等待者  │        │
│                        │            └─────────────────┘        │
│                        v                                        │
│                 ┌─────────────┐                                 │
│                 │ 使用已拉取  │                                 │
│                 │ 的缓存数据  │                                 │
│                 └─────────────┘                                 │
└─────────────────────────────────────────────────────────────────┘
```

**规则**：
1. `cold -> warming` 转换需要获取资源级锁（`resource_lock`）。
2. 多个并发请求触发同一资源 miss 时，只允许一个进入 `warming`。
3. 其他请求等待 `warming` 完成后直接使用结果（条件变量通知）。
4. waiting 请求超时按 `on_timeout` 策略处理。
5. 去重窗口：同一资源的多个 miss 在 100ms 窗口内合并。

## 5. snapshot 与 live 的兼容策略

### 5.1 snapshot 场景

1. 对 agent 暴露全文件 `*.res.jsonl`。
2. 读取可被 `cat/rg/grep/sed` 直接消费。
3. 若上游实际是分页 API，由后端内部吸收分页并物化为全文件语义。

### 5.2 live 场景

1. 对 agent 暴露 `*.res.json` 分页 envelope。
2. 通过 `_paging/fetch_next.act` 与 `_paging/close.act` 控制分页生命周期。
3. handle 状态纳入 Journal/State Store，支持恢复与过期治理。

### 5.3 混合场景

1. 同一 app 可同时声明 snapshot 与 live 节点。
2. snapshot 与 live 互不替代、职责明确：
   - snapshot 优先检索体验；
   - live 优先动态浏览体验。

## 6. 一致性与可靠性约束

| 约束 | 说明 | 实现方式 |
|------|------|----------|
| **原子扩展** | 扩容必须先落临时区，再原子发布 | tmp + rename 或事务边界 |
| **并发去重** | 同资源同窗口 miss 仅触发一次上游拉取 | 资源级锁 + 条件变量 |
| **恢复可用** | 重启后可恢复 request/cache 状态并继续执行 | Journal 持久化 |
| **终态唯一** | 同一请求不得产生多个 terminal 事件 | request_id 唯一性约束 |
| **行完整性** | 不返回截断的 JSON 行 | JSONL 边界校验 |

## 7. 存储模型

### 7.1 缓存元数据表

```sql
CREATE TABLE appfs_snapshot_cache (
    resource_path TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    state TEXT NOT NULL,           -- cold/warming/hot/partial/stale/error
    version_strategy TEXT NOT NULL,-- auto/revision/last_modified/ttl_only
    version_value TEXT,            -- revision/timestamp
    materialized_bytes INTEGER NOT NULL DEFAULT 0,
    max_materialized_bytes INTEGER NOT NULL,
    fetched_at TEXT,               -- ISO 8601 timestamp
    updated_at TEXT NOT NULL,
    error_message TEXT,
    in_flight INTEGER DEFAULT 0,   -- 并发控制：>0 表示有请求在处理
    FOREIGN KEY (app_id) REFERENCES appfs_apps(app_id)
);
```

### 7.2 Journal 表

```sql
CREATE TABLE appfs_journal (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL UNIQUE,
    app_id TEXT NOT NULL,
    path TEXT NOT NULL,
    client_token TEXT,
    state TEXT NOT NULL,           -- pending/running/completed/failed
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    terminal_event_id TEXT,
    error_code TEXT,
    error_message TEXT
);
```

## 8. 可观测性（最低要求）

| 指标 | 类型 | 说明 |
|------|------|------|
| `cache_hit_ratio` | Gauge | 缓存命中率 |
| `cache_expand_latency_ms` | Histogram | 扩容延迟分布 |
| `expand_fail_total` | Counter | 扩容失败总数 |
| `action_terminal_latency_ms` | Histogram | 动作端到端延迟 |
| `journal_recovery_total` | Counter | 启动恢复次数 |

详见 [非功能性需求](./APPFS-v0.2-非功能性需求.zh-CN.md)。

## 9. 约束

1. 不绑定具体 mount 后端实现细节（FUSE/WinFsp/NFS）。
2. 不绑定具体上游协议（REST/gRPC/SDK）。
3. 必须对外保持接口规范中定义的统一行为。

## 10. 验收

1. 任一实现者可根据组件边界独立开发模块。
2. 数据流可直接映射为 CT2 测试路径。
3. snapshot 与 live 的语义边界清晰且无冲突。
4. 状态机定义完整，可实现为代码。

## 11. 关联文档

1. [总览](./APPFS-v0.2-总览.zh-CN.md)
2. [接口规范](./APPFS-v0.2-接口规范.zh-CN.md)
3. [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)
4. [非功能性需求](./APPFS-v0.2-非功能性需求.zh-CN.md)
5. [合同测试 CT2](./APPFS-v0.2-合同测试CT2.zh-CN.md)
6. [Connector 接口](./APPFS-v0.2-Connector接口.zh-CN.md)
