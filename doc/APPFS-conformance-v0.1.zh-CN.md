# AppFS 一致性配置 v0.1（中文）

- 版本：`0.1`
- 日期：`2026-03-16`
- 状态：`Draft`
- 依赖：
  - `APPFS-v0.1 (r8)`
  - `APPFS-adapter-requirements-v0.1 (r2)`

## 1. 目的

本文定义实现如何声明 AppFS 兼容性。

它与语言和运行时形态无关：

1. 允许任意实现语言。
2. 允许任意部署形态（进程内、sidecar、daemon、service）。
3. 兼容性以外部行为与合约测试判定，而不是内部设计。

各语言/传输/能力级别的覆盖情况见：

1. `APPFS-compatibility-matrix-v0.1.md`

## 2. 兼容级别

### 2.1 Core Profile（必需）

仅当满足全部 Core 要求，才可声明 **AppFS v0.1 Core compatible**：

1. 必需命名空间与每个 app 的布局。
2. `.act` 的 `write+close` 语义。
3. 含稳定 `event_id` 的流事件契约。
4. 重放面：`cursor` 与 `from-seq`。
5. 分页协议（`fetch_next.act`、`close.act`）及错误映射。
6. 路径安全与可移植性保护。
7. 适配器要求文档定义的原子性与顺序约束。

### 2.2 Recommended Profile（可选）

规范中的可选 `SHOULD` 项可声明为 **Recommended compatible**。

例如：

1. Observer 面。
2. 进度节奏提示。
3. 额外搜索投影。

### 2.3 Extension Profile（可选）

允许厂商/应用扩展，但必须满足：

1. Core 行为不变。
2. 未知字段/路径不会破坏 Core 客户端。
3. 扩展键使用命名空间（建议：`x_<vendor>_*`）。

## 3. 版本兼容规则

1. `contract_version` 必须出现在 manifest。
2. `0.1.x` patch 版本在 Core 级必须保持向后兼容。
3. 允许新增字段；删除字段或语义破坏必须升级 minor/major 合约版本。
4. Agent 应忽略未知字段，除非 profile 显式声明其为关键字段。

## 4. 一致性声明

每个 adapter/runtime 建议在 manifest 元数据发布 conformance block：

```json
{
  "conformance": {
    "appfs_version": "0.1",
    "profiles": ["core"],
    "recommended": ["observer", "progress_policy"],
    "extensions": ["x_example_batch_send"],
    "implementation": {
      "name": "aiim-adapter",
      "version": "0.1.0",
      "language": "rust"
    }
  }
}
```

其中 `language` 仅为信息字段，不影响兼容判定。

## 5. 基于测试的合规

### 5.1 最小测试门槛

声明 Core 兼容必须通过：

1. AppFS 静态合约测试（`CT-001`、`CT-003`、`CT-005`）。
2. AppFS live 合约套件（`CT-002`、`CT-004`、`CT-006` 到 `CT-016`）。
3. `APPFS-adapter-requirements-v0.1` 中的适配器验收清单项。
4. CI 必须同时包含 static + live 合约执行（参考 `.github/workflows/rust.yml` 的 `appfs-contract-gate`）。
5. 仓库参考 CI 额外验证进程外传输一致性：`appfs-contract-gate-http-bridge` 与 `appfs-contract-gate-grpc-bridge`。
6. bridge 模式（HTTP/gRPC）建议运行韧性探针（`CT-017`），验证重试/断路器/cooldown 恢复行为。

### 5.2 失败策略

1. 任一 Core MUST 违规即 **不具备 Core 兼容性**。
2. Recommended/Extension 失败不影响 Core 声明。

## 6. 面向 Agent 的互操作规则

Agent 客户端应：

1. 检查 `contract_version` 与 `conformance.profiles`。
2. 当包含 `core` 且必需节点存在时继续执行。
3. 忽略未知扩展字段。
4. 推荐能力缺失时优雅降级。

## 7. 语言与运行时中立性

允许的实现示例：

1. Rust 进程内适配器。
2. Go sidecar 写入流文件。
3. Node.js daemon 桥接应用 API。
4. Python service + 文件系统桥接。

只要通过一致性测试并满足 Core 规则，以上实现兼容性等价。

## 8. 声明流程（实践）

建议适配器作者按以下顺序执行：

1. 在 `manifest.res.json` 写入 `contract_version` 与 `conformance`。
2. 运行静态门槛：
   - `APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 ./tests/test-appfs-contract.sh`
3. 在挂载 AppFS + 适配器运行时下执行 live 门槛：
   - `APPFS_CONTRACT_TESTS=1 APPFS_ROOT=<mounted_app_root> ./tests/test-appfs-contract.sh`
   - `APPFS_CONTRACT_TESTS=1 ./tests/appfs/run-live-with-adapter.sh`
4. 填写适配器验收清单（每项 pass/fail + 证据）。
5. 发布兼容声明：
   - 仅在无 Core MUST 违规时声明 `core`
   - 可选声明 `recommended` 与 `extensions`

建议声明措辞：

```text
该实现声明对 app <app_id> 具备 AppFS v0.1 Core 兼容性，
验证依据为 CT-001/002/003/004/005 及日期为 <date> 的适配器清单证据。
```
