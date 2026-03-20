# APPFS v0.2 合同测试 CT2（草案）

- 版本：`v0.2-draft`
- 状态：`Frozen (Phase A, 2026-03-20)`
- 依赖文档：[接口规范](./APPFS-v0.2-接口规范.zh-CN.md), [后端架构](./APPFS-v0.2-后端架构.zh-CN.md), [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)

## 1. 目标

1. 先冻结 v0.2 测试合同，再进入编码。
2. 保证每条能力都有可执行验收标准。
3. 建立"接口条款 -> 架构组件 -> 测试用例"的追踪链路。

## 2. CT2 列表总览

| CT2 编号 | 名称 | 能力级别 | CI 分层 |
|----------|------|----------|---------|
| CT2-001 | 启动预热 | Recommended | Required |
| CT2-002 | 读命中 | Core | Required |
| CT2-003 | 读 miss 扩容 | Recommended | Required |
| CT2-004 | 并发去重 | Recommended | Required |
| CT2-005 | 超限映射 | Recommended | Required |
| CT2-006 | 中断恢复 | Recommended | Required |
| CT2-007 | ActionLineV2 JSONL 解析 | Core | Required |
| CT2-008 | 非 JSONL 与 mode 字段拒绝 | Core | Required |
| CT2-009 | snapshot/live 双语义 | Core | Required |
| CT2-010 | 跨平台最小一致性 | Core | Informational |

## 3. CT2 详细验收标准

### 3.1 CT2-001 启动预热

**能力级别**：Recommended
**接口条款**：`prewarm_snapshot`
**架构组件**：Snapshot Cache Manager, Upstream Connector

```gherkin
Feature: Snapshot 启动预热

  Scenario: 正常预热流程
    Given app "aiim" 已挂载
    And manifest 声明 snapshot 资源 "/chats/chat-001/messages.res.jsonl"
    And 该资源配置 "prewarm: true"
    And 上游 Connector 可返回 metadata:
      | size_bytes: 5000 |
      | revision: v1 |

    When AppFS 初始化启动

    Then 调用 Connector.prewarm_snapshot_meta("/chats/chat-001/messages.res.jsonl")
    And 缓存状态为 "hot"
    And 日志包含: "[prewarm] resource=/chats/chat-001/messages.res.jsonl state=hot"

  Scenario: 预热超时不阻塞启动
    Given app "aiim" 已挂载
    And manifest 声明 snapshot 资源 "/chats/chat-001/messages.res.jsonl"
    And 该资源配置 "prewarm: true, prewarm_timeout_ms: 1000"
    And 上游 Connector 响应超时 (>1000ms)

    When AppFS 初始化启动

    Then 启动成功完成
    And 缓存状态为 "cold"
    And 日志包含: "[prewarm] timeout resource=/chats/chat-001/messages.res.jsonl"
```

**Evidence 锚点**：
- 日志：`[prewarm] resource=... state=...`
- 数据库：`appfs_snapshot_cache.state = 'hot'`

---

### 3.2 CT2-002 读命中

**能力级别**：Core
**接口条款**：`read_snapshot` hit path
**架构组件**：Read Interceptor, Snapshot Cache Manager

```gherkin
Feature: Snapshot 缓存命中读取

  Scenario: 缓存命中直接返回
    Given app "aiim" 已挂载
    And snapshot 资源 "/chats/chat-001/messages.res.jsonl" 缓存状态为 "hot"
    And 缓存包含 100 条消息记录 (约 10000 字节)

    When Agent 执行命令:
      | cat /app/aiim/chats/chat-001/messages.res.jsonl |

    Then 返回完整 JSONL 内容 (100 行)
    And 不触发任何上游 API 调用
    And 响应延迟 <= 50ms (p95)

  Scenario: 部分读取命中
    Given app "aiim" 已挂载
    And snapshot 资源 "/chats/chat-001/messages.res.jsonl" 缓存状态为 "hot"
    And 缓存包含 10000 字节

    When Agent 执行命令:
      | dd if=/app/aiim/chats/chat-001/messages.res.jsonl bs=1000 count=5 |

    Then 返回前 5000 字节
    And 不触发上游 API 调用
```

