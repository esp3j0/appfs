# AppFS

面向 shell-first AI agent 的文件系统原生应用协议。

[English README](README.md)

AppFS 的目标是把不同应用统一成一套文件系统交互模型，让 agent 用一致命令操作不同 app：

1. 用 `cat` 读取资源。
2. 用 `>> *.act`（append JSONL）触发动作。
3. 用 `tail -f` 订阅异步事件流。

本仓库当前包含 AppFS 规范、适配器契约、参考夹具、一致性测试，以及基于 AgentFS 的 runtime 实现。

## 为什么是 AppFS

核心设计面向 LLM + bash 的实际操作：

1. 不再为每个 App 记一套 MCP 参数格式。
2. 路径即语义，token 开销更低。
3. 流优先的异步模型，支持重放。
4. 运行时自动生成 request_id，模型不用自己造 UUID。
5. 契约冻结后，可跨语言实现适配器。

## 核心交互模型

```bash
# 1) 先订阅事件流
tail -f /app/aiim/_stream/events.evt.jsonl

# 2) 以 append ActionLineV2 JSONL 触发动作
printf '{"version":2,"client_token":"msg-001","payload":{"text":"hello"}}\n' >> /app/aiim/contacts/zhangsan/send_message.act

# 3) 直接读取资源
cat /app/aiim/contacts/zhangsan/profile.res.json

# 4) snapshot 资源是完整文件（.res.jsonl），live 资源继续分页
cat /app/aiim/chats/chat-001/messages.res.jsonl | rg "hello"
cat /app/aiim/feed/recommendations.res.json
printf '{"version":2,"client_token":"page-001","payload":{"handle_id":"<from-page>"}}\n' >> /app/aiim/_paging/fetch_next.act
```

## 可用动作（AIIM 夹具）

事实来源：`examples/appfs/aiim/_meta/manifest.res.json`。

1. `contacts/{contact_id}/send_message.act`
   - `kind`: `action`
   - `execution_mode`: `inline`
   - `input_mode`: `json`
2. `files/{file_id}/download.act`
   - `kind`: `action`
   - `execution_mode`: `streaming`
   - `input_mode`: `json`
3. `/_paging/fetch_next.act`
   - `kind`: `action`
   - `execution_mode`: `inline`
   - `input_mode`: `json`
4. `/_paging/close.act`
   - `kind`: `action`
   - `execution_mode`: `inline`
   - `input_mode`: `json`
5. `/_snapshot/refresh.act`
   - `kind`: `action`
   - `execution_mode`: `inline`
   - `input_mode`: `json`

## 运行时快速开始（HTTP Bridge）

这个 runtime demo 有五个组成部分：

1. AgentFS 挂载
2. 将 AIIM fixture 拷入挂载树
3. 启动 HTTP bridge connector
4. 启动 `agentfs serve appfs` backend runtime
5. 用单独终端 append `.act` 并 tail `_stream/events.evt.jsonl`

如果缺少第 4 步，`.act` 写入不会被消费。只有 mount 和 bridge 还不够。

### Windows（PowerShell，5 步）

1. 挂载 AgentFS（终端 A）。

```powershell
cd C:\Users\esp3j\rep\agentfs\cli
cargo run -- init win-real
cargo run -- mount .agentfs\win-real.db C:\mnt\win-real --foreground
```

2. 把 AIIM fixture 放到挂载点（终端 B）。

```powershell
cd C:\Users\esp3j\rep\agentfs
Copy-Item -Recurse -Force .\examples\appfs\aiim C:\mnt\win-real\aiim
```

3. 启动 HTTP bridge（终端 C）。

```powershell
cd C:\Users\esp3j\rep\agentfs\examples\appfs\http-bridge\python
uv run python bridge_server.py
```

4. 启动 AppFS backend runtime（终端 D）。

```powershell
cd C:\Users\esp3j\rep\agentfs\cli
$env:APPFS_ADAPTER_HTTP_ENDPOINT = "http://127.0.0.1:8080"
cargo run -- serve appfs --root C:\mnt\win-real --app-id aiim
```

预期启动信号：

```text
AppFS adapter using HTTP bridge endpoint: http://127.0.0.1:8080
AppFS adapter started for ...
```

