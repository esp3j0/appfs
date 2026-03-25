# APPFS v0.3 实施计划（Connectorization）

- 版本：`v0.3`
- 状态：`Closed for repository-level connectorization (2026-03-24)`
- 目标：以破坏性升级方式完成 Connector 化收口，使 `in-process`、`HTTP bridge`、`gRPC bridge` 统一到同一套最新 connector 契约。

> 2026-03-24 状态更新：
> - V3-01 ~ V3-08、V3-10 已完成并进入仓库级发布口径。
> - V3-09（真实 app pilot）不作为本次仓库发布收口的宣称项，后续以独立专项推进。
## 1. 背景与决策

1. `v0.2.0` 已完成并发布，属于已收口基线。
2. 现有代码仍存在“文档定义的完整 connector 能力”与“shipping surface 实现”之间差距（尤其是 snapshot read-through 路径）。
3. 因当前尚未对外形成真实 app 兼容包袱，`v0.3` 允许破坏性协议/trait 升级。

## 2. v0.3 范围

### 2.1 In scope

1. 新增并冻结 V2 connector trait（替代 v0.1 adapter 主路径）。
2. runtime 全链路接入 V2 connector：
   - `prewarm_snapshot_meta`
   - `fetch_snapshot_chunk`
   - `fetch_live_page`
   - `submit_action`
   - 对已声明 snapshot `*.res.jsonl` 的普通文件读取 cold miss 自动 read-through 扩容
3. connector 能力面保留并暴露 `health(ctx)`（用于 bridge/connector 可用性与认证状态表达）。
4. HTTP bridge 与 gRPC bridge 同步升级到 V2 协议。
5. CI gate 与 CT2 证据升级，避免“假 bridge 覆盖”。
6. 文档与 README 切换为 v0.3 connector 主线叙事。
7. 至少 1 个真实 app（sandbox/测试租户）完成端到端 pilot 验收。

### 2.2 Out of scope

1. Level 3 optional 能力大规模扩展。
2. 多真实 app 同时接入与跨租户大规模 rollout。
3. 生产大规模放量策略（灰度分波次）仍沿用独立上线流程文档，不在本计划内展开。

## 3. 目标接口（V2）

> 命名可在 ADR 中最终冻结，以下为计划目标能力面。

1. `connector_id()`
2. `health(ctx)`
3. `prewarm_snapshot_meta(resource_path, timeout, ctx)`
4. `fetch_snapshot_chunk(request, ctx)`
5. `fetch_live_page(request, ctx)`
6. `submit_action(request, ctx)`

约束：

1. 不再把 snapshot 扩容主逻辑留在 runtime 内部静态 stub。
2. runtime 负责缓存生命周期、事件语义、恢复逻辑；connector 负责上游数据获取与映射。
3. 三种 transport 的语义必须一致，不能“HTTP 一套 / gRPC 一套 / in-process 一套”。
4. `/_snapshot/refresh.act` 保留为显式控制面，不再作为 snapshot 数据面的唯一触发入口。
5. 已声明 snapshot `*.res.jsonl` 的普通读取 cold miss 现在由 mount 侧自动扩容，当前挂载后端覆盖：
   - Linux `FUSE`
   - macOS `NFS`
   - Windows `WinFsp`

### 3.1 数据一致性约束（新增硬门禁）

1. `fetch_snapshot_chunk` 必须提供可重放、可去重的数据语义：
   - 明确稳定排序键（例如 `updated_at + id` 或等价单调键）；
   - 明确去重主键（记录级唯一标识）；
   - 断点续传时不得出现“重复写入/漏写”。
2. `fetch_live_page` 必须保证 cursor 可恢复：
   - 同一 cursor 重放结果应满足幂等读取预期；
   - `page.mode=live`、`handle_id/page_no/has_more` 语义一致；
   - connector 侧游标失效时必须返回可判定错误码，不得静默回退。
3. runtime 与 connector 的职责边界必须可审计：
   - runtime 管缓存与文件物化；
   - connector 管上游拉取与映射；
   - 不允许在 runtime 中保留“主路径业务数据 stub”。

## 4. 阶段划分

### Phase A：契约冻结

1. 输出 v0.3 ADR 与 connector 接口文档。
2. 明确破坏性升级项、错误码口径、兼容边界。

门禁：

1. 接口/语义冻结后再进入编码。

### Phase B：SDK 与 Runtime 主路径升级