**Evidence 锚点**：
- 日志：`[cache] hit resource=...`
- 指标：`cache_hit_ratio > 0`

---

### 3.3 CT2-003 读 miss 扩容

**能力级别**：Recommended
**接口条款**：`expand_snapshot_cache`
**架构组件**：Read Interceptor, Upstream Connector, 物化层

```gherkin
Feature: Snapshot 读穿缓存扩展

  Scenario: 读 miss 触发扩容
    Given app "aiim" 已挂载
    And snapshot 资源 "/chats/chat-001/messages.res.jsonl" 缓存状态为 "cold"
    And 上游 Connector 可返回 100 条消息记录

    When Agent 执行命令:
      | cat /app/aiim/chats/chat-001/messages.res.jsonl |

    Then 触发 Connector.fetch_snapshot_chunk()
    And 缓存状态转换: cold -> warming -> hot
    And 返回完整 JSONL 内容 (100 行)
    And 事件流包含:
      | type: cache.expand |
      | path: /chats/chat-001/messages.res.jsonl |
      | phase: completed |

  Scenario: 读 miss 超时降级
    Given app "aiim" 已挂载
    And snapshot 资源 "/chats/chat-001/messages.res.jsonl" 缓存状态为 "cold"
    And 该资源配置 "read_through_timeout_ms: 1000, on_timeout: fail"
    And 上游 Connector 响应超时 (>1000ms)

    When Agent 执行命令:
      | cat /app/aiim/chats/chat-001/messages.res.jsonl |

    Then 返回错误 CACHE_MISS_EXPAND_FAILED
    And 日志包含: "[cache] expand failed resource=/chats/chat-001/messages.res.jsonl"
```

**Evidence 锚点**：
- 日志：`[cache] miss, expanding resource=...`
- 日志：`[cache] expanded resource=... bytes=...`
- 数据库：`appfs_snapshot_cache.materialized_bytes > 0`

---

### 3.4 CT2-004 并发去重

**能力级别**：Recommended
**接口条款**：并发去重约束
**架构组件**：Snapshot Cache Manager, Journal/State Store

```gherkin
Feature: 并发读 miss 去重

  Scenario: 并发请求去重成功
    Given app "aiim" 已挂载
    And snapshot 资源 "/chats/chat-001/messages.res.jsonl" 缓存状态为 "cold"
    And 上游 Connector 可返回 100 条消息记录

    When 三个并发进程同时执行:
      | 进程 A: cat /app/aiim/chats/chat-001/messages.res.jsonl |
      | 进程 B: cat /app/aiim/chats/chat-001/messages.res.jsonl |
      | 进程 C: cat /app/aiim/chats/chat-001/messages.res.jsonl |

    Then Connector.fetch_snapshot_chunk() 仅被调用 1 次
    And 三个进程都返回正确内容
    And 事件流仅包含 1 次 cache.expand 事件
    And 日志包含: "[cache] coalesced concurrent miss resource=..."

  Scenario: 去重窗口内的请求合并
    Given app "aiim" 已挂载
    And snapshot 资源 "/chats/chat-001/messages.res.jsonl" 缓存状态为 "cold"

    When 三个请求在 100ms 窗口内依次到达:
      | T+0ms: 请求 A |
      | T+50ms: 请求 B |
      | T+80ms: 请求 C |

    Then 仅请求 A 触发上游拉取
    And 请求 B 和 C 等待并复用 A 的结果
```

**Evidence 锚点**：
- 日志：`[cache] coalesced concurrent miss resource=...`
- 指标：上游调用次数 = 1

---

### 3.5 CT2-005 超限映射

