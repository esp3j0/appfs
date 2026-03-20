# APPFS v0.2 非功能性需求

- 版本：`v0.2-draft`
- 状态：`Frozen (Phase A, 2026-03-20)`
- 依赖文档：[接口规范](./APPFS-v0.2-接口规范.zh-CN.md), [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)

## 1. 目标

1. 定义 v0.2 性能、可靠性、容量边界。
2. 为 Recommended 级别能力提供量化目标。
3. 为 CT2 测试和 RC 发布提供验收依据。

## 2. 性能目标

### 2.1 读路径延迟

| 指标 | 目标值 | 测量条件 | 能力级别 |
|------|--------|----------|----------|
| 缓存命中读延迟 (p50) | ≤ 10ms | hot 状态，本地缓存 | Core |
| 缓存命中读延迟 (p95) | ≤ 50ms | hot 状态，本地缓存 | Core |
| 缓存命中读延迟 (p99) | ≤ 100ms | hot 状态，本地缓存 | Core |
| 读穿扩展延迟 (p50) | ≤ 1s | 上游正常，首次 miss | Recommended |
| 读穿扩展延迟 (p95) | ≤ 3s | 上游正常，首次 miss | Recommended |
| 读穿扩展延迟 (p99) | ≤ 5s | 上游正常，首次 miss | Recommended |

### 2.2 写路径延迟

| 指标 | 目标值 | 测量条件 | 能力级别 |
|------|--------|----------|----------|
| action 接受延迟 (p95) | ≤ 100ms | append 完成到 accepted 事件 | Core |
| action inline 完成延迟 (p95) | ≤ inline_timeout_ms | inline 类型动作 | Core |
| action streaming 首个事件延迟 (p95) | ≤ 1s | streaming 类型动作 | Recommended |
| 事件写入延迟 (p95) | ≤ 100ms | 事件生成到可读取 | Core |

### 2.3 启动与恢复

| 指标 | 目标值 | 测量条件 | 能力级别 |
|------|--------|----------|----------|
| 单 app 启动时间（无预热） | ≤ 1s | 不含 prewarm | Core |
| 单 app 启动时间（含预热） | ≤ 5s | 10 个 snapshot 资源 | Recommended |
| 单资源预热超时 | ≤ 5s | 单个资源 metadata 获取 | Recommended |
| 启动恢复时间 | ≤ 30s | 从 journal 恢复状态 | Recommended |

### 2.4 吞吐量

| 指标 | 目标值 | 测量条件 | 能力级别 |
|------|--------|----------|----------|
| snapshot 并发读取 | ≥ 100 QPS | 单 app，缓存命中 | Core |
| action 并发提交 | ≥ 50 QPS | 单 app，inline 类型 | Core |
| 事件流并发消费 | ≥ 50 连接 | 单 app | Core |

## 3. 可靠性目标

### 3.1 数据一致性

| 指标 | 目标 | 说明 | 能力级别 |
|------|------|------|----------|
| 终态事件唯一性 | 100% | 同一 request_id 仅一个 terminal 事件 | Core |
| 并发去重正确性 | 100% | 同窗口无重复上游拉取 | Recommended |
| 缓存原子性 | 100% | 不暴露半成品内容 | Recommended |
| JSONL 行完整性 | 100% | 不返回截断的 JSON 行 | Core |

### 3.2 故障恢复

| 指标 | 目标 | 说明 | 能力级别 |
|------|------|------|----------|
| 重启恢复成功率 | ≥ 99.9% | 从 journal 恢复后继续服务 | Recommended |
| 上游故障降级 | 有旧缓存时可用 | 返回 stale 数据 + 警告事件 | Recommended |
| 部分拉取恢复 | 支持 | 中断后可继续扩展 | Recommended |

### 3.3 错误处理

| 场景 | 预期行为 | 能力级别 |
|------|----------|----------|
| 上游超时 | 返回 `CACHE_MISS_EXPAND_FAILED` 或降级 | Core |
| 上游 5xx | 标记 `retryable: true`，建议重试 | Core |
| 上游 4xx | 标记 `retryable: false`，不重试 | Core |
| 认证失败 | 返回 `PERMISSION_DENIED`，触发刷新（如有） | Core |
| 限流 | 返回 `RESOURCE_EXHAUSTED`，建议退避 | Core |

## 4. 容量边界

### 4.1 资源容量

| 指标 | 边界值 | 说明 | 配置方式 |
|------|--------|------|----------|
| 单 snapshot 最大缓存 | `max_materialized_bytes` | manifest 声明 | 资源级配置 |
| 默认单 snapshot 上限 | 100 MB | 未显式配置时 | 全局默认 |
| 硬性单 snapshot 上限 | 1 GB | 超出返回 `SNAPSHOT_TOO_LARGE` | 系统限制 |
| 单 app 最大 snapshot 数 | 100 | 建议值 | 可调 |
| 单 live handle 最长生命周期 | `handle_ttl_sec` | manifest 声明 | 资源级配置 |
| 默认 handle TTL | 600s (10分钟) | 未显式配置时 | 全局默认 |

