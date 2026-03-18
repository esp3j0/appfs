# AppFS v0.1 合约测试计划（中文）

- 版本：`0.1-draft-r10`
- 日期：`2026-03-17`
- 状态：`Draft`
- 依赖：`APPFS-v0.1 (r8)`、`APPFS-adapter-requirements-v0.1`

## 1. 目的

本计划定义 AppFS v0.1 可执行的合约检查。

目标：

1. 将规范中的 MUST 条款转化为可重复测试。
2. 为 runtime 与 adapter 变更提供稳定门槛。
3. 保持 shell-first，贴合 LLM + bash 使用方式。

## 2. 测试入口

执行器：

```bash
cd cli
APPFS_CONTRACT_TESTS=1 ./tests/test-appfs-contract.sh
```

静态 fixture 模式（无需 live runtime）：

```bash
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT=/mnt/c/Users/esp3j/rep/agentfs/examples/appfs ./tests/test-appfs-contract.sh
```

可选聚合执行器：

```bash
cd cli
APPFS_CONTRACT_TESTS=1 ./tests/all.sh
```

Linux CI 门禁（GitHub Actions）：

1. 静态 fixture 门禁：

```bash
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT=$GITHUB_WORKSPACE/examples/appfs sh ./tests/test-appfs-contract.sh
```

2. live mount + adapter 门禁：

```bash
APPFS_CONTRACT_TESTS=1 sh ./tests/appfs/run-live-with-adapter.sh
```

3. live HTTP bridge 门禁：

```bash
APPFS_CONTRACT_TESTS=1 \
APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 \
APPFS_ADAPTER_BRIDGE_MAX_RETRIES=1 \
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES=2 \
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS=1200 \
APPFS_BRIDGE_RESILIENCE_CONTRACT=1 \
sh ./tests/appfs/run-live-with-adapter.sh
```

4. live gRPC bridge 门禁：

```bash
APPFS_CONTRACT_TESTS=1 \
APPFS_ADAPTER_GRPC_ENDPOINT=http://127.0.0.1:50051 \
APPFS_ADAPTER_BRIDGE_MAX_RETRIES=1 \
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES=2 \
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS=1200 \
APPFS_BRIDGE_RESILIENCE_CONTRACT=1 \
sh ./tests/appfs/run-live-with-adapter.sh
```

## 3. 环境变量输入

| 变量 | 默认值 | 说明 |
|---|---|---|
| `APPFS_CONTRACT_TESTS` | `0` | 设为 `1` 启用 AppFS 合约测试 |
| `APPFS_ROOT` | `/app` | 挂载的 AppFS 根目录 |
| `APPFS_APP_ID` | `aiim` | `/app` 下 app id |
| `APPFS_TEST_ACTION` | `/app/aiim/contacts/zhangsan/send_message.act` | action 测试使用的 sink |
| `APPFS_PAGEABLE_RESOURCE` | `/app/aiim/chats/chat-001/messages.res.json` | 分页测试使用的资源 |
| `APPFS_TIMEOUT_SEC` | `10` | 异步断言等待超时 |
| `APPFS_STATIC_FIXTURE` | `0` | 设为 `1` 只跑 fixture 静态检查 |
| `APPFS_BRIDGE_RESILIENCE_CONTRACT` | `0` | bridge 模式下设为 `1` 启用 `CT-017`（重试/断路/恢复） |
| `APPFS_BRIDGE_RESILIENCE_CONTACT_PREFIX` | `resilience-` | `CT-017` 多 sink 探测使用的联系人前缀 |
| `APPFS_BRIDGE_FAULT_CONFIG_PATH` | `/tmp/appfs-bridge-fault-config.json` | runtime 写入的 bridge 故障配置文件（用于 `CT-017` 可重复注入） |
| `APPFS_BRIDGE_RESILIENCE_MIN_BREAKER_COOLDOWN_MS` | `4000` | `CT-017` 强制的最小断路冷却时间，防止竞态 |

## 4. 合约测试套件

