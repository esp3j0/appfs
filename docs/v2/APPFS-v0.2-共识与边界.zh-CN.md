# APPFS v0.2 共识与边界（认知同步稿）

- 日期：`2026-03-20`
- 状态：`Frozen (Phase A, 2026-03-20)`
- 目的：对当前团队认知进行一致性收口，作为 Phase A 文档冻结入口

## 1. 项目定位共识

1. AgentFS 是底层存储与挂载引擎，核心状态落在 SQLite。
2. AppFS 是构建在 AgentFS 之上的应用层文件协议。
3. 二者关系是“存储引擎 + 应用交互协议”，不是互相替代。

## 2. 交互模型共识

1. Agent 通过文件操作与应用交互（`cat/printf/tail`）。
2. 事件流是主要反馈通道，不依赖状态轮询文件。
3. `request_id` 由服务端生成，`client_token` 用于业务关联。

## 3. 节点语义共识

1. `*.res.jsonl`：snapshot 全文件语义，面向检索。
2. `*.res.json`：live 分页语义，面向动态浏览。
3. `*.act`：append-only 动作接收器，**仅支持 JSONL 行协议**。
4. `*.evt.jsonl`：事件流。

## 4. v0.1 与 v0.2 边界共识

1. v0.1 定位为 demo/reference，不再承接架构级升级。
2. v0.2 定位为 backend-native 主路线。
3. v0.2 的核心问题不是“动作提交”，而是“读路径能力”：
   - 启动预热（snapshot 元信息）
   - 读穿缓存（read miss 自动扩容）
   - 并发去重与原子扩展

## 5. `/_snapshot/refresh.act` 共识

1. v0.2 阶段不直接删除 `/_snapshot/refresh.act`。
2. 该节点在 v0.2 中作为显式控制入口保留。
3. 最终语义在接口冻结时二选一：
   - 显式重校验
   - 强制重物化

## 6. Connector 共识

1. v0.2 必须有 Connector 层，用于对接真实 app。
2. Connector 负责把上游 REST/gRPC/SDK 映射到 AppFS 统一模型。
3. Core 不应感知具体 app API 细节。

## 7. 能力分级共识（Core/Recommended/Optional）

## 7.1 Core（必须实现）

1. `.act` JSONL 提交与边界保障。
2. snapshot 全文件语义与读命中能力。
3. live 分页与句柄控制。
4. 统一错误码与事件语义。
5. CT2 Core required 用例通过。

## 7.2 Recommended（推荐实现）

1. 启动预热（prewarm）。
2. 读 miss 自动扩容（read-through）。
3. 并发去重、恢复一致性、`cache.expand/cache.stale` 事件。

## 7.3 Optional（按 app 选择）

1. 高级观测指标与成本估算。
2. 额外控制动作（如专用 refresh 变体）。
3. 特殊领域增强能力（如向量检索、推荐重排）。

## 8. 当前阶段共识

1. 当前处于 Phase A 文档冻结阶段。
2. 先完成 v0.2 文档闭环，再进入编码。
3. 未冻结的接口项不得实现。

## 9. 与现有文档关系

1. 本文是“共识入口文档”，不替代协议细节文档。
2. 详细接口见：[APPFS-v0.2-接口规范.zh-CN.md](./APPFS-v0.2-接口规范.zh-CN.md)。
3. 架构细节见：[APPFS-v0.2-后端架构.zh-CN.md](./APPFS-v0.2-后端架构.zh-CN.md)。
4. 测试门禁见：[APPFS-v0.2-合同测试CT2.zh-CN.md](./APPFS-v0.2-合同测试CT2.zh-CN.md)。

## 10. 验收

1. 团队成员可据本文快速判断认知是否一致。
2. 本文中的共识项在 v0.2 设计评审中默认生效。
3. 与本文冲突的提案需先更新本文再进入评审。
