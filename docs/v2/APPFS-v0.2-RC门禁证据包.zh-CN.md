# APPFS v0.2 RC 门禁证据包

- 版本：`v0.2-rc-final`
- 状态：`Completed (Phase E, 2026-03-22)`
- 依赖文档：
  - [APPFS-v0.2-实施计划.zh-CN.md](./APPFS-v0.2-实施计划.zh-CN.md)
  - [APPFS-v0.2-合同测试CT2.zh-CN.md](./APPFS-v0.2-合同测试CT2.zh-CN.md)
  - [APPFS-v0.2-真实App对接规范.zh-CN.md](./APPFS-v0.2-真实App对接规范.zh-CN.md)

## 1. 目标与适用范围

1. 固化 Phase E RC 审查门禁证据格式，避免依赖聊天记录与临时口头说明。
2. 适用于：
   - 发布评审（是否进入放量）；
   - 事故后复盘（是否回退/降级）；
   - 新 app 接入的 RC 合规检查。
3. 本文只定义证据口径与模板，不改协议语义，不引入 runtime 新功能。

## 2. 当前 RC 基线说明

1. Required Gate（阻塞发布）：
   - v0.2：`CT2-001..CT2-009`（Linux）。
   - v0.1：baseline smoke（static + live）。
2. Informational（不阻塞，但必须可解释）：
   - `CT2-010` 跨平台最小一致性矩阵。
   - bridge-path 信号（尤其 gRPC continue-on-error 路径）。
3. 审查输入至少包含：
   - CI 门禁结果；
   - 关键日志/事件/指标；
   - 波次执行与处置时间线。

## 3. Required Gate Evidence（必须项）

### 3.1 CT2 Required（001..009）

| 检查项 | 执行入口 | 预期结果 | 证据产物 |
|---|---|---|---|
| CT2-001..009 | `APPFS_V2_CONTRACT_TESTS=1 APPFS_V2_STRICT=1 ./tests/test-appfs-v2-contract.sh` | `pass=9/9 required` 且 exit=0 | 命令输出、CI job 链接、执行时间 |

### 3.2 v0.1 baseline smoke

| 检查项 | 执行入口 | 预期结果 | 证据产物 |
|---|---|---|---|
| static contract suite | `APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 ./tests/test-appfs-contract.sh` | exit=0 | 命令输出、失败项=0 |
| live contract suite | `APPFS_CONTRACT_TESTS=1 ./tests/appfs/run-live-with-adapter.sh` | exit=0 | 命令输出、关键步骤摘要 |

### 3.3 Required 失败处置约束

1. 任何 required 失败 = 不得继续放量。
2. required 失败后只能三选一：
   - 修复后重跑并补齐证据；
   - 降级并重新评审；
   - 回退到上一个稳定批次。

## 4. Informational Evidence（非阻塞但必须可追踪）

### 4.1 CT2-010 跨平台最小一致性

1. 建议命令（按 #44 口径）：
   - `sh tests/appfs-v2/test-ct2-010-cross-platform-minimal.sh`
   - `APPFS_V2_CT2_010_REFERENCE_OUT=<linux-ref.json> ...`
   - `APPFS_V2_CT2_010_REFERENCE=<linux-ref.json> ...`
2. 最小覆盖点：
   - ActionLineV2 accept/reject basics；
   - snapshot/live dual-shape；
   - event/error surface 最小一致性；
   - Windows 反斜杠路径归一化。
3. informational 失败不直接阻塞发布，但必须给出差异解释与处置计划。

### 4.2 bridge-path signals

| 信号 | 建议来源 | 用途 | 阻塞级别 |
|---|---|---|---|
| HTTP bridge 合同链路 | Rust CI `AppFS Contract Gate (linux, http bridge)` | 验证 bridge 模式端到端可用性 | 建议视为 required 辅助信号 |
| gRPC bridge 合同链路 | Rust CI `AppFS Contract Gate (linux, grpc bridge)` | 长期观测 gRPC 路径稳定度 | informational |

## 5. RC 阈值表（固化口径）

> 默认阈值适用于 RC 审查；单 app 可在 issue 中声明更严格阈值，但不得更宽松。

| 指标 | 默认阈值 | 观测窗口 | 超阈动作 |
|---|---|---|---|
| Required Gate 成功率 | 100% | 每次发布候选执行 | 立即阻断发布 |
| 动作成功率（action.completed / total） | >= 99.0% | 最近 24h | 冻结放量，进入定界 |
| 超时率（timeout / total） | <= 1.0% | 最近 24h | 启动降级或回退评估 |
| 限流率（rate-limited / total） | <= 2.0% | 最近 24h | 调整配额并限制扩量 |
| 失败码 TopN 稳定性 | 无新增未知 blocker 错误码 | 最近 24h | 标记风险并补救后复核 |
| 全量后稳定观察 | >= 24h | Wave 全量后 | 未达窗口不得宣告收口 |

