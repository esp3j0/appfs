# APPFS v0.2 RC 迁移与上线包

- 版本：`v0.2-rc-final`
- 状态：`Completed (Phase E, 2026-03-22)`
- 依赖文档：
  - [APPFS-v0.2-实施计划.zh-CN.md](./APPFS-v0.2-实施计划.zh-CN.md)
  - [APPFS-v0.2-合同测试CT2.zh-CN.md](./APPFS-v0.2-合同测试CT2.zh-CN.md)
  - [APPFS-v0.2-完成总结-2026-03-22.zh-CN.md](./APPFS-v0.2-完成总结-2026-03-22.zh-CN.md)

## 1. 目标与适用范围

1. 将 Phase E 的迁移、灰度、回退、处置策略落为可执行操作包。
2. 支持三类场景：
   - 现有 app 能力灰度（按租户/资源类型）；
   - RC 审查与发布决策。
3. 不新增协议功能，不改变 CT2 语义，仅定义上线运营策略。

## 2. 发布基线（RC 门槛）

### 2.1 功能与测试基线

1. Linux required：`CT2-001..CT2-009` 全绿。
2. `v0.1 baseline smoke` 通过，且无新增阻塞回归。
3. `CT2-010` 处于 informational 稳定观察状态（可追踪）。

### 2.2 运行与运营基线

1. Oncall 值班表、升级路径、事故频道已就绪。
2. 最小可观测字段已接入：`request_id`、`client_token`、`trace_id`、`connector_id`。
3. 关键指标可查询：成功率、失败码分布、扩容延迟、限流触发率。

## 3. 迁移前置条件（Migration Prerequisites）

### 3.1 App 级前置

1. 完成目标 app 资源建模（snapshot/live/action）与路径约定。
2. 完成上游错误码到 v0.2 错误码映射清单。
3. 完成鉴权与凭证刷新策略验证（含失效回退）。

### 3.2 Connector 级前置

1. health 检查可稳定返回。
2. 写路径可独立暂停（不影响读与审计）。
3. 具备最小降级开关（见第 5 节能力降级矩阵）。

### 3.3 发布前检查

1. 回放最近 7 天同类故障样本，确认处置路径可执行。
2. 确认回退目标版本、回退窗口与负责人。
3. 确认本次灰度波次清单（app/tenant/resource-type）已审批。

## 4. Rollout Strategy（灰度策略）

### 4.1 分批维度

1. 按 app 分批：先低风险 app，再核心 app。
2. 按 tenant 分批：先内部租户，再小流量租户，再全量租户。
3. 按 resource-type 分批：先 snapshot/read，再 live/paging，最后 action/write。

### 4.2 建议波次

1. Wave 0（影子验证）：0% 外部流量，仅跑只读探测与事件证据。
2. Wave 1（读路径灰度）：开启 snapshot/live 读能力，不开写路径。
3. Wave 2（受控写入）：对小租户开启 action/write，观察错误分布。
4. Wave 3（扩量）：按租户批次逐步放量到目标比例。
5. Wave 4（稳定观察）：全量后观察窗口不少于 24h。

### 4.3 放量门禁

1. 放量前：当前波次 required 检查项全部通过。
2. 放量中：错误率、超时率、限流率未超过阈值。
3. 放量后：关键路径无 Sev-1/Sev-2 新增事故。

## 5. Rollback / Degradation Policy（回退与降级）

### 5.1 回退触发条件

1. required 路径出现连续失败且无法在窗口内止血。
2. 出现数据一致性风险（事件重放不一致、终态冲突）。
3. 上游 connector 大范围鉴权或配额异常。

### 5.2 回退动作（顺序执行）

1. 立即暂停受影响 connector 写路径（action submit）。
2. 将受影响 app/tenant 回切到上一稳定发布批次。
3. 保留审计与事件采集，不中断证据链。
4. 按回退预案恢复读能力并验证核心链路。

### 5.3 能力降级矩阵（优先“可用但受限”）

1. 读穿扩容高风险：降级为仅返回已物化 snapshot（必要时返回标准失败）。
2. live 分页异常：降级为保守分页窗口，必要时限制 `fetch_next`。
3. 写路径高风险：仅关闭 action/write，保留 read 与观测能力。

## 6. Operator Checklist（运维执行清单）

