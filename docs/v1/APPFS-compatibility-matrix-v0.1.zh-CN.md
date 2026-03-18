# AppFS 兼容性矩阵 v0.1（中文）

- 版本：`0.1`
- 日期：`2026-03-17`
- 状态：`Draft`
- 范围：适配器实现语言 x 传输方式 x 能力级别

## 1. 阅读规则

1. `Core` 表示 AppFS v0.1 的必需兼容声明级别。
2. `Recommended` 表示在 Core 之上通过推荐能力检查（若在 manifest 声明 observer/progress-policy）。
3. `Extension` 表示在 Core 之上通过应用/厂商扩展校验。
4. 最小验收命令保持 shell-first，并与 CI 对齐。

## 2. 矩阵

| 语言 | 传输 | Core（最小验收命令） | Recommended（最小验收命令） | Extension（最小验收命令） |
|---|---|---|---|---|
| Rust | in-process | `cd examples/appfs && sh ./run-conformance.sh inprocess` | Core 命令 + `cat /app/<app_id>/_meta/manifest.res.json` 并校验 `conformance.recommended` 与实现一致 | Recommended 命令 + 扩展专项合约脚本（示例：`sh ./tests/appfs/test-<extension>.sh`） |
| Rust | HTTP bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core 命令 + observer/progress-policy 元数据校验 | Recommended 命令 + 扩展专项合约脚本 |
| Rust | gRPC bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_GRPC_ENDPOINT=http://127.0.0.1:50051 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core 命令 + observer/progress-policy 元数据校验 | Recommended 命令 + 扩展专项合约脚本 |
| Python | in-process | `N/A`（v0.1 的 runtime in-process 适配面仅 Rust） | `N/A` | `N/A` |
| Python | HTTP bridge | `cd examples/appfs && sh ./run-conformance.sh http-python` | Core 命令 + `cat /app/<app_id>/_meta/manifest.res.json` 并校验 recommended profile 字段 | Recommended 命令 + 扩展专项合约脚本 |
| Python | gRPC bridge | `cd examples/appfs && sh ./run-conformance.sh grpc-python` | Core 命令 + observer/progress-policy 元数据校验 | Recommended 命令 + 扩展专项合约脚本 |
| Go | in-process | `N/A`（v0.1 的 runtime in-process 适配面仅 Rust） | `N/A` | `N/A` |
| Go | HTTP bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core 命令 + manifest recommended-profile 校验 | Recommended 命令 + 扩展专项合约脚本 |
| Go | gRPC bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_GRPC_ENDPOINT=http://127.0.0.1:50051 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core 命令 + manifest recommended-profile 校验 | Recommended 命令 + 扩展专项合约脚本 |
| TypeScript | in-process | `N/A`（v0.1 的 runtime in-process 适配面仅 Rust） | `N/A` | `N/A` |
| TypeScript | HTTP bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core 命令 + manifest recommended-profile 校验 | Recommended 命令 + 扩展专项合约脚本 |
| TypeScript | gRPC bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_GRPC_ENDPOINT=http://127.0.0.1:50051 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core 命令 + manifest recommended-profile 校验 | Recommended 命令 + 扩展专项合约脚本 |

## 3. CI 分层映射（Required vs Informational）

Required CI：

1. static contract + live in-process（`appfs-contract-gate`）
2. live HTTP bridge（`appfs-contract-gate-http-bridge`）

Informational CI：

1. live gRPC bridge（`appfs-contract-gate-grpc-bridge`，允许失败但必须上报信号）

## 4. 兼容性声明最小证据

任一矩阵单元要声明 Core，至少提供：

1. 最小验收命令输出
2. 合约套件汇总（`CT-001` 到 `CT-017`）
3. manifest conformance block 快照

声明 Recommended/Extension 还需：

1. Core 证据
2. manifest 中声明的 recommended/extensions 列表
3. 每个声明项对应的额外脚本或日志证据
