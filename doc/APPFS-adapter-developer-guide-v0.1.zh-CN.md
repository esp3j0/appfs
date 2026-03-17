# AppFS 适配器开发指南 v0.1（中文）

- 版本：`0.1`
- 日期：`2026-03-17`
- 状态：`Draft`
- 读者：适配器实现者（Rust/Python/Go/TS）、运行时集成人员

## 1. 目标

这份文档是 AppFS 适配器开发的中文入口。

完成本指南后，你应当可以：

1. 在本地跑通 `init -> submit -> stream -> paging`。
2. 通过 `CT-001 ~ CT-017`。
3. 快速定位失败原因，并给出兼容性声明所需证据。

## 2. 建议阅读顺序

1. 协议基线：`doc/APPFS-v0.1.md`
2. 适配器要求：`doc/APPFS-adapter-requirements-v0.1.md`
3. 本文：实现路径与排障
4. 一致性与测试定义：
   - `doc/APPFS-conformance-v0.1.md`
   - `doc/APPFS-contract-tests-v0.1.md`
5. 兼容性矩阵：
   - `doc/APPFS-compatibility-matrix-v0.1.md`

## 3. 30 分钟最小闭环

```bash
# 1) 静态合约检查
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT="$PWD/../examples/appfs" sh ./tests/test-appfs-contract.sh

# 2) live（in-process）
cd ../examples/appfs
sh ./run-conformance.sh inprocess

# 3) live（HTTP bridge，包含 CT-017）
sh ./run-conformance.sh http-python
```

说明：

1. 静态模式覆盖布局/Schema/策略类校验（`CT-001`、`CT-003`、`CT-005`）。
2. live 模式覆盖动作、流、分页、安全与恢复（`CT-002` 到 `CT-017`）。
3. HTTP/gRPC bridge 模式用于验证跨传输的一致性。

## 4. 选择实现路径

### 4.1 Rust in-process

适用场景：

1. 你直接修改 runtime。
2. 你希望最短调试链路（单进程）。

参考：

1. `sdk/rust/src/appfs_adapter.rs`
2. `sdk/rust/src/appfs_demo_adapter.rs`
3. `examples/appfs/adapter-template/rust-minimal/`

### 4.2 HTTP bridge（多语言优先）

适用场景：

1. 你要用 Python/Go/TS 实现业务逻辑。
2. 你需要独立部署与重启。

参考：

1. `examples/appfs/http-bridge/python/`
2. `doc/APPFS-adapter-http-bridge-v0.1.md`

### 4.3 gRPC bridge

适用场景：

1. 你希望更强类型约束。
2. 团队需要共享 proto 合约。

参考：

1. `examples/appfs/grpc-bridge/python/`
2. `doc/APPFS-adapter-grpc-bridge-v0.1.md`

## 5. 适配器必须保证的行为

### 5.1 Action 提交

1. `.act` 以 `write+close` 作为提交边界。
2. close-time 校验失败时，不得发出 `action.accepted`。
3. 已接受请求必须且仅有一个终态事件。

### 5.2 流与重放

1. 每个请求内事件因果顺序稳定。
2. 保持 `event_id`、`request_id` 语义一致。
3. 与 `cursor`、`from-seq` 重放机制协同。

### 5.3 分页

1. `cat *.res.json` 返回首页并携带 `handle_id`。
2. `/_paging/fetch_next.act` 返回下一页 envelope。
3. `/_paging/close.act` 幂等且结果可预测。

### 5.4 安全与可移植性

1. 非法路径必须在副作用前被拒绝。
2. 路径分段命名满足跨平台约束（含 Windows）。
3. 超长 handle 归一化规则稳定且可复现。

## 6. Bridge 韧性参数

常用参数：

1. `APPFS_ADAPTER_BRIDGE_MAX_RETRIES`
2. `APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS`
3. `APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS`
4. `APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES`
5. `APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS`

CT-017 相关：

1. `APPFS_BRIDGE_RESILIENCE_CONTRACT=1`
2. `APPFS_BRIDGE_FAULT_CONFIG_PATH`

## 7. CI 分层（required / informational）

建议基线：

1. Required：`appfs-contract-gate`、`appfs-contract-gate-http-bridge`
2. Informational：`appfs-contract-gate-grpc-bridge`（`continue-on-error`）

## 8. 常见问题排障

### 8.1 `Address already in use`

处理步骤：

1. `ss -ltnp | grep ':8080'`
2. `APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:9000 sh ./run-conformance.sh`
3. 确认脚本会将 endpoint 映射到 bridge 实际监听端口。

### 8.2 Python 3.12 报 `Start directory is not importable: tests`

处理步骤：

1. 确保存在 `tests/__init__.py`
2. 使用：
   - `uv run python -m unittest discover -s tests -t . -p "test_*.py"`

### 8.3 gRPC 报 `ModuleNotFoundError: grpc`

处理步骤：

1. `python3 -m pip install -r requirements.txt`
2. `./generate_stubs.sh`

### 8.4 CT-017 失败（缺少 `action.failed` 或断路器未开启）

处理步骤：

1. 检查 resilience 环境变量是否生效。
2. 检查 fault path prefix 是否匹配探测路径。
3. 查看日志关键字：`retry`、`circuit opened`、`short-circuit`。
4. 确认 cooldown 不低于最小阈值（默认 `4000ms`）。

### 8.5 live 挂载失败或卡住

处理步骤：

1. 检查 Linux FUSE 依赖。
2. 清理残留 mountpoint 与旧进程。
3. 查看日志：
   - `cli/appfs-mount-live.log`
   - `cli/appfs-adapter-live.log`

## 9. 兼容性声明最小清单

声明 `AppFS v0.1 Core` 前，至少满足：

1. `CT-001 ~ CT-017` 全通过。
2. 适配器要求文档清单项有证据。
3. `manifest` 含 conformance block。
4. required CI 全绿。

## 10. 下一步建议

1. 增加一个真实 app 连接器（不只 mock）。
2. 继续完善脚手架（`examples/appfs/new-adapter.sh`）。
3. 按兼容性矩阵逐步补齐 Go/TS 参考实现。
