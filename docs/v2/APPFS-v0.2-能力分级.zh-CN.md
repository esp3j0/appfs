# APPFS v0.2 能力分级

- 版本：`v0.2-draft`
- 状态：`Frozen (Phase A, 2026-03-20)`
- 依赖文档：[总览](./APPFS-v0.2-总览.zh-CN.md)

## 1. 目标

1. 定义 v0.2 能力分级，降低开发者实现负担。
2. 明确 Core/Recommended/Optional 三级边界，避免规范过重。
3. 为 conformance 声明和合同测试提供能力映射依据。

## 2. 能力金字塔

```
┌─────────────────────────────────────────────────────────────────┐
│                        v0.2 能力金字塔                           │
├─────────────────────────────────────────────────────────────────┤
│  Level 3: Optional（可选实现）                                   │
│  ├─ Connector ack_event（上游回执）                              │
│  ├─ 成本估算（estimate_cost）                                    │
│  ├─ 多租户隔离                                                   │
│  └─ 自定义认证刷新（refresh_auth）                               │
├─────────────────────────────────────────────────────────────────┤
│  Level 2: Recommended（推荐实现，影响体验）                       │
│  ├─ 读穿缓存扩展（read_through）                                 │
│  ├─ 启动预热（prewarm）                                          │
│  ├─ action.progress 事件                                         │
│  ├─ action.canceled 事件                                         │
│  ├─ 缓存过期检测（version_check）                                │
│  └─ 可观测指标（observer）                                       │
├─────────────────────────────────────────────────────────────────┤
│  Level 1: Core（必须实现，CT2 required 覆盖）                    │
│  ├─ ActionLineV2 JSONL 解析                                      │
│  ├─ 非 JSONL / mode 字段拒绝                                     │
│  ├─ snapshot 读命中                                              │
│  ├─ live 分页基础流程                                            │
│  ├─ 事件流基础类型（accepted/completed/failed）                  │
│  ├─ cursor/replay 语义                                           │
│  ├─ 标准错误码映射                                               │
│  └─ manifest 自描述                                              │
└─────────────────────────────────────────────────────────────────┘
```

## 3. Level 1: Core（必须实现）

Core 能力是 v0.2 合规的最低要求，必须全部实现才能声明 `core: true`。

### 3.1 ActionLineV2 JSONL 解析

| 项目 | 要求 |
|------|------|
| 协议版本 | `version: "2.0"` |
| 输入格式 | 单行 JSON 对象，以 `\n` 为提交边界 |
| 必填字段 | `version`, `client_token`, `payload` |
| 禁止项 | 原始文本直写、非对象 JSON、`mode` 字段 |

### 3.2 非 JSONL / mode 字段拒绝

| 项目 | 要求 |
|------|------|
| 非 JSONL 拒绝 | submit-time 返回 `INVALID_PAYLOAD` |
| mode 字段拒绝 | submit-time 返回 `INVALID_ARGUMENT` |
| 不触发事件 | 拒绝后不得 emit `action.accepted` |

### 3.3 snapshot 读命中

| 项目 | 要求 |
|------|------|
| 命中定义 | 请求区间 `[offset, offset+length)` 完全在缓存内 |
| 返回内容 | 原始 JSONL 字节流，无 envelope 包装 |
| 延迟目标 | p95 ≤ 50ms（参考 [非功能性需求](./APPFS-v0.2-非功能性需求.zh-CN.md)） |

### 3.4 live 分页基础流程

| 项目 | 要求 |
|------|------|
| 首页读取 | `cat *.res.json` 返回 `{items, page}` envelope |
| handle 生成 | 服务端生成，session-scoped |
| fetch_next | 通过 `/_paging/fetch_next.act` 获取下一页 |
| close | 通过 `/_paging/close.act` 关闭句柄 |

### 3.5 事件流基础类型

| 事件类型 | 触发条件 |
|----------|----------|
| `action.accepted` | streaming 动作成功接受 |
| `action.completed` | 动作成功完成 |
| `action.failed` | 动作执行失败 |

### 3.6 cursor/replay 语义

| 项目 | 要求 |
|------|------|
| cursor.res.json | 暴露 `min_seq`, `max_seq`, `retention_hint_sec` |
| from-seq 重放 | 支持 `/app/<app_id>/_stream/from-seq/<seq>.evt.jsonl` |
| 超范围处理 | `<seq> < min_seq` 返回 `ERANGE` |

### 3.7 标准错误码映射

| 错误码 | 语义 |
|--------|------|
| `INVALID_ARGUMENT` | 参数格式错误 |
| `INVALID_PAYLOAD` | payload 不满足 schema |
| `NOT_SUPPORTED` | 操作不支持 |
| `SNAPSHOT_TOO_LARGE` | 超出 max_materialized_bytes |
| `CACHE_MISS_EXPAND_FAILED` | 读穿扩容失败 |
| `INTERNAL` | 内部错误 |

### 3.8 manifest 自描述

| 文件 | 要求 |
|------|------|
| `manifest.res.json` | 声明 nodes, schemas, limits |
| `context.res.json` | 声明 session_id, principal |
| `permissions.res.json` | 声明 granted/denied scopes |

## 4. Level 2: Recommended（推荐实现）

