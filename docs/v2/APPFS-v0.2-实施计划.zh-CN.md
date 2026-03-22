# APPFS v0.2 实施计划（阶段门禁）

- 版本：`v0.2-draft`
- 状态：`Frozen (Phase A, 2026-03-20)`
- 依赖文档：[总览](./APPFS-v0.2-总览.zh-CN.md), [接口规范](./APPFS-v0.2-接口规范.zh-CN.md), [合同测试 CT2](./APPFS-v0.2-合同测试CT2.zh-CN.md), [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)

## 1. 目标

1. 用阶段门禁方式推进 v0.2，避免边实现边改需求。
2. 每阶段都具备输入、输出、DoD、风险处置条件。
3. 以 CT2 为唯一功能验收基准。

## 2. Phase A： 文档冻结

### 输入

1. v0.2 正式文档集草案（10 个正式文档 + 1 个 superseded 草案）。
2. v0.1 差异清单。

### 输出

1. 正式文档集评审通过版本（冻结标记）。
2. backlog 拆分依据（Epic/Issue 列表）。

### DoD（Definition of Done）

1. `.act` ActionLineV2、snapshot/live 双形态、错误码集合全部冻结。
2. CT2 编号与语义冻结。
3. 开放问题全部决策并写入正式文档。

### 风险处置条件

1. 关键接口存在未决策项 → 必须在进入 Phase B 前决策完成。
2. CT2 无法映射到接口与架构组件 → 需要调整接口或架构设计。

## 3. Phase B: 接口骨架实现（最小可用）

### 输入

1. 冻结后的接口规范。
2. CT2-002/007/008/009 最小集（Core 能力）。

### 输出

1. 单资源 snapshot read-through 骨架链路。
2. ActionLineV2（JSONL-only）解析骨架。

### DoD

1. CT2 最小集可执行并有明确红绿结果。
2. 不破坏 v0.1 baseline。

### 风险处置条件

1. 需要临时修改接口字段才能继续 → 回退到 Phase A 重新决策。
2. 最小链路无法提供可诊断日志 → 需要补充日志锚点。

## 4. Phase C: 可靠性实现

### 输入

1. 骨架实现。
2. CT2-004/005/006 需求（Recommended 能力）。

### 输出

1. Journal/State Store 持久化能力。
2. 并发去重与恢复机制。
3. 超限错误映射完善。

### DoD

1. CT2-004/005/006 通过。
2. 重启恢复与终态唯一不冲突。

### 风险处置条件

1. 出现不可控重复拉取或半成品读取 → 回退设计，修复并发控制。
2. 重启后状态机不可恢复 → 需要增强 Journal 持久化。

## 5. Phase D: 全链路实现

### 输入

1. 可靠性基础。
2. live 分页与 snapshot 并存需求。

### 输出

1. snapshot/live/act（JSONL-only）全链路实现。
2. 事件、重放、cursor 一致性实现。

### DoD

1. CT2-001..CT2-009 全绿（Linux required）。
2. v0.1 baseline smoke 通过。

### 风险处置条件

1. snapshot 与 live 语义互相污染 → 需要明确边界，回退修复。
2. 事件流与重放出现不一致 → 需要检查事件写入顺序和 cursor 管理。

## 6. Phase E: RC 收口

### 输入

1. 全链路实现。
2. CI required/informational 结果。

### 输出

1. v0.2 RC 文档包（迁移、风险、处置预案）。
2. 发布门禁记录与证据。

### DoD

1. CT2 required 全绿。
2. 跨平台最小一致性达标（CT2-010 至少 informational 稳定）。
3. 可生成明确迁移 issue 列表。

### 风险处置条件

1. 关键 required 反复不稳定 → 需要修复根本问题，不得带病发布。
2. 无法给出迁移风险闭环说明 → 需要补充迁移文档。

## 7. 管理方式（文档 vs Issue）

### 文档管理"规范真相"

| 管理对象 | 说明 |
|----------|------|
| 协议 | 接口规范文档 |
| 架构 | 后端架构文档 |
| 测试标准 | 合同测试 CT2 文档 |
| 能力定义 | 能力分级文档 |
| 阶段门禁 | 本文档 |

### Issue 管理"执行任务"

每个 issue 必须引用至少一个文档锚点：

```
Issue 模板：
- 标题：[Phase X] 功能名称
- 引用文档：APPFS-v0.2-XXX.zh-CN.md#章节
- 验收标准：CT2-XXX 通过
- DoD 检查项：
  - [ ] 功能实现
  - [ ] 单元测试通过
  - [ ] CT2 通过
```

## 8. v0.2 文档集

| 文档 | 说明 | 状态 |
|------|------|------|
| [总览](./APPFS-v0.2-总览.zh-CN.md) | 目标、边界、术语定义 | Frozen (Phase A) |
| [接口规范](./APPFS-v0.2-接口规范.zh-CN.md) | ActionLineV2、配置项、错误码 | Frozen (Phase A) |
| [后端架构](./APPFS-v0.2-后端架构.zh-CN.md) | 组件边界、数据流、状态机 | Frozen (Phase A) |
| [能力分级](./APPFS-v0.2-能力分级.zh-CN.md) | Core/Recommended/Optional 定义 | Frozen (Phase A) |
| [非功能性需求](./APPFS-v0.2-非功能性需求.zh-CN.md) | 性能/可靠性/容量目标 | Frozen (Phase A) |
| [合同测试 CT2](./APPFS-v0.2-合同测试CT2.zh-CN.md) | CT2-001~CT2-010 验收标准 | Frozen (Phase A) |
| [Connector 接口](./APPFS-v0.2-Connector接口.zh-CN.md) | 真实 app 对接层契约 | Frozen (Phase A) |
| [真实 App 对接规范](./APPFS-v0.2-真实App对接规范.zh-CN.md) | 接入前置清单、映射规则 | Frozen (Phase A) |
| [实施计划](./APPFS-v0.2-实施计划.zh-CN.md) | Phase A~E 阶段门禁 | Frozen (Phase A) |
| [RC迁移与上线包](./APPFS-v0.2-RC迁移与上线包.zh-CN.md) | Phase E 迁移、灰度、回退、处置与 issue 生成规则 | Draft (Phase E) |
| [backend-mode-requirements-draft](./APPFS-v0.2-backend-mode-requirements-draft.zh-CN.md) | 需求草案（已 superseded） | Superseded |

## 9. 约束

1. 未通过 Phase A 冻结，不进入编码。
2. 任何协议变更必须先改文档再改实现。
3. 不在 v0.2 实施中回写 v0.1 行为语义。

## 10. 验收

1. 实施者可以据此直接排期和拆分任务。
2. 任一阶段失败都有明确风险处置条件。
3. 可作为 RC 审查清单的直接来源。

## 11. 关联文档

1. [总览](./APPFS-v0.2-总览.zh-CN.md)
2. [接口规范](./APPFS-v0.2-接口规范.zh-CN.md)
3. [后端架构](./APPFS-v0.2-后端架构.zh-CN.md)
4. [能力分级](./APPFS-v0.2-能力分级.zh-CN.md)
5. [非功能性需求](./APPFS-v0.2-非功能性需求.zh-CN.md)
6. [合同测试 CT2](./APPFS-v0.2-合同测试CT2.zh-CN.md)
7. [RC迁移与上线包](./APPFS-v0.2-RC迁移与上线包.zh-CN.md)