### 4.2 并发容量

| 指标 | 边界值 | 说明 |
|------|--------|------|
| 单 app 并发 action | 100 | 建议值，超出可排队 |
| 单 app 并发 snapshot 读 | 100 | 建议值 |
| 单 app 事件流连接数 | 50 | 建议值 |
| 单 session 并发 handle | 20 | 防止资源泄漏 |

### 4.3 存储容量

| 指标 | 边界值 | 说明 |
|------|--------|------|
| 单 app 缓存总大小 | 1 GB | 建议值，超出触发 LRU 淘汰 |
| 事件流保留时间 | `retention_hint_sec` | manifest 声明 |
| 默认事件保留时间 | 7 天 | 未显式配置时 |
| Journal 保留条数 | 10000 | 建议值 |

## 5. 安全性要求

### 5.1 认证与会话

| 要求 | 说明 | 能力级别 |
|------|------|----------|
| 认证状态检查 | Connector 必须支持 `health()` 检查 | Core |
| 会话隔离 | handle/session_id 必须隔离 | Core |
| 越权拒绝 | 跨 session 访问返回 `PERMISSION_DENIED` | Core |
| 凭证刷新 | 支持 `refresh_auth()`（Optional） | Optional |

### 5.2 数据安全

| 要求 | 说明 | 能力级别 |
|------|------|----------|
| 敏感字段脱敏 | 写入日志/事件前脱敏 | Core |
| 凭证不落盘 | token/secret 不持久化到文件 | Core |
| 审计日志 | 关键操作记录到 tool_calls 表 | Recommended |

### 5.3 威胁模型

| 威胁 | 缓解措施 |
|------|----------|
| 路径遍历 | 运行时验证路径，拒绝 `..`、驱动器前缀 |
| 注入攻击 | JSONL 解析严格校验，拒绝非法 JSON |
| 资源耗尽 | 容量边界限制，超限返回 `RESOURCE_EXHAUSTED` |
| 重放攻击 | `client_token` 支持幂等去重（app 定义） |

## 6. 可观测性要求

### 6.1 必须暴露的指标（Core）

| 指标 | 类型 | 说明 |
|------|------|------|
| `action_accepted_total` | Counter | 接受的 action 总数 |
| `action_completed_total` | Counter | 完成的 action 总数 |
| `action_failed_total` | Counter | 失败的 action 总数 |

### 6.2 推荐暴露的指标（Recommended）

| 指标 | 类型 | 说明 |
|------|------|------|
| `cache_hit_ratio` | Gauge | 缓存命中率 |
| `cache_expand_latency_ms` | Histogram | 扩容延迟分布 |
| `expand_fail_total` | Counter | 扩容失败总数 |
| `action_terminal_latency_ms` | Histogram | 动作端到端延迟 |
| `journal_recovery_total` | Counter | 启动恢复次数 |
| `upstream_call_total` | Counter | 上游调用总数（按状态码） |
| `upstream_latency_ms` | Histogram | 上游调用延迟 |

### 6.3 日志要求

| 级别 | 场景 |
|------|------|
| ERROR | 上游失败、扩容失败、恢复失败 |
| WARN | 预热超时、缓存过期、降级使用 stale 数据 |
| INFO | action 接受/完成、缓存扩展、恢复成功 |
| DEBUG | 详细请求/响应、状态转换 |

### 6.4 追踪要求

| 字段 | 用途 |
|------|------|
| `request_id` | 单次请求追踪 |
| `client_token` | 客户端关联 |
| `trace_id` | 分布式追踪（Optional） |
| `upstream_request_id` | 上游调用关联（如有） |

## 7. 兼容性要求

### 7.1 协议版本兼容

| 场景 | 策略 |
|------|------|
| v0.1 → v0.2 迁移 | 允许一次性破坏性升级（不要求共存窗口） |
| v0.2.x 小版本升级 | 向后兼容，字段可新增不可删除 |
| v0.2 → v0.3 大版本升级 | 需要显式迁移 |

### 7.2 平台兼容

| 平台 | 支持级别 | 说明 |
|------|----------|------|
| Linux | Required | CT2 required 覆盖 |
| Windows | Informational | CT2-010 覆盖 |
| macOS | Informational | 参考 Linux 行为 |

## 8. 约束

1. 本文同时覆盖 Core 与 Recommended：Core 约束正确性与最低性能门槛，Recommended 约束体验与生产可用性目标。
2. 目标值用于 RC 验收，不作为运行时 SLA 承诺。
3. 容量边界为建议值，实现者可根据资源调整。

## 9. 验收

1. 性能目标可通过 CT2 + 压测验证。
2. 可靠性目标可通过故障注入测试验证。
3. 容量边界可通过极限测试验证。
4. 安全性可通过安全审计和渗透测试验证。

## 10. 关联文档

1. [接口规范](./APPFS-v0.2-接口规范.zh-CN.md)
2. [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)
3. [合同测试 CT2](./APPFS-v0.2-合同测试CT2.zh-CN.md)
4. [后端架构](./APPFS-v0.2-后端架构.zh-CN.md)