### 6.1 发布前（T-1）

1. 核对版本、配置、回退目标与回退命令。
2. 核对灰度名单（app/tenant/resource-type）。
3. 核对告警阈值、看板链接、值班联系人。

### 6.2 发布中（T0）

1. 按波次执行放量并记录开始/结束时间。
2. 每波次固定采样：成功率、P95、失败码 TopN。
3. 达到停止条件时立即冻结放量并进入第 7 节处置流程。

### 6.3 发布后（T+1）

1. 输出波次复盘（变化项、风险项、处置项）。
2. 更新 app 接入状态表（未开始/灰度中/稳定）。
3. 归档证据链接，供 RC 审查复用。

## 7. Incident Handling（问题处置策略）

### 7.1 分级与响应

1. Sev-1：核心路径不可用或一致性高风险，5 分钟内升级并触发回退。
2. Sev-2：部分租户/能力受影响，15 分钟内止血并给出处置计划。
3. Sev-3：可绕过问题，纳入后续修复批次。

### 7.2 标准处置流程

1. 发现：告警或人工发现后创建 incident 记录。
2. 定界：明确 app、tenant、resource-type、时间窗口。
3. 止血：优先暂停写路径或能力降级。
4. 修复：最小变更修复并在灰度环境复验。
5. 恢复：逐步恢复流量并持续观测。
6. 复盘：产出根因、影响面、改进项与 owner。

### 7.3 最小证据要求

1. 错误日志片段（含 request_id/trace_id）。
2. 关键事件流样本（前后对比）。
3. 指标截图（事故前/中/后）。
4. 执行动作时间线（暂停、降级、回退、恢复）。

## 8. 已知风险与缓解

1. snapshot/live 语义污染：按资源类型分波次放量，异常立即冻结。
2. 事件与重放不一致：保留事件证据，优先执行回退而非强推修复。
3. 上游限流或配额波动：提前准备降级策略与重试预算。
4. 多租户隔离误配：先内部租户验证，再扩到外部租户。

## 9. Migration Issue 生成规则

### 9.1 拆分原则

1. 按 `app x tenant-wave x resource-type` 生成 issue，避免超大任务。
2. 每个 issue 必须绑定一个文档锚点与一个验收标准。
3. 先生成 required 路径 issue，再生成扩展与优化 issue。

### 9.2 命名规则

`[Phase E][<app>] <tenant-wave> <resource-type> migration/rollout`

示例：`[Phase E][aiim] wave-2 tenant-small action-write migration/rollout`

### 9.3 必填字段

1. 引用文档锚点（本文件章节 + 对应协议/CT2）。
2. rollout 波次、起止时间、owner、回退 owner。
3. 验收标准（required 指标阈值 + CT2 条目）。
4. 风险与回退动作（明确到可执行命令或开关）。

### 9.4 Issue 模板

```md
- 标题：[Phase E][<app>] <tenant-wave> <resource-type> migration/rollout
- 引用文档：
  - APPFS-v0.2-RC迁移与上线包.zh-CN.md#<章节>
  - APPFS-v0.3-实施计划.zh-CN.md
- 范围：
  - app: <app>
  - tenant wave: <wave>
  - resource type: <snapshot|live|action>
- 验收标准：
  - [ ] CT2 required 相关条目保持通过
  - [ ] 发布门禁指标达标
  - [ ] v0.1 baseline smoke 无回归
- 回退策略：
  - [ ] connector 写路径暂停开关可执行
  - [ ] capability degrade 开关可执行
  - [ ] 回退目标版本与负责人明确
```

## 10. RC 审查输出物清单

1. 波次执行记录（按 app/tenant/resource-type）。
2. 关键指标趋势与阈值判定结果。
3. 事故与处置记录（含复盘）。
4. 迁移 issue 清单与状态矩阵。
5. 发布结论：继续放量 / 暂停观察 / 回退。

## 11. 关联文档

1. [APPFS-v0.2-实施计划.zh-CN.md](./APPFS-v0.2-实施计划.zh-CN.md)
2. [APPFS-v0.2-合同测试CT2.zh-CN.md](./APPFS-v0.2-合同测试CT2.zh-CN.md)
3. [APPFS-v0.3-实施计划.zh-CN.md](../v3/APPFS-v0.3-实施计划.zh-CN.md)