**能力级别**：Recommended
**接口条款**：`SNAPSHOT_TOO_LARGE`
**架构组件**：Snapshot Cache Manager, Event Engine

```gherkin
Feature: Snapshot 大小超限处理

  Scenario: 扩容超限返回错误
    Given app "aiim" 已挂载
    And manifest 声明 snapshot 资源:
      | path: /chats/chat-001/messages.res.jsonl |
      | max_materialized_bytes: 10000 |
    And 缓存状态为 "cold"
    And 上游数据大小为 50000 字节

    When Agent 执行命令:
      | cat /app/aiim/chats/chat-001/messages.res.jsonl |

    Then 返回错误 SNAPSHOT_TOO_LARGE
    And 不存储任何缓存数据
    And 事件流包含:
      | type: cache.expand |
      | phase: failed |
      | error.code: SNAPSHOT_TOO_LARGE |
      | error.details.size: 50000 |
      | error.details.max_size: 10000 |

  Scenario: 部分扩容后超限
    Given app "aiim" 已挂载
    And snapshot 资源配置 max_materialized_bytes: 10000
    And 已缓存 8000 字节

    When 继续读取触发扩容
    And 上游返回额外 5000 字节 (总计 13000 字节)

    Then 返回错误 SNAPSHOT_TOO_LARGE
    And 已缓存的 8000 字节保持不变（原子性）
```

**Evidence 锚点**：
- 日志：`[cache] snapshot_too_large resource=... size=... max=...`
- 数据库：`appfs_snapshot_cache.materialized_bytes` 未变化

---

### 3.6 CT2-006 中断恢复

**能力级别**：Recommended
**接口条款**：恢复约束
**架构组件**：Journal/State Store, Snapshot Cache Manager

```gherkin
Feature: 扩容中断恢复

  Scenario: 扩容中断不暴露半成品
    Given app "aiim" 已挂载
    And snapshot 资源 "/chats/chat-001/messages.res.jsonl" 正在扩容
    And 已拉取 50% 数据到临时区

    When 进程被强制终止 (kill -9)
    And 重新启动 AppFS

    Then 临时区数据不暴露给读者
    And 缓存状态恢复为 "cold" 或 "error"
    And 日志包含: "[recovery] incomplete expansion resource=..."

  Scenario: 重启后恢复并继续
    Given app "aiim" 已挂载
    And Journal 中有未完成的请求记录

    When 重新启动 AppFS

    Then 从 Journal 恢复请求状态
    And 缓存状态正确恢复
    And 可继续处理后续请求
    And 日志包含: "[recovery] restored N pending requests"
```

**Evidence 锚点**：
- 日志：`[recovery] incomplete expansion resource=...`
- 日志：`[recovery] restored N pending requests`
- 读取返回完整 JSONL 行（无截断）

---

### 3.7 CT2-007 ActionLineV2 JSONL 解析

**能力级别**：Core
**接口条款**：ActionLineV2（JSONL-only）
**架构组件**：Action Dispatcher

```gherkin
Feature: ActionLineV2 JSONL 解析

  Scenario: 标准 JSONL 请求解析
    Given app "aiim" 已挂载
    And action 节点 "/contacts/zhangsan/send_message.act" 存在

    When Agent 执行命令:
      | printf '{"version":"2.0","client_token":"msg-001","payload":{"text":"hello"}}\n' >> /app/aiim/contacts/zhangsan/send_message.act |

    Then 请求被正确解析
    And 生成 request_id
    And 事件流包含:
      | type: action.accepted 或 action.completed |
      | client_token: msg-001 |
      | path: /contacts/zhangsan/send_message.act |

  Scenario: payload 包含特殊字符
    Given app "aiim" 已挂载

    When Agent 执行命令:
      | printf '{"version":"2.0","client_token":"msg-002","payload":{"text":"hello\\nworld\\t!"}}\n' >> /app/aiim/contacts/zhangsan/send_message.act |

    Then 请求被正确解析
    And payload.text 包含换行和制表符

  Scenario: 多行 JSONL 连续提交
    Given app "aiim" 已挂载

    When Agent 执行命令:
      | printf '{"version":"2.0","client_token":"msg-003","payload":{"text":"a"}}\n{"version":"2.0","client_token":"msg-004","payload":{"text":"b"}}\n' >> /app/aiim/contacts/zhangsan/send_message.act |

    Then 两个请求都被正确解析
    And 生成两个不同的 request_id
    And 事件流包含两个 action.completed 事件
```