1. 增加 V2 trait + 类型。
2. runtime 改为调用 V2 connector（snapshot/live/action/prewarm）。

门禁：

1. 本地编译与单测通过。
2. in-process 路径 CT2 required 通过。

### Phase C：HTTP/gRPC bridge 升级

1. HTTP bridge 协议与 Rust adapter 升级到 V2。
2. gRPC proto/service/client 升级到 V2。
3. demo connector 实现 V2 全能力面。

门禁：

1. HTTP bridge required 子集全绿。
2. gRPC bridge 达到目标语义（若暂为 informational，需明确升级条件）。
3. 运行条件门禁通过（auth/限流/重试/超时/降级）：
   - `health` 可反映认证状态；
   - `RESOURCE_EXHAUSTED`、`TIMEOUT`、`UNAVAILABLE` 等错误码映射稳定；
   - retry/backoff/circuit 行为有测试证据；
   - 降级路径可观测且不破坏 CT2 语义。

### Phase D：门禁与发布收口

1. CI required/informational 分层稳定。
2. v0.3 完成总结、迁移说明、README 入口更新。
3. runner 命名与版本语义完成迁移方案：
   - 保留 `APPFS_V2_*` 兼容变量（过渡）；
   - 新增 `APPFS_V3_*` 别名并在文档中标注弃用窗口；
   - CI/job 命名与 branch protection 同步更新，避免 pending check 漂移。

门禁：

1. required 全绿且无隐藏 fallback。
2. 文档口径与主线行为一致。

### Phase E：真实 App Pilot 收口（新增）

1. 选择 1 个真实 app sandbox 做首个 connector 接入。
2. 完成 `snapshot + action + live` 最小业务面联调。
3. 输出 24h 稳定性证据与回退预案。

门禁：

1. CT2 required + app-specific E2E 全绿。
2. 关键运行指标在阈值内（成功率、超时率、限流率、失败码 TopN）。
3. 回退开关可执行并完成一次演练记录。

## 5. Issue 拆分（执行清单）

1. V3-01：ADR + V2 契约冻结。
2. V3-02：Rust SDK V2 trait/type 落地。
3. V3-03：runtime 接 V2 connector（snapshot/live/action/prewarm）。
4. V3-04：HTTP bridge V2 协议与 adapter 升级。
5. V3-05：gRPC bridge V2 proto 与 adapter 升级。
6. V3-06：demo connector parity（in-process/http/grpc）。
7. V3-07：CT2/CI gate 升级与防假覆盖校验。
8. V3-08：v0.3 文档与 release 收口。
9. V3-09：真实 App Pilot 验收（sandbox）。
10. V3-10：runner/CI 版本语义迁移（`APPFS_V2_*` -> `APPFS_V3_*` 兼容方案）。

## 6. 建议分工

1. coderB：V3-02 + V3-03（SDK/runtime 主路径）。
2. coderC：V3-04 + V3-05 + V3-06（bridge/demo 路径）。
3. 集成侧：V3-01 + V3-07 + V3-08 + V3-10（ADR/gate/docs/release/命名迁移）。
4. Pilot owner（待指定 app 负责人）：V3-09。

## 7. 风险与防护

1. 风险：仍有 legacy fallback 造成“看起来通过但没走 connector”。
   - 防护：CT2 脚本增加 connector call evidence 断言。
2. 风险：HTTP 与 gRPC 协议漂移。
   - 防护：共享 golden payload/response fixture。
3. 风险：workflow check 名称漂移导致 required pending。
   - 防护：在本阶段冻结 job 名称并同步 branch protection。
4. 风险：完成平台 connector 化后仍无法落地真实 app。
   - 防护：把“1 个真实 app pilot 验收”提升为 DoD 必选项。

## 8. 完成定义（Definition of Done）

1. V2 connector 契约已冻结并实现。
2. in-process/http/grpc 全部走 V2 connector 主路径。
3. required gate 可证明不存在“旧路径兜底”。
4. auth/限流/重试/超时/降级运行条件有自动化门禁证据。
5. `APPFS_V2_*` 到 `APPFS_V3_*` 的 runner/CI 迁移策略已生效且兼容窗口明确。
6. 至少 1 个真实 app pilot 在 sandbox 验收通过（含 24h 稳定性与回退演练记录）。
7. v0.3 文档与 README 反映真实 shipping 行为。