## 6. 证据清单模板（可直接复制）

### 6.1 日志样本模板

```md
### Log Evidence
- 时间窗口：<start> ~ <end>
- 过滤条件：app=<app>, tenant=<tenant>, trace_id=<trace_id>
- 样本：
  - [INFO] ...
  - [WARN] ...
  - [ERROR] ...
```

### 6.2 事件流对照模板

```md
### Event Stream Diff
- 请求：<request_id>
- 预期终态：action.completed | action.failed
- 实际终态：<type>
- 对照：
  - expected: ...
  - actual: ...
```

### 6.3 指标截图/记录模板

```md
### Metrics Snapshot
- 采样时间：<ts>
- 成功率：<value>
- 超时率：<value>
- 限流率：<value>
- 失败码 TopN：<code1,count1> <code2,count2> ...
- 仪表盘链接：<url>
```

### 6.4 时间线记录模板

```md
### Timeline
- T0  : 开始灰度 wave=<wave>
- T+5 : 观察到 <signal>
- T+8 : 执行 <degrade/rollback action>
- T+15: 结果 <stable/unstable>
- T+30: 结论 <continue/pause/rollback>
```

## 7. 波次执行记录模板

| wave | app | tenant scope | resource-type | start | end | gate result | decision | owner |
|---|---|---|---|---|---|---|---|---|
| wave-0 | <app> | internal | snapshot/live | <ts> | <ts> | pass/fail | continue/pause/rollback | <name> |
| wave-1 | <app> | small tenants | action/write | <ts> | <ts> | pass/fail | continue/pause/rollback | <name> |

## 8. CT2-010 informational diff/report 示例

### 8.1 JSON 示例

```json
{
  "platform": "windows",
  "reference": "linux-ref.json",
  "diffs": [
    "observed.live_mode: current='live' reference='live'",
    "observed.error_code: current='PAGER_HANDLE_NOT_FOUND' reference='PAGER_HANDLE_NOT_FOUND'"
  ],
  "decision": "accepted_with_note",
  "owner": "appfs-oncall"
}
```

### 8.2 差异报告模板

| 字段 | Linux reference | Current | 差异说明 | 结论 |
|---|---|---|---|---|
| observed.live_mode | `live` | `<value>` | `<none|reason>` | pass/fail |
| observed.error_code | `PAGER_HANDLE_NOT_FOUND` | `<value>` | `<none|reason>` | pass/fail |
| observed.windows_normalized_path | `/chats/chat-001/messages.res.jsonl` | `<value>` | `<none|reason>` | pass/fail |

## 9. RC 结论页模板（审批页）

```md
# AppFS v0.2 RC Gate Conclusion

- 发布候选版本：<sha/tag>
- 审查时间：<ts>
- 审查范围：<apps/tenants/waves>

## Gate Summary
- Required: pass/fail（附链接）
- Informational: stable/unstable（附解释）
- Rollout Evidence: complete/incomplete

## Decision
- [ ] 继续放量（Continue Rollout）
- [ ] 暂停观察（Pause and Observe）
- [ ] 回退（Rollback）

## Approvals
- Release Owner: <name/sign/time>
- Runtime Owner: <name/sign/time>
- Oncall: <name/sign/time>
```

## 10. 三类证据边界（required / informational / rollout）

1. required 证据：用于“是否允许发布/放量”，结论必须二元（pass/fail）。
2. informational 证据：用于“风险识别与解释”，可带注释通过，但必须记录处置计划。
3. rollout 证据：用于“执行质量与可追溯性”，必须覆盖波次、时间线、处置动作。
4. 三类证据不得互相替代：
   - informational 绿灯不能覆盖 required 红灯；
   - rollout 执行完整不能覆盖 required 失败。

## 11. 最小交付清单（RC 审查必备）

1. Required gate 结果链接与原始输出。
2. Informational（至少 CT2-010）报告与差异说明。
3. 指标阈值判定表（最近 24h）。
4. 波次执行记录与时间线。
5. RC 结论页（含审批信息）。

## 12. 关联文档

1. [APPFS-v0.2-实施计划.zh-CN.md](./APPFS-v0.2-实施计划.zh-CN.md)
2. [APPFS-v0.2-合同测试CT2.zh-CN.md](./APPFS-v0.2-合同测试CT2.zh-CN.md)
3. [APPFS-v0.2-真实App对接规范.zh-CN.md](./APPFS-v0.2-真实App对接规范.zh-CN.md)
