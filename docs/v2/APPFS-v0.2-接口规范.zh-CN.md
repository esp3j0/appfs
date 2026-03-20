# APPFS v0.2 接口规范（后端模式）

- 版本：`v0.2-draft`
- 状态：`Frozen (Phase A, 2026-03-20)`
- 依赖文档：[总览](./APPFS-v0.2-总览.zh-CN.md), [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)

## 1. 目标

1. 固定 v0.2 的动作输入、资源读写、分页与错误映射接口。
2. 避免实现阶段再做协议决策。
3. 支持任意 app 在同一接口模型下接入。

## 2. `.act` 输入协议（ActionLineV2，JSONL-only）

### 2.1 行协议定义

`.act` 为 append-only JSONL，单行即一个请求对象：

```json
{
  "version": "2.0",
  "client_token": "msg-001",
  "payload": { "text": "hello\\nworld" }
}
```

### 2.2 必填字段

| 字段 | 类型 | 说明 |
|------|------|------|
| `version` | string | 固定为 `"2.0"` |
| `client_token` | string | 客户端幂等关联键，用于事件回溯 |
| `payload` | object | 动作负载对象，必须满足该动作的 `input_schema` |

### 2.3 协议约束

1. 仅接受"单行 JSON 对象"。
2. 文本类输入通过 `payload` 内的字符串字段表达（例如 `payload.text`）。
3. 一行一请求，换行 `\n` 为提交边界。
4. 不区分 `text/json` 模式，协议层不再定义 `mode` 字段。

### 2.4 禁止项

| 禁止项 | 处理方式 |
|--------|----------|
| 原始文本直写到 `.act`（非 JSONL） | submit-time 返回 `INVALID_PAYLOAD` |
| 无换行的半行提交 | 不作为有效请求，等待补全 |
| 非对象 JSON（数组、纯字符串、数字） | submit-time 返回 `INVALID_ARGUMENT` |
| 使用 `mode` 字段作为协议控制字段 | submit-time 返回 `INVALID_ARGUMENT` |

## 3. 资源接口

### 3.1 snapshot（`*.res.jsonl`）

面向有限数据、全文检索、文件语义。

#### 3.1.1 接口定义

| 接口 | 说明 |
|------|------|
| `prewarm_snapshot(resource_path)` | 初始化 metadata（size/revision/ttl） |
| `read_snapshot(resource_path, offset, length)` | 读拦截入口，命中缓存直接返回 |
| `expand_snapshot_cache(resource_path, from_cursor\|from_offset)` | 读 miss 时扩容，必须原子发布 |

#### 3.1.2 预热配置（Recommended）

```json
// manifest.res.json 中的 snapshot 节点声明
"nodes": {
  "chats/{chat_id}/messages.res.jsonl": {
    "kind": "resource",
    "output_mode": "jsonl",
    "snapshot": {
      "max_materialized_bytes": 10485760,
      "prewarm": true,
      "prewarm_timeout_ms": 5000
    }
  }
}
```

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `prewarm` | boolean | `true` | 是否在启动时预热该资源 |
| `prewarm_timeout_ms` | integer | `5000` | 单资源预热超时（毫秒） |

**预热行为**：
1. 启动时对 `prewarm: true` 的资源调用 Connector 的 `prewarm_snapshot_meta()`。
2. 超时不阻塞启动，仅记录 WARN 日志。
3. 预热失败的资源状态为 `cold`，首次读取时触发读穿扩展。

#### 3.1.3 读 miss 策略配置（Recommended）

```json
"snapshot": {
  "read_through_timeout_ms": 10000,
  "on_timeout": "return_stale"
}
```

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `read_through_timeout_ms` | integer | `10000` | 读穿扩展等待超时（毫秒） |
| `on_timeout` | string | `"return_stale"` | 超时处理策略 |

**on_timeout 策略**：

| 值 | 行为 |
|----|------|
| `return_stale` | 有旧缓存时返回旧数据 + 事件流发 `cache.stale` 警告；无旧缓存返回 `EAGAIN` |
| `fail` | 直接返回 `CACHE_MISS_EXPAND_FAILED` |

#### 3.1.4 版本一致性策略配置（Recommended）

```json
"snapshot": {
  "version_strategy": "auto",
  "ttl_sec": 300
}
```

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `version_strategy` | string | `"auto"` | 版本一致性检测策略 |
| `ttl_sec` | integer | `300` | 仅 `ttl_only` 策略有效 |

**version_strategy 策略**：

| 值 | 行为 | 适用场景 |
|----|------|----------|
| `auto` | 自动检测：revision > last_modified > ttl_only | 推荐，通用场景 |
| `revision` | 强制使用 revision/ETag | 上游支持强一致性 |
| `last_modified` | 使用 last_modified 时间戳比较 | 上游有时间戳无 revision |
| `ttl_only` | 仅依赖 TTL 过期 | 上游无版本信息 |

**缓存元数据必须记录**：
- `version_strategy`：实际使用的策略
- `version_value`：具体值（revision/timestamp）
- `fetched_at`：拉取时间戳

### 3.2 live（`*.res.json`）

面向动态数据、分页浏览。

#### 3.2.1 接口定义

| 接口 | 说明 |
|------|------|
| `fetch_live_page(resource_path, handle_id\|cursor, page_size)` | 获取下一页数据 |
| `close_live_handle(handle_id)` | 关闭分页句柄 |

#### 3.2.2 分页配置

