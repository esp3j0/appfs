# AppFS

面向 shell-first AI agent 的文件系统原生应用协议，由 AgentFS 提供底层能力。

[English README](README.md)

AppFS 把不同应用统一成一套文件系统契约，让 agent 始终使用同一种操作方式：

- 用 `cat` 读取资源
- 用 `>> *.act` 以 JSONL 追加方式触发动作
- 用 `tail -f` 订阅异步事件流

本仓库包含 AppFS 协议文档、runtime、参考夹具、bridge adapter 和一致性测试。AppFS 是面向应用侧的协议与 managed runtime，AgentFS 则提供底层的存储、overlay、sync 与挂载能力。

## 简介

AppFS 主要面向真实的 LLM + shell 工作流：

- 不再为每个 app 记一套不同的 schema
- 路径即语义，token 开销更低
- 流优先的异步交互模型，支持重放
- managed runtime lifecycle，支持动态注册 app
- connector 可走 in-process、HTTP、gRPC 三条路径

## Powered by AgentFS

当前仓库的主叙事已经是 AppFS，但它仍然通过 AgentFS 引擎和 CLI 交付：

- `agentfs init ...` 负责准备底层 AgentFS 数据库与存储层
- `agentfs appfs up ...` 负责在这个 AgentFS-backed filesystem 之上启动 AppFS managed runtime
- 目前仍使用 `agentfs` 作为 CLI 入口，是因为 AppFS 依赖 AgentFS 的数据库生命周期、overlay 语义、sync 能力与跨平台挂载后端

也就是说：AppFS 是面向用户的协议与交互层，AgentFS 是在底下提供能力的引擎。

面向真实项目集成时，推荐的高层启动入口是：

```bash
agentfs appfs compose up -f appfs-compose.yaml
```

底层 managed runtime 原语仍然是：

```bash
agentfs appfs up <id-or-path> <mountpoint>
```

managed runtime 的共享状态文件位于：

```text
/_appfs/apps.registry.json
```

底层调试入口仍然保留：

- `agentfs mount ... --managed-appfs`
- `agentfs serve appfs --managed`

## Quick Start

现在更推荐的高层 AppFS 流程是：

1. 在 `appfs-compose.yaml` 中声明 runtime、connectors、apps
2. 运行 `agentfs appfs compose up`
3. 直接使用挂载树

底层 managed 流程仍然保留：

1. 启动 bridge 或进程内 connector
2. 初始化一个空的 AgentFS 数据库，作为 AppFS 的存储与挂载底座
3. 用 `agentfs appfs up` 启动 AppFS
4. 通过 `/_appfs/register_app.act` 注册 app
5. 在挂载树中直接读文件、切换 scope、触发动作

也就是说，当前更推荐 compose-first 的集成路径；`agentfs appfs up` 继续保留给需要直接调试 mount/runtime 行为的场景。

一个 Huoyan attached-case 的 compose 示例位于 [examples/appfs/appfs-compose.huoyan-attached-case.example.yaml](./examples/appfs/appfs-compose.huoyan-attached-case.example.yaml)。

下面是参考 HTTP bridge 的最小 compose 结构：

```yaml
version: 1

runtime:
  db: ./.agentfs/compose-aiim.db
  mountpoint: C:/mnt/appfs-compose-aiim
  backend: winfsp
  init: if_missing
  reset: false

connectors:
  aiim-http:
    mode: command
    transport: http
    endpoint: http://127.0.0.1:8080
    healthcheck:
      kind: connector
      interval_ms: 500
      timeout_ms: 2000
      max_attempts: 40
    command:
      cwd: ./examples/appfs/bridges/http-python
      program: uv
      args: ["run", "python", "bridge_server.py"]

apps:
  aiim:
    connector: aiim-http
```

有了这个文件之后，直接运行：

```bash
agentfs appfs compose up -f appfs-compose.yaml
```