5. 操作文件并观察事件（终端 E）。

```powershell
# 订阅事件流（单独终端）
Get-Content C:\mnt\win-real\aiim\_stream\events.evt.jsonl -Wait

# 触发动作（append ActionLineV2 JSONL，一行一个 JSON 对象）
Add-Content C:\mnt\win-real\aiim\contacts\zhangsan\send_message.act '{"version":2,"client_token":"msg-001","payload":{"text":"hello"}}'

# snapshot 资源可直接检索
Get-Content C:\mnt\win-real\aiim\chats\chat-001\messages.res.jsonl | Select-String "hello"

# live 资源继续分页
Get-Content C:\mnt\win-real\aiim\feed\recommendations.res.json -Raw
Add-Content C:\mnt\win-real\aiim\_paging\fetch_next.act '{"version":2,"client_token":"page-001","payload":{"handle_id":"ph_live_7f2c"}}'
Add-Content C:\mnt\win-real\aiim\_paging\close.act '{"version":2,"client_token":"page-close-001","payload":{"handle_id":"ph_live_7f2c"}}'

# 显式触发 snapshot 刷新（缓存/物化检查点）
Add-Content C:\mnt\win-real\aiim\_snapshot\refresh.act '{"version":2,"client_token":"refresh-001","payload":{"resource_path":"/chats/chat-001/messages.res.jsonl"}}'

# 读取资源
Get-Content C:\mnt\win-real\aiim\contacts\zhangsan\profile.res.json -Raw
```

### Linux（bash，5 步）

1. 挂载 AgentFS（终端 A）。

```bash
cd /path/to/agentfs/cli
cargo run -- init linux-real
mkdir -p /tmp/appfs-real
cargo run -- mount .agentfs/linux-real.db /tmp/appfs-real --foreground
```

2. 把 AIIM fixture 放到挂载点（终端 B）。

```bash
cd /path/to/agentfs
cp -R ./examples/appfs/aiim /tmp/appfs-real/aiim
```

3. 启动 HTTP bridge（终端 C）。

```bash
cd /path/to/agentfs/examples/appfs/http-bridge/python
uv run python bridge_server.py
```

4. 启动 AppFS backend runtime（终端 D）。

```bash
cd /path/to/agentfs/cli
APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 cargo run -- serve appfs --root /tmp/appfs-real --app-id aiim
```

预期启动信号：

```text
AppFS adapter using HTTP bridge endpoint: http://127.0.0.1:8080
AppFS adapter started for ...
```

5. 操作文件并观察事件（终端 E）。

```bash
# 订阅事件流（单独终端）
tail -f /tmp/appfs-real/aiim/_stream/events.evt.jsonl

# 触发动作（append ActionLineV2 JSONL）
printf '{"version":2,"client_token":"msg-001","payload":{"text":"hello"}}\n' >> /tmp/appfs-real/aiim/contacts/zhangsan/send_message.act

# snapshot 资源可直接检索
cat /tmp/appfs-real/aiim/chats/chat-001/messages.res.jsonl | rg "hello"

# live 资源继续分页
cat /tmp/appfs-real/aiim/feed/recommendations.res.json
printf '{"version":2,"client_token":"page-001","payload":{"handle_id":"ph_live_7f2c"}}\n' >> /tmp/appfs-real/aiim/_paging/fetch_next.act
printf '{"version":2,"client_token":"page-close-001","payload":{"handle_id":"ph_live_7f2c"}}\n' >> /tmp/appfs-real/aiim/_paging/close.act

# 显式触发 snapshot 刷新（缓存/物化检查点）
printf '{"version":2,"client_token":"refresh-001","payload":{"resource_path":"/chats/chat-001/messages.res.jsonl"}}\n' >> /tmp/appfs-real/aiim/_snapshot/refresh.act

# 读取资源
cat /tmp/appfs-real/aiim/contacts/zhangsan/profile.res.json
```

注意：