```json
// manifest.res.json 中的 live 节点声明
"nodes": {
  "feed/recommendations.res.json": {
    "kind": "resource",
    "output_mode": "json",
    "paging": {
      "enabled": true,
      "mode": "live",
      "default_page_size": 20,
      "max_page_size": 50,
      "handle_ttl_sec": 600
    }
  }
}
```

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `enabled` | boolean | - | 是否启用分页 |
| `mode` | string | `"live"` | 分页模式（v0.2 仅支持 live） |
| `default_page_size` | integer | `20` | 默认每页条数 |
| `max_page_size` | integer | `100` | 最大每页条数 |
| `handle_ttl_sec` | integer | `600` | 句柄有效期（秒） |

### 3.3 snapshot 控制动作（Optional）

为避免删除后再新增的来回调整，v0.2 保留显式刷新入口 `/_snapshot/refresh.act`（Optional）。

#### 3.3.1 路径与输入

- 路径：`/app/<app_id>/_snapshot/refresh.act`
- 输入：ActionLineV2 JSONL（payload 示例）

```json
{
  "version": "2.0",
  "client_token": "refresh-001",
  "payload": {
    "resource_path": "/chats/chat-001/messages.res.jsonl",
    "refresh_type": "revalidate"
  }
}
```

#### 3.3.2 字段约束

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `resource_path` | string | 是 | 目标 snapshot 路径 |
| `refresh_type` | string | 否 | `revalidate` 或 `rematerialize`，默认 `revalidate` |

#### 3.3.3 行为约束

1. `revalidate`：仅重校验版本（必要时触发扩容）。
2. `rematerialize`：强制重物化（受 `max_materialized_bytes` 限制）。
3. 该动作执行结果通过事件流返回 `action.completed/action.failed`。

## 4. 事件与状态接口

### 4.1 事件类型

| 事件类型 | 级别 | 触发条件 |
|----------|------|----------|
| `action.accepted` | Core | streaming 动作成功接受 |
| `action.completed` | Core | 动作成功完成 |
| `action.failed` | Core | 动作执行失败 |
| `action.progress` | Recommended | streaming 动作进度更新 |
| `action.canceled` | Recommended | 动作被取消 |
| `cache.stale` | Recommended | 使用过期缓存警告 |
| `cache.expand` | Recommended | 缓存扩展事件 |

### 4.2 事件关联字段

| 字段 | 类型 | 说明 |
|------|------|------|
| `request_id` | string | 服务端生成的请求 ID |
| `client_token` | string | 客户端提供的关联键 |
| `path` | string | 动作路径 |
| `session_id` | string | 会话 ID |

### 4.3 流与重放

保持 v0.1 兼容结构：

| 路径 | 说明 |
|------|------|
| `events.evt.jsonl` | 事件流 |
| `from-seq/<seq>.evt.jsonl` | 从指定序列号重放 |
| `cursor.res.json` | 游标信息 |

## 5. 错误码最小集

### 5.1 标准错误码

| 错误码 | 语义 | retryable |
|--------|------|-----------|
| `INVALID_ARGUMENT` | 参数格式错误 | false |
| `INVALID_PAYLOAD` | payload 不满足 schema | false |
| `NOT_SUPPORTED` | 操作不支持 | false |
| `SNAPSHOT_TOO_LARGE` | 超出 max_materialized_bytes | false |
| `CACHE_MISS_EXPAND_FAILED` | 读穿扩容失败 | true |
| `INTERNAL` | 内部错误 | true |

### 5.2 扩展错误码（Optional）

| 错误码 | 语义 | retryable |
|--------|------|-----------|
| `UPSTREAM_UNAVAILABLE` | 上游服务不可用 | true |
| `RATE_LIMITED` | 被限流 | true |
| `AUTH_EXPIRED` | 认证过期 | false |
| `PERMISSION_DENIED` | 权限不足 | false |
| `RESOURCE_EXHAUSTED` | 资源耗尽 | true |

### 5.3 映射要求

1. submit-time 校验错误必须确定性（不 emit `action.accepted`）。
2. read-through 扩容失败必须可诊断（包含资源路径与阶段信息）。
3. 必须显式标注 `retryable`，供客户端决策重试策略。

## 6. conformance 声明格式

```json
{
  "conformance": {
    "appfs_version": "0.2",
    "capabilities": {
      "core": true,
      "recommended": [
        "read_through",
        "prewarm",
        "progress_event"
      ],
      "optional": []
    }
  }
}
```

详见 [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)。

## 7. 约束

1. 本规范为决策级接口，不绑定具体语言实现。
2. 后端可以任意语言实现，但外部语义必须一致。
3. 字段新增允许向后兼容扩展，删除/重命名属于 v2.x 破坏变更流程。

## 8. 验收

1. 实现者无需再决定 `.act` 输入分支，统一 JSONL 即可。
2. 实现者无需再决定 snapshot/live 的外部形态。
3. 错误码集合可直接用于 CT2 断言。
4. 配置项可直接映射到 manifest 字段。

## 9. 关联文档

1. [总览](./APPFS-v0.2-总览.zh-CN.md)
2. [后端架构](./APPFS-v0.2-后端架构.zh-CN.md)
3. [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)
4. [非功能性需求](./APPFS-v0.2-非功能性需求.zh-CN.md)
5. [合同测试 CT2](./APPFS-v0.2-合同测试CT2.zh-CN.md)