compose 是当前推荐的主路径，因为它会在一个前台进程里完成：

- 准备或复用 AgentFS runtime 数据库
- 拉起并监管 external / command 模式的 connector
- 预填充 managed AppFS registry
- 挂载树并启动 runtime

环境前置：

- 已安装 Rust toolchain，且 `cargo` 可用
- 参考 HTTP bridge 需要 Python 和 `uv`
- `127.0.0.1:8080` 端口可用
- Windows：已安装 WinFsp
- Linux：具备 FUSE 能力
- macOS：具备 NFS 挂载能力

### Windows

请先安装 WinFsp。AppFS 在 Windows 上通过 `--backend winfsp` 使用 WinFsp 作为挂载后端。

- 从 [winfsp.dev/rel](https://winfsp.dev/rel/) 下载并安装最新版本的 WinFsp
- 安装完成后，请重新打开一个终端，再运行 `agentfs appfs up`
- 通常不需要重启；如果安装后仍然提示驱动忙碌，或者挂载还是失败，再重启一次后重试
- 对 WinFsp 来说，挂载点路径本身应该不存在。保留父目录，例如 `C:\mnt`，但不要提前创建 `C:\mnt\appfs-compose-aiim`。现在 `appfs compose up` 已兼容这个约束；如果旧版本运行残留了一个空目录占位符，也会在启动时自动清理掉。

Windows 下的 compose 启动方式：

```powershell
cd C:\Users\esp3j\rep\agentfs\cli
cargo run -- appfs compose up -f ..\examples\appfs\appfs-compose.huoyan-attached-case.example.yaml
```

如果目标软件本身已经处在案件或其他附着 scope 内，推荐像 Huoyan 示例那样，通过 compose 里的 connector 环境变量传入 attached-case bootstrap 信息，而不是再额外触发一次 `enter_scope`。

启动参考 HTTP bridge：

```powershell
cd C:\Users\esp3j\rep\agentfs\examples\appfs\bridges\http-python
uv run python bridge_server.py
```

初始化空数据库：

```powershell
cd C:\Users\esp3j\rep\agentfs\cli
cargo run -- init managed-http --force
```

启动 AppFS：

```powershell
cd C:\Users\esp3j\rep\agentfs\cli
cargo run -- appfs up .agentfs\managed-http.db C:\mnt\appfs-managed-http --backend winfsp
```

注册 app：

```powershell
Add-Content C:\mnt\appfs-managed-http\_appfs\register_app.act '{"app_id":"aiim","transport":{"kind":"http","endpoint":"http://127.0.0.1:8080","http_timeout_ms":5000,"grpc_timeout_ms":5000,"bridge_max_retries":2,"bridge_initial_backoff_ms":100,"bridge_max_backoff_ms":1000,"bridge_circuit_breaker_failures":5,"bridge_circuit_breaker_cooldown_ms":3000},"client_token":"reg-http-001"}'
```

读取 snapshot 并触发动作：

```powershell
Get-Content C:\mnt\appfs-managed-http\aiim\chats\chat-001\messages.res.jsonl | Select-Object -First 5
Add-Content C:\mnt\appfs-managed-http\aiim\contacts\zhangsan\send_message.act '{"version":2,"client_token":"msg-001","payload":{"text":"hello"}}'
```

### Linux

启动参考 HTTP bridge：

```bash
cd /path/to/agentfs/examples/appfs/bridges/http-python
uv run python bridge_server.py
```

初始化空数据库：

```bash
cd /path/to/agentfs/cli
cargo run -- init managed-http --force
```

启动 AppFS：

```bash
cd /path/to/agentfs/cli
mkdir -p /tmp/appfs-managed-http
cargo run -- appfs up .agentfs/managed-http.db /tmp/appfs-managed-http --backend fuse
```

注册 app：

```bash
echo '{"app_id":"aiim","transport":{"kind":"http","endpoint":"http://127.0.0.1:8080","http_timeout_ms":5000,"grpc_timeout_ms":5000,"bridge_max_retries":2,"bridge_initial_backoff_ms":100,"bridge_max_backoff_ms":1000,"bridge_circuit_breaker_failures":5,"bridge_circuit_breaker_cooldown_ms":3000},"client_token":"reg-http-001"}' >> /tmp/appfs-managed-http/_appfs/register_app.act
```

读取 snapshot 并触发动作：

```bash
head -n 5 /tmp/appfs-managed-http/aiim/chats/chat-001/messages.res.jsonl
echo '{"version":2,"client_token":"msg-001","payload":{"text":"hello"}}' >> /tmp/appfs-managed-http/aiim/contacts/zhangsan/send_message.act
```

### macOS

macOS 使用 AgentFS 的 NFS backend，而不是 FUSE。AppFS 的 managed 流程保持一致，但新的 runtime 行为仍建议优先在 Linux 上做首轮验证。

初始化空数据库：

```bash
cd /path/to/agentfs/cli
cargo run -- init managed-http --force
```

启动 AppFS：

```bash
cd /path/to/agentfs/cli
mkdir -p /tmp/appfs-managed-http
cargo run -- appfs up .agentfs/managed-http.db /tmp/appfs-managed-http --backend nfs
```

后续注册 app、触发动作、读取 snapshot 的方式与上面的 Windows / Linux 流程一致。

## 手动编译

### 环境依赖

在一台全新的 Windows 机器上，运行 `cargo build` 之前需要先安装 Rust toolchain 和 Visual Studio Build Tools。

如果你看到：

```text
error: linker `link.exe` not found
```

通常说明当前 shell 里还没有加载 MSVC 的编译环境。

1. 安装 Rust

```powershell
winget install --id Rustlang.Rustup -e
rustup default stable-x86_64-pc-windows-msvc
```

2. 安装 Visual Studio Build Tools

```powershell
# 下载 Visual Studio Build Tools 安装器
Invoke-WebRequest -Uri https://aka.ms/vs/17/release/vs_buildtools.exe -OutFile vs_buildtools.exe

# 安装 C++ Build Tools workload（不带完整 IDE）
Start-Process -Wait -FilePath .\vs_buildtools.exe -ArgumentList "--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

3. 单独安装官方 LLVM Windows 发行版

请从 LLVM 官方发布页下载最新的 Windows 安装包 `LLVM-*-win64.exe`：

- [LLVM 官方发布页](https://github.com/llvm/llvm-project/releases)

安装时如果看到“Add LLVM to PATH”之类的选项，建议勾上。默认安装目录通常是：

```text
C:\Program Files\LLVM\bin
```

4. 确保 MSVC 和 LLVM 编译器目录已经在 `PATH` 中

重新打开一个终端后，再执行：

```powershell
$MsvcBin = Get-ChildItem "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC" -Directory |
    Sort-Object Name -Descending |
    Select-Object -First 1 |
    ForEach-Object { Join-Path $_.FullName "bin\Hostx64\x64" }

$LlvmBin = "C:\Program Files\LLVM\bin"

$env:Path = "$MsvcBin;$LlvmBin;$env:Path"

where.exe cl
where.exe link
where.exe clang-cl
```

在常见安装路径下，这两个目录通常会长这样：

```text
C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\<version>\bin\Hostx64\x64
C:\Program Files\LLVM\bin
```

如果 `where.exe cl` 或 `where.exe link` 仍然找不到结果，先检查上面的 MSVC 目录是否存在。不存在的话，说明 Build Tools 没有安装完整，需要重新安装。

如果 `where.exe clang-cl` 仍然找不到结果，说明 LLVM 还没有安装好，或者没有加入 `PATH`。这时请先检查 `C:\Program Files\LLVM\bin\clang-cl.exe` 是否存在，再把 `C:\Program Files\LLVM\bin` 加入 `PATH`。

对 `cargo build` 来说，最关键的条件其实就是 `cl.exe`、`link.exe` 和 `clang-cl.exe` 都能通过 `PATH` 被找到。

### CLI 与 Rust SDK

```bash
cd cli
cargo build
cargo build --release
cargo test --package agentfs
```

```bash
cd sdk/rust
cargo test
```

### TypeScript SDK

```bash
cd sdk/typescript
npm install
npm run build
npm test
```

### Python SDK

```bash
cd sdk/python
uv sync
uv run pytest
uv run ruff check .
uv run ruff format .
```

## 文档

推荐先看这些入口：

- [文档总索引](docs/README.md)
- [当前 AppFS 文档主入口（v4）](docs/v4/README.md)
- [Connectorization 里程碑（v3）](docs/v3/README.md)
- [Backend-native 里程碑（v2）](docs/v2/README.md)
- [examples/appfs 使用说明](examples/appfs/README.md)
- [cli/TEST-WINDOWS.md](cli/TEST-WINDOWS.md)
- [Runtime 收口设计计划](docs/plans/2026-03-26-appfs-runtime-closure-design.md)

## 架构

AppFS 现在按三层组织：

- AgentFS Core：AppFS 之下的引擎层，包括 SQLite 文件系统、通用 overlay 语义、sync 与平台挂载后端
- AppFS Engine：registry、structure sync、runtime lifecycle、snapshot read-through、connector adapter
- AppFS UX：`agentfs appfs up` 和挂载树上的 `/_appfs/*` 控制面

```mermaid
flowchart TD
    A["Shell / PowerShell / bash"] --> B["agentfs appfs up"]
    B --> C["AgentFS Core"]
    B --> D["AppFS Engine"]
    D --> E["/_appfs/apps.registry.json"]
    C --> F["挂载后的 AppFS 树"]
    D --> F
    D --> G["AppTreeSyncService"]
    D --> H["AppConnector"]
    G --> H
    H --> I["in-process / HTTP / gRPC adapters"]
    I --> J["真实 app backend 或 demo backend"]
```

补充说明：

- `AppConnector` 是运行时 canonical connector surface
- `_meta/manifest.res.json` 是派生视图，不是运行态主真相
- snapshot cold miss 会在普通文件读取时自动扩展
- 当前 CLI 分层是有意为之：AppFS 以 `agentfs` 子命令形式暴露，因为它仍依赖 AgentFS 的存储与挂载基础设施
- `agentfs init --base` 仍保留为 AgentFS 功能，但不属于推荐的 AppFS 主路径

## 仓库结构

- `cli/`：`agentfs` CLI，本身同时包含 AppFS 子命令、runtime 与挂载集成
- `sdk/rust/`：Rust SDK 与文件系统实现
- `sdk/typescript/`：TypeScript SDK
- `sdk/python/`：Python SDK
- `sandbox/`：Linux-only syscall interception sandbox
- `examples/appfs/`：AppFS 示例，按 `fixtures/`、`bridges/`、`templates/`、`legacy/` 分层
- `docs/`：ADR、计划、契约与发布文档

## 测试

核心验证入口：

- Rust CLI 测试：`cargo test --manifest-path cli/Cargo.toml --package agentfs`
- Rust SDK 测试：`cargo test --manifest-path sdk/rust/Cargo.toml`
- Linux 合同测试：`cli/tests/test-appfs-connector-contract.sh`
- Windows managed lifecycle 回归：`cli/test-windows-appfs-managed.ps1`

Linux 仍然是主要 required CI 平台；Windows 已补齐 managed AppFS 流程的专门手动回归脚本。

## 当前状态

当前仓库状态：

- `v0.3` connectorization 已合入，并保持 release baseline
- `v0.4` structure sync、统一 connector、managed lifecycle 和 `appfs up` 已在树内实现
- multi-app managed runtime 已可用
- 真实 app pilot 是稳定发布前的下一步关键验证

## 许可证

MIT