1. `.act` 统一为 append-only JSONL：使用 `>>`（或 PowerShell `Add-Content`）提交，一行一个 ActionLineV2 JSON 对象。
2. `serve appfs` 必须运行，`.act` 写入才会被消费。仅有 mount 和 bridge 不会处理动作文件。
3. 对 `.act` 使用 `>` 覆写/截断会被视为非法变更，runtime 只记录诊断日志并跳过该批内容。
4. 运行时语义为 `at-least-once`，建议业务层基于 `client_token`/`request_id` 做幂等去重。
5. runtime 会兼容 shell 展开导致的多行 JSON 片段，并尝试合并相邻行恢复为单次请求；推荐写法仍是单行 JSON，并在字符串中使用转义 `\\n`。

## 架构

### v0.2 Backend + Connector 调用链

```mermaid
flowchart TD
    A["Agent shell / PowerShell / bash"] --> B["AgentFS 挂载（Windows: WinFsp，Linux: FUSE）"]
    B --> C["AppFS 树（_meta、_stream、_paging、_snapshot、业务路径）"]
    C --> D["agentfs serve appfs（v0.2 backend runtime）"]

    D --> E["Action Dispatcher（ActionLineV2 解析与校验）"]
    D --> F["Snapshot Cache Manager（prewarm、read-through、stale）"]
    D --> G["Journal + Recovery"]
    D --> H["Event Engine + Paging"]

    D --> I["Connector transport"]
    I --> J["进程内 adapter"]
    I --> K["HTTP bridge adapter"]
    I --> L["gRPC bridge adapter"]

    K --> M["HTTP bridge service"]
    L --> N["gRPC bridge service"]
    J --> O["真实 app backend 或 reference demo backend"]
    M --> O
    N --> O

    H --> C
    F --> C
    G --> C
```

### v0.2 中 `serve appfs` 的职责

`cargo run -- serve appfs --root ... --app-id ...` 启动的是 AppFS backend runtime。当前实现里它仍然是一个长驻进程，内部有 poll/event loop，但它已经不再是 v0.1 那种薄 sidecar。

在 v0.2 里它负责：

1. 装载 manifest、action spec、snapshot spec、paging 控制与运行策略；
2. 选择并初始化 connector transport（进程内 / HTTP bridge / gRPC bridge）；
3. 执行 ActionLineV2 校验与 submit-time reject；
4. 驱动 snapshot prewarm、read-through、timeout fallback、journal recovery 与 paging；
5. 把事件、重放、cursor 与物化资源文件回写到挂载树。

换句话说：

1. **v0.1** 的 `serve appfs`：更接近 sidecar/reference runtime，主要围绕 action sink 和 bridge 转发。
2. **v0.2** 的 `serve appfs`：是承接 AppFS 协议语义的 backend runtime，connector 只负责 app-specific 的上游调用。

## 一致性快速开始

### 1) 静态契约检查

```bash
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT="$PWD/../examples/appfs" sh ./tests/test-appfs-contract.sh
```

### 2) Live 一致性（进程内适配器）

Linux + FUSE 环境：

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

建议阅读顺序：

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
4. 常见排障基线统一在开发指南中维护。

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
11. `cli/tests/appfs/`：live 契约与韧性测试（`CT-001` 到 `CT-022`）。

## 当前状态

当前分支已包含 AppFS v0.1 契约套件与 RC 收口产物，包括：

1. 发布检查清单与发布说明。
2. RC 收口记录。
3. 进程内与 bridge 模式的 static/live 一致性门禁。

发布相关文档：

1. [APPFS-release-checklist-v0.1-rc1.md](docs/v1/APPFS-release-checklist-v0.1-rc1.md)
2. [APPFS-release-notes-v0.1-rc1.md](docs/v1/APPFS-release-notes-v0.1-rc1.md)
3. [APPFS-rc-closure-v0.1.md](docs/v1/APPFS-rc-closure-v0.1.md)
4. [APPFS-v0.1.0-rc2-freeze.md](docs/v1/APPFS-v0.1.0-rc2-freeze.md)
5. [APPFS-migration-note-v0.1.0-rc2.md](docs/v1/APPFS-migration-note-v0.1.0-rc2.md)
6. [APPFS-project-status-and-roadmap-2026-03-17.md](docs/v1/APPFS-project-status-and-roadmap-2026-03-17.md)

## 许可证

MIT
