# APPFS v0.2 后端模式需求草案

> Superseded by v0.2 formal document set.
已由以下文档集合替代，请优先阅读：
1. [APPFS-v0.2-总览.zh-CN.md](./APPFS-v0.2-总览.zh-CN.md)
2. [APPFS-v0.2-接口规范.zh-CN.md](./APPFS-v0.2-接口规范.zh-CN.md)
3. [APPFS-v0.2-后端架构.zh-CN.md](./APPFS-v0.2-后端架构.zh-CN.md)
4. [APPFS-v0.2-合同测试CT2.zh-CN.md](./APPFS-v0.2-合同测试CT2.zh-CN.md)
5. [APPFS-v0.2-实施计划.zh-CN.md](./APPFS-v0.2-实施计划.zh-CN.md)

- 日期：`2026-03-20`
- 状态：`Superseded`
- 范围：Backend-native AppFS 执行模型

## 1. 为什么是 v0.2（而不是直接改 v0.1）

这次升级的核心原因不是普通优缺点权衡，而是语义预期与现状存在本质差异：

1. 预期模型：AppFS 适配层位于文件系统后端执行路径（backend 模式）。
2. 预期能力：运行时可以感知 `*.res.jsonl` 的正常读请求。
3. 预期能力：当读取超过缓存范围时，运行时可在读路径触发上游 API 拉取并扩展缓存。
4. 当前 v0.1 参考实现是 sidecar 轮询模型（`agentfs serve appfs`），无法把"读路径回源扩容"作为后端级强保证。

因此该变更属于版本级语义升级，按 `v0.2` 处理更合理。

## 2. v0.2 目标能力

后端模式必须提供以下能力：

1. 启动预热（Prewarm）
   - 启动时读取 manifest 中声明的 snapshot 资源。
   - 调用上游 size/metadata API 获取基础信息。
   - 初始化本地缓存状态与限额边界。
2. 读路径拦截（Read Interception）
   - 对 `*.res.jsonl` 读取请求获取 `offset/length`。
   - 判断请求区间是否已物化到本地缓存。
3. 读穿式缓存扩展（Read-through Cache Growth）
   - 若读取超出缓存区间，触发上游拉取并扩展本地缓存。
   - 扩展后继续返回读取结果。
4. 限额与错误一致性
   - 严格执行 `snapshot.max_materialized_bytes`。
   - 超限必须稳定映射为 `SNAPSHOT_TOO_LARGE`。

## 3. 架构需求

### 3.1 数据路径

1. snapshot 资源（`*.res.jsonl`）仍保持"文件优先"语义，兼容 `cat/rg/grep/sed`。
2. 文件内容由可增量物化的缓存提供。
3. 读请求可触发上游补拉，不再仅依赖离线预生成。

### 3.2 缓存元数据

每个 snapshot 资源建议维护：

1. `resource_path`
2. `upstream_revision`（或等价版本标识）
3. `materialized_bytes`
4. `max_materialized_bytes`
5. `state`（`cold/warming/hot/stale/error`）
6. `updated_at`

### 3.3 拉取与物化策略

1. 支持上游分页 API 与字节区间 API 两类输入。
2. 统一转换为 JSONL 物化格式。
3. 采用原子发布（`tmp + rename` 或事务边界）。
4. 禁止向读者暴露半条记录或半文件状态。

### 3.4 并发控制

1. 同一资源并发读取不得触发风暴式重复拉取。
2. 需要资源级 in-flight 去重锁。
3. 读 miss 时的策略（阻塞/返回旧缓存/异步补拉）必须在协议中显式定义。

### 3.5 恢复能力

1. 重启后可恢复缓存元数据和未完成拉取状态。
2. 对不完整缓存段要能检测并修复或隔离。

## 4. 合同层影响（v0.2 草案）

1. snapshot 仍保持 `*.res.jsonl` 全量文件语义。
2. 新增 v0.2 能力标识：支持 backend read-through。
3. `/_snapshot/refresh.act` 语义需明确：
   - 选项 A：显式重校验；
   - 选项 B：强制重物化。
4. 事件模型延续 `action.completed/action.failed`，并可补充缓存来源字段。

## 5. 测试增量（相对 v0.1）

建议新增 v0.2 合同测试：

1. 启动预热会调用声明资源的 metadata/size API。
2. 读取超出缓存时触发上游拉取并扩展缓存。
3. 并发读取去重（同窗口只拉取一次）。
4. 中断场景原子性（不返回半成品）。
5. 读路径扩容触发超限时稳定返回 `SNAPSHOT_TOO_LARGE`。
6. 重启后可恢复部分物化状态并继续服务。

## 6. 实施前置条件

在开始编码前必须先冻结：

1. v0.2 后端需求文档；
2. v0.2 合同测试增量与验收标准；
3. 迁移策略文档：
   - v0.1 sidecar 作为 demo/reference；
   - v0.2 backend 作为主路线；
   - 两者共存与切换窗口策略。

## 7. 编码阶段顺序建议

1. 先实现 backend hook（读拦截 + 缓存元数据存储）。
2. 再实现 fetch/materialize 引擎。
3. 再补可观测性与错误映射。
4. 每阶段以合同测试作为门禁。

## 8. 开放问题

> **已决策**：参见 v0.2 正式文档集中的解决方案

| 问题 | 决策 | 参考文档 |
|------|------|----------|
| 启动预热是默认强制还是 manifest 可选开关？ | 默认强制 + manifest 可覆盖 | [接口规范](./APPFS-v0.2-接口规范.zh-CN.md) |
| 读 miss 时，默认阻塞等待还是可返回旧缓存？ | 阻塞等待 + 超时降级 | [接口规范](./APPFS-v0.2-接口规范.zh-CN.md) |
| 若上游无 revision 能力，版本一致性如何定义？ | 分层策略（revision > last_modified > ttl_only） | [接口规范](./APPFS-v0.2-接口规范.zh-CN.md) |
| 缓存元数据应存放在核心 SQLite 表还是独立存储层？ | 核心 SQLite 表 | [后端架构](./APPFS-v0.2-后端架构.zh-CN.md) |

## 9. 本草案非目标

1. 暂不定义最终 Rust trait 签名。
2. 暂不定义最终表结构。
3. 不修改 v0.1 运行行为。