**Evidence 锚点**：
- 事件流：包含 `request_id`, `client_token`
- 日志：`[action] parsed request_id=... client_token=...`

---

### 3.8 CT2-008 非 JSONL 与 mode 字段拒绝

**能力级别**：Core
**接口条款**：协议收口拒绝规则
**架构组件**：Action Dispatcher, Validation Layer

```gherkin
Feature: 协议违规拒绝

  Scenario: 原始文本直写被拒绝
    Given app "aiim" 已挂载

    When Agent 执行命令:
      | printf 'hello world\n' >> /app/aiim/contacts/zhangsan/send_message.act |

    Then 返回错误 EINVAL
    And 不触发 action.accepted 事件
    And 日志包含: "[action] rejected: not json line=hello world"

  Scenario: 包含 mode 字段被拒绝
    Given app "aiim" 已挂载

    When Agent 执行命令:
      | printf '{"version":"2.0","mode":"text","client_token":"x","payload":{}}\n' >> /app/aiim/contacts/zhangsan/send_message.act |

    Then 返回错误 INVALID_ARGUMENT
    And 不触发 action.accepted 事件
    And 日志包含: "[action] rejected: mode field not allowed"

  Scenario: 非对象 JSON 被拒绝
    Given app "aiim" 已挂载

    When Agent 执行命令:
      | printf '["array","not","allowed"]\n' >> /app/aiim/contacts/zhangsan/send_message.act |

    Then 返回错误 INVALID_ARGUMENT
    And 不触发 action.accepted 事件

  Scenario: 缺少必填字段被拒绝
    Given app "aiim" 已挂载

    When Agent 执行命令:
      | printf '{"version":"2.0","payload":{}}\n' >> /app/aiim/contacts/zhangsan/send_message.act |

    Then 返回错误 INVALID_ARGUMENT
    And 错误信息包含: "client_token required"
```

**Evidence 锚点**：
- 日志：`[action] rejected: ...`
- 返回码：EINVAL 或 INVALID_ARGUMENT
- 事件流：无 action.accepted

---

### 3.9 CT2-009 snapshot/live 双语义

**能力级别**：Core
**接口条款**：snapshot/live 双形态
**架构组件**：Read Interceptor, Paging Control Path

```gherkin
Feature: Snapshot 与 Live 双语义

  Scenario: snapshot 返回全文件 JSONL
    Given app "aiim" 已挂载
    And snapshot 资源 "/chats/chat-001/messages.res.jsonl" 缓存为 hot

    When Agent 执行命令:
      | cat /app/aiim/chats/chat-001/messages.res.jsonl |

    Then 返回纯 JSONL 内容（每行一个 JSON 对象）
    And 不包含 {items, page} envelope
    And 可被 rg/grep/sed 直接处理

  Scenario: live 返回分页 envelope
    Given app "aiim" 已挂载
    And live 资源 "/feed/recommendations.res.json" 存在

    When Agent 执行命令:
      | cat /app/aiim/feed/recommendations.res.json |

    Then 返回 {items, page} envelope
    And page.handle_id 存在
    And page.mode = "live"

  Scenario: live 分页流程
    Given app "aiim" 已挂载
    And 首次读取返回 handle_id: "ph_live_001"

    When Agent 执行命令:
      | printf '{"handle_id":"ph_live_001"}\n' >> /app/aiim/_paging/fetch_next.act |

    Then 返回下一页数据
    And page.page_no 递增
```