Recommended 能力影响用户体验和生产可用性，强烈建议实现。

### 4.1 读穿缓存扩展（read_through）

| 项目 | 要求 |
|------|------|
| 触发条件 | 读 miss（请求区间超出缓存） |
| 行为 | 阻塞等待上游拉取 + 原子扩展 |
| 超时处理 | 按 `on_timeout` 策略降级 |
| 相关 CT2 | CT2-003, CT2-004, CT2-005, CT2-006 |

### 4.2 启动预热（prewarm）

| 项目 | 要求 |
|------|------|
| 触发时机 | AppFS 初始化时 |
| 行为 | 对声明的 snapshot 调用 metadata/size API |
| 超时 | `prewarm_timeout_ms`（默认 5000ms） |
| 相关 CT2 | CT2-001 |

### 4.3 action.progress 事件

| 项目 | 要求 |
|------|------|
| 适用场景 | streaming 类型动作 |
| 字段 | `percent`, `stage`, `message` |
| 频率 | 遵循 `progress_policy.max_silence_ms` |

### 4.4 action.canceled 事件

| 项目 | 要求 |
|------|------|
| 触发条件 | 动作被取消（app 策略决定） |
| 终态 | 与 completed/failed 互斥 |

### 4.5 缓存过期检测（version_check）

| 项目 | 要求 |
|------|------|
| 策略 | revision > last_modified > ttl_only |
| 行为 | 检测到过期时标记 `stale` 状态 |

### 4.6 可观测指标（observer）

| 指标 | 语义 |
|------|------|
| `cache_hit_ratio` | 缓存命中率 |
| `cache_expand_latency_ms` | 扩容延迟 |
| `action_terminal_latency_ms` | 动作端到端延迟 |

## 5. Level 3: Optional（可选实现）

Optional 能力针对特定场景，按需实现。

### 5.1 Connector ack_event

| 项目 | 说明 |
|------|------|
| 用途 | 上游回执确认 |
| 场景 | 需要 exactly-once 语义时 |

### 5.2 成本估算（estimate_cost）

| 项目 | 说明 |
|------|------|
| 用途 | 请求成本预估 |
| 场景 | 限流前置检查 |

### 5.3 多租户隔离

| 项目 | 说明 |
|------|------|
| 用途 | 租户级配额和隔离 |
| 场景 | SaaS 多租户部署 |

### 5.4 自定义认证刷新（refresh_auth）

| 项目 | 说明 |
|------|------|
| 用途 | 主动刷新令牌 |
| 场景 | 长期运行会话 |

## 6. conformance 声明格式

### 6.1 最小声明（仅 Core）

```json
{
  "conformance": {
    "appfs_version": "0.2",
    "capabilities": {
      "core": true
    }
  }
}
```

### 6.2 完整声明（Core + 部分 Recommended + 部分 Optional）

```json
{
  "conformance": {
    "appfs_version": "0.2",
    "capabilities": {
      "core": true,
      "recommended": [
        "read_through",
        "prewarm",
        "progress_event",
        "observer"
      ],
      "optional": [
        "ack_event"
      ]
    }
  }
}
```

### 6.3 声明规则

1. `core: true` 是声明 v0.2 合规的前提。
2. `recommended` 数组列出已实现的 Recommended 能力标识。
3. `optional` 数组列出已实现的 Optional 能力标识。
4. 未列出的能力视为未实现。

## 7. 能力与 CT2 映射

| CT2 编号 | 覆盖能力 | 级别 |
|----------|----------|------|
| CT2-001 | prewarm | Recommended |
| CT2-002 | snapshot 读命中 | Core |
| CT2-003 | read_through | Recommended |
| CT2-004 | read_through（并发去重） | Recommended |
| CT2-005 | read_through（超限映射） | Recommended |
| CT2-006 | read_through（恢复一致性） | Recommended |
| CT2-007 | ActionLineV2 JSONL 解析 | Core |
| CT2-008 | 非 JSONL / mode 字段拒绝 | Core |
| CT2-009 | snapshot/live 双语义 | Core |
| CT2-010 | 跨平台一致 | Core |

**CI 分层策略**：
- Required：Core 能力对应的 CT2（CT2-002, CT2-007, CT2-008, CT2-009, CT2-010）
- Required（生产可用）：Recommended 能力对应的 CT2（CT2-001, CT2-003~006）
- Informational：跨平台扩展、压测、长稳运行

## 8. 约束

1. 本文档定义能力分级，不定义具体接口签名。
2. 能力新增允许向后兼容扩展，删除/重命名属于 v2.x 破坏变更。
3. 实现者可根据自身场景选择 Recommended/Optional 子集。

## 9. 验收

1. 实现者可据此明确最低合规要求（Core）。
2. 用户可根据 conformance 声明判断实现能力边界。
3. CT2 可据此分层设置 required/informational。

## 10. 关联文档

1. [总览](./APPFS-v0.2-总览.zh-CN.md)
2. [接口规范](./APPFS-v0.2-接口规范.zh-CN.md)
3. [合同测试 CT2](./APPFS-v0.2-合同测试CT2.zh-CN.md)
4. [非功能性需求](./APPFS-v0.2-非功能性需求.zh-CN.md)