说明：`cli/tests/appfs/` 当前包含 CT-001 到 CT-016 的直接脚本，`run-live-with-adapter.sh` 还会额外执行生命周期探针（CT-016）和可选 bridge 韧性探针（CT-017）。下面先列基线 CT-001~CT-005；同一执行器还会覆盖扩展 live 检查（`CT-006` 流生命周期、`CT-007` close-time 拒绝、`CT-008` 提交顺序、`CT-009` 分页错误映射、`CT-010`/`CT-011` 提交原子性/中断、`CT-012` 路径安全、`CT-013` 重复消费、`CT-014` 并发提交压力、`CT-015` 长句柄归一化、`CT-016` 重启对账、`CT-017` bridge 重试/断路/恢复容错）。

### CT-001 布局与必需节点

规范引用：

1. `APPFS-v0.1` 第 4 节。
2. `APPFS-v0.1` 第 13 节。

断言：

1. 必需文件存在（`manifest`、`context`、`permissions`、`events`、`cursor`、`from-seq`、`_paging/fetch_next.act`、`_paging/close.act`）。
2. `manifest` 包含 `app_id` 与 `nodes`。

脚本：

```text
cli/tests/appfs/test-layout.sh
```

### CT-002 Action Sink 语义

规范引用：

1. `APPFS-v0.1` 第 7 节。
2. `APPFS-v0.1` 第 8 节。

断言：

1. 对 `.act` 执行 `write+close` 成功。
2. 动作提交后事件流增长。
3. `.act` 读（`cat`）失败。
4. `.act` 追加写（`>>`）失败。
5. 新终态事件包含 `request_id` 与 `type`（若系统有 `jq`）。

脚本：

```text
cli/tests/appfs/test-action-basics.sh
```

### CT-003 流重放与 Cursor

规范引用：

1. `APPFS-v0.1` 第 8 节（replay/resume）。

断言：

1. `cursor.res.json` 包含 `min_seq`、`max_seq`、`retention_hint_sec`。
2. `from-seq/<seq>.evt.jsonl` 对有效序号返回数据。
3. `from-seq/<min_seq-1>.evt.jsonl` 在早于保留窗口时失败。

脚本：

```text
cli/tests/appfs/test-stream-replay.sh
```

### CT-004 分页句柄协议

规范引用：

1. `APPFS-v0.1` 第 11 节。

断言：

1. 对可分页资源 `cat` 返回 `{items, page}`。
2. 存在 `page.handle_id`。
3. `fetch_next.act` 接受 `handle_id`。
4. 事件流包含分页动作的完成事件。
5. `close.act` 接受 `handle_id`。

脚本：

```text
cli/tests/appfs/test-paging.sh
```

### CT-005 Manifest 策略检查

规范引用：

1. `APPFS-v0.1` 第 5 节。
2. `APPFS-v0.1` 第 13 节。

断言：

1. 节点名不包含禁止路径模式（`..`、反斜杠、盘符）。
2. action 节点声明期望字段（`input_mode`、`execution_mode`）。
3. 可分页资源声明 `paging` 元数据。

脚本：

```text
cli/tests/appfs/test-manifest-policy.sh
```

### CT-017 Bridge 容错（重试/断路/恢复）

规范引用：

1. `APPFS-adapter-requirements-v0.1`（`AR-019`，韧性基线）。
2. `agentfs serve appfs` 的 bridge 韧性选项。

断言：

1. 可重试传输失败会触发有界重试（在适配器日志中可见）。
2. 连续可重试失败达到阈值后，断路器对新请求进行短路。
3. 断路窗口内，请求仍收到确定性的终态失败事件。
4. 冷却时间过后，健康请求无需重启 runtime 即可恢复成功。

入口：

```text
cli/tests/appfs/run-live-with-adapter.sh (通过 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 启用)
```

## 5. 缺口与后续（v0.2 候选）

当前 shell 黑盒测试尚未完全覆盖：

1. runtime 在进入 backend 前的 unsafe segment 预拒绝（需要更底层测试钩子）。
2. 崩溃/重试模拟下 `at-least-once` 重复投递行为。
3. 适配器间生成 ID 的分段缩短哈希确定性。

建议后续：

1. 在 runtime crate 增加 SDK 级与单元级测试，覆盖路径归一化与 guard 顺序。
2. 增加面向流持久化与重放的故障注入测试框架。