**Evidence 锚点**：
- snapshot：无 envelope，纯 JSONL
- live：包含 `{items: [...], page: {...}}`

---

### 3.10 CT2-010 跨平台最小一致性

**能力级别**：Core
**接口条款**：跨平台一致
**架构组件**：全链路

```gherkin
Feature: 跨平台一致性

  Scenario Outline: 核心功能跨平台一致
    Given 平台 <platform>
    And app "aiim" 已挂载

    When 执行 <operation>

    Then 行为与 Linux 参考实现一致

    Examples:
      | platform | operation |
      | Linux    | cat snapshot resource |
      | Windows  | cat snapshot resource |
      | Linux    | append action |
      | Windows  | append action |
      | Linux    | read events stream |
      | Windows  | read events stream |

  Scenario: Windows 路径分隔符处理
    Given 平台 Windows
    And app "aiim" 已挂载

    When Agent 使用反斜杠路径:
      | type \\app\\aiim\\_meta\\manifest.res.json |

    Then 正确映射为正斜杠路径
    And 返回正确内容
```

**Evidence 锚点**：
- 测试矩阵：Linux/Windows 对比结果
- 差异报告：记录不一致项

---

## 4. 追踪矩阵

| CT2 | 接口条款 | 架构组件 | 能力级别 |
|-----|----------|----------|----------|
| CT2-001 | `prewarm_snapshot` | Snapshot Cache Manager, Upstream Connector | Recommended |
| CT2-002 | `read_snapshot` hit path | Read Interceptor, Snapshot Cache Manager | Core |
| CT2-003 | `expand_snapshot_cache` | Read Interceptor, Upstream Connector, 物化层 | Recommended |
| CT2-004 | 并发去重约束 | Snapshot Cache Manager, Journal/State Store | Recommended |
| CT2-005 | `SNAPSHOT_TOO_LARGE` | Snapshot Cache Manager, Event Engine | Recommended |
| CT2-006 | 恢复约束 | Journal/State Store, Snapshot Cache Manager | Recommended |
| CT2-007 | ActionLineV2（JSONL-only） | Action Dispatcher | Core |
| CT2-008 | 协议收口拒绝规则 | Action Dispatcher, Validation Layer | Core |
| CT2-009 | snapshot/live 双形态 | Read Interceptor, Paging Control Path | Core |
| CT2-010 | 跨平台一致 | 全链路 | Core |

## 5. CI 分层

### 5.1 Required（阻塞发布）

| 测试项 | 平台 |
|--------|------|
| CT2-001 | Linux |
| CT2-002 | Linux |
| CT2-003 | Linux |
| CT2-004 | Linux |
| CT2-005 | Linux |
| CT2-006 | Linux |
| CT2-007 | Linux |
| CT2-008 | Linux |
| CT2-009 | Linux |
| v0.1 baseline smoke | Linux |

### 5.2 Informational（不阻塞）

| 测试项 | 说明 |
|--------|------|
| CT2-010 | 跨平台扩展矩阵 |
| 性能压测 | p95/p99 延迟验证 |
| 长稳运行 | 24h 稳定性测试 |

## 6. 约束

1. CT2 编号和语义冻结后，不允许在实现中临时改口径。
2. 任何协议调整必须先更新接口文档，再更新 CT2。
3. 先让 CT2 进入"可失败执行"状态，再开始开发实现。

## 7. 验收

1. 每条 v0.2 核心能力至少有一个 required CT2 覆盖。
2. 可直接据此拆分测试任务与开发任务。
3. 能形成 release gate，不依赖人工口头判断。

## 8. 关联文档

1. [总览](./APPFS-v0.2-总览.zh-CN.md)
2. [接口规范](./APPFS-v0.2-接口规范.zh-CN.md)
3. [后端架构](./APPFS-v0.2-后端架构.zh-CN.md)
4. [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)
5. [实施计划](./APPFS-v0.2-实施计划.zh-CN.md)
