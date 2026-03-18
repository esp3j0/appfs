# AppFS

面向 shell-first AI agent 的文件系统原生应用协议。

[English README](README.md)

AppFS 的目标是把不同应用统一为同一种文件系统交互模型，让 agent 用一致命令操作不同 app：

1. 用 `cat` 读资源。
2. 用 `echo > *.act` 触发动作。
3. 用 `tail -f` 订阅异步事件流。

本仓库当前包含 AppFS 规范、适配器契约、参考夹具、一致性测试，以及基于 AgentFS 的 runtime 实现。

## 核心交互模型

```bash
# 1) 先订阅事件流
tail -f /app/aiim/_stream/events.evt.jsonl

# 2) write+close 触发动作
echo "hello" > /app/aiim/contacts/zhangsan/send_message.act

# 3) 直接读取资源
cat /app/aiim/contacts/zhangsan/profile.res.json

# 4) 统一分页读取长内容
cat /app/aiim/chats/chat-001/messages.res.json
echo '{"handle_id":"<from-page>"}' > /app/aiim/_paging/fetch_next.act
```

## 快速开始

### 1) 静态合约检查

```bash
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT="$PWD/../examples/appfs" sh ./tests/test-appfs-contract.sh
```

### 2) Live 一致性（进程内适配器）

```bash
cd examples/appfs
sh ./run-conformance.sh inprocess
```

### 3) Live 一致性（进程外 bridge）

```bash
cd examples/appfs
sh ./run-conformance.sh http-python
sh ./run-conformance.sh grpc-python
```

## Adapter Developer Path（中文）

从这里开始：

1. [APPFS-adapter-developer-guide-v0.1.zh-CN.md](docs/v1/APPFS-adapter-developer-guide-v0.1.zh-CN.md)
2. [ADAPTER-QUICKSTART.zh-CN.md](examples/appfs/ADAPTER-QUICKSTART.zh-CN.md)
3. [APPFS-adapter-requirements-v0.1.zh-CN.md](docs/v1/APPFS-adapter-requirements-v0.1.zh-CN.md)
4. [APPFS-compatibility-matrix-v0.1.zh-CN.md](docs/v1/APPFS-compatibility-matrix-v0.1.zh-CN.md)
5. [APPFS-conformance-v0.1.zh-CN.md](docs/v1/APPFS-conformance-v0.1.zh-CN.md)
6. [APPFS-contract-tests-v0.1.zh-CN.md](docs/v1/APPFS-contract-tests-v0.1.zh-CN.md)
7. [APPFS-adapter-structure-mapping-v0.1.zh-CN.md](docs/v1/APPFS-adapter-structure-mapping-v0.1.zh-CN.md)

兼容性承诺：

1. 允许任意语言实现，只要协议行为一致。
2. 兼容性以行为与一致性测试结果判定。
3. `v0.1.x` 期间接口面冻结，仅允许向后兼容增量扩展。
4. 常见排障基线统一收敛在开发指南。

## AppFS 相关目录

1. `docs/v1/APPFS-v0.1.md`：核心协议。
2. `docs/v1/APPFS-adapter-requirements-v0.1.md`：适配器要求。
3. `docs/v1/APPFS-adapter-developer-guide-v0.1.md`：英文开发指南。
4. `docs/v1/APPFS-adapter-developer-guide-v0.1.zh-CN.md`：中文开发指南。
5. `docs/v1/APPFS-adapter-structure-mapping-v0.1.md`：结构定义与桥接映射（英文）。
6. `docs/v1/APPFS-adapter-structure-mapping-v0.1.zh-CN.md`：结构定义与桥接映射（中文）。
7. `docs/v1/APPFS-compatibility-matrix-v0.1.md`：兼容性矩阵（英文）。
8. `docs/v1/APPFS-compatibility-matrix-v0.1.zh-CN.md`：兼容性矩阵（中文）。
9. `examples/appfs/`：参考夹具、bridge 示例与脚手架。
10. `cli/src/cmd/appfs.rs`：AppFS runtime 命令实现。
11. `cli/tests/appfs/`：live 合约与韧性测试（`CT-001` 到 `CT-017`）。

## 许可证

MIT
