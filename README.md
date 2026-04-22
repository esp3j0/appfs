# AppFS

Filesystem-native app protocol for shell-first AI agents, powered by AgentFS.

[中文 README](README.zh-CN.md)

AppFS turns different apps into one filesystem contract so agents can use the same primitives everywhere:

- `cat` to read resources
- `>> *.act` to trigger actions with JSONL
- `tail -f` to watch async event streams

This repository contains the AppFS protocol docs, runtime, reference fixtures, bridge adapters, and conformance tests. AppFS is the app-facing protocol and managed runtime; AgentFS is the storage, overlay, sync, and mount engine underneath.

## Overview

AppFS is designed for practical LLM + shell workflows:

- one interaction model across many apps instead of one schema per app
- path-native operations with low token overhead
- stream-first async flows with replay support
- managed runtime lifecycle with dynamic app registration
- connector adapters for in-process, HTTP, and gRPC integrations

## Powered by AgentFS

AppFS is the primary product story in this repository, but it is currently shipped through the AgentFS engine and CLI:

- `agentfs init ...` prepares the underlying AgentFS database and storage layer
- `agentfs appfs up ...` starts the managed AppFS runtime on top of that AgentFS-backed filesystem
- the `agentfs` binary remains the entrypoint because AppFS currently depends on AgentFS database lifecycle, overlay semantics, sync support, and platform mount backends

In other words: AppFS is the app-facing protocol and UX; AgentFS is the engine that powers it.

The recommended integration entrypoint for real app projects is:

```bash
agentfs appfs compose up -f appfs-compose.yaml
```

The lower-level managed runtime primitive remains:

```bash
agentfs appfs up <id-or-path> <mountpoint>
```

Managed runtime state lives in:

```text
/_appfs/apps.registry.json
```

Low-level debug commands still exist:

- `agentfs mount ... --managed-appfs`
- `agentfs serve appfs --managed`

## Quick Start

The higher-level AppFS flow is now:

1. declare runtime, connectors, and apps in `appfs-compose.yaml`
2. run `agentfs appfs compose up`
3. use the mounted tree directly

The lower-level managed flow is still:

1. start a bridge or in-process connector
2. initialize an empty AgentFS database that AppFS will use as its storage and mount substrate
3. start AppFS with `agentfs appfs up`
4. register an app through `/_appfs/register_app.act`
5. read files, switch scope, and trigger actions through the mounted tree

Today the recommended integration path is compose-first. `agentfs appfs up` remains the lower-level runtime primitive when you want to debug mount/runtime behavior directly.

A Huoyan attached-case compose example lives at [examples/appfs/appfs-compose.huoyan-attached-case.example.yaml](./examples/appfs/appfs-compose.huoyan-attached-case.example.yaml).

Minimal compose shape for the reference HTTP bridge:

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

With that file in place:

```bash
agentfs appfs compose up -f appfs-compose.yaml
```

Compose is the recommended path when you want one command to:

- prepare or reopen the AgentFS runtime database
- supervise an external or command-launched connector
- bootstrap the managed AppFS registry
- mount the tree and start the runtime in one foreground process

Prerequisites:

- Rust toolchain with `cargo`
- Python + `uv` for the reference HTTP bridge
- port `127.0.0.1:8080` available
- Windows: WinFsp installed
- Linux: FUSE available
- macOS: NFS mount support available

### Windows

Install WinFsp first. AppFS uses WinFsp as the Windows mount backend for `--backend winfsp`.

- Download and install the latest WinFsp release from [winfsp.dev/rel](https://winfsp.dev/rel/)
- After installation, open a new terminal before running `agentfs appfs up`
- A reboot is usually not required, but if Windows reports the driver is busy or mounts still fail after install, reboot once and retry
- For WinFsp, the mountpoint path itself should be absent before mount. Keep the parent directory, such as `C:\mnt`, but do not pre-create `C:\mnt\appfs-compose-aiim`. `appfs compose up` now preserves this WinFsp expectation and will clean up an empty stale placeholder directory if an older run left one behind.

Compose-first startup on Windows:

```powershell
cd C:\Users\esp3j\rep\agentfs\cli
cargo run -- appfs compose up -f ..\examples\appfs\appfs-compose.huoyan-attached-case.example.yaml
```

If the target software is already inside a case or other attached scope, prefer passing that bootstrap mode through connector env in the compose file, as shown in the Huoyan example. That avoids issuing an extra `enter_scope` just to land on the working tree.

Start the reference HTTP bridge:

```powershell
cd C:\Users\esp3j\rep\agentfs\examples\appfs\bridges\http-python
uv run python bridge_server.py
```

Initialize an empty database:

```powershell
cd C:\Users\esp3j\rep\agentfs\cli
cargo run -- init managed-http --force
```

Start AppFS:

```powershell
cd C:\Users\esp3j\rep\agentfs\cli
cargo run -- appfs up .agentfs\managed-http.db C:\mnt\appfs-managed-http --backend winfsp
```

Register an app:

```powershell
Add-Content C:\mnt\appfs-managed-http\_appfs\register_app.act '{"app_id":"aiim","transport":{"kind":"http","endpoint":"http://127.0.0.1:8080","http_timeout_ms":5000,"grpc_timeout_ms":5000,"bridge_max_retries":2,"bridge_initial_backoff_ms":100,"bridge_max_backoff_ms":1000,"bridge_circuit_breaker_failures":5,"bridge_circuit_breaker_cooldown_ms":3000},"client_token":"reg-http-001"}'
```

Read a snapshot and trigger an action:

```powershell
Get-Content C:\mnt\appfs-managed-http\aiim\chats\chat-001\messages.res.jsonl | Select-Object -First 5
Add-Content C:\mnt\appfs-managed-http\aiim\contacts\zhangsan\send_message.act '{"version":2,"client_token":"msg-001","payload":{"text":"hello"}}'
```

### Linux

Start the reference HTTP bridge:

```bash
cd /path/to/agentfs/examples/appfs/bridges/http-python
uv run python bridge_server.py
```

Initialize an empty database:

```bash
cd /path/to/agentfs/cli
cargo run -- init managed-http --force
```

Start AppFS:

```bash
cd /path/to/agentfs/cli
mkdir -p /tmp/appfs-managed-http
cargo run -- appfs up .agentfs/managed-http.db /tmp/appfs-managed-http --backend fuse
```

Register an app:

```bash
echo '{"app_id":"aiim","transport":{"kind":"http","endpoint":"http://127.0.0.1:8080","http_timeout_ms":5000,"grpc_timeout_ms":5000,"bridge_max_retries":2,"bridge_initial_backoff_ms":100,"bridge_max_backoff_ms":1000,"bridge_circuit_breaker_failures":5,"bridge_circuit_breaker_cooldown_ms":3000},"client_token":"reg-http-001"}' >> /tmp/appfs-managed-http/_appfs/register_app.act
```

Read a snapshot and trigger an action:

```bash
head -n 5 /tmp/appfs-managed-http/aiim/chats/chat-001/messages.res.jsonl
echo '{"version":2,"client_token":"msg-001","payload":{"text":"hello"}}' >> /tmp/appfs-managed-http/aiim/contacts/zhangsan/send_message.act
```

### macOS

macOS uses the AgentFS NFS backend instead of FUSE. The AppFS managed flow is the same, but Linux is still the primary required CI platform.

Initialize an empty database:

```bash
cd /path/to/agentfs/cli
cargo run -- init managed-http --force
```

Start AppFS:

```bash
cd /path/to/agentfs/cli
mkdir -p /tmp/appfs-managed-http
cargo run -- appfs up .agentfs/managed-http.db /tmp/appfs-managed-http --backend nfs
```

Then use the same `/_appfs/register_app.act`, per-app `.act`, and ordinary file reads shown above.

## Build From Source

### Environment Dependencies

On a fresh Windows machine, install the Rust toolchain and Visual Studio Build Tools before running `cargo build`.

If you see:

```text
error: linker `link.exe` not found
```

the MSVC build environment is not available in the current shell yet.

1. Install Rust

```powershell
winget install --id Rustlang.Rustup -e
rustup default stable-x86_64-pc-windows-msvc
```

2. Install Visual Studio Build Tools

```powershell
# Download the Visual Studio Build Tools installer
Invoke-WebRequest -Uri https://aka.ms/vs/17/release/vs_buildtools.exe -OutFile vs_buildtools.exe

# Install the C++ Build Tools workload (without the full IDE)
Start-Process -Wait -FilePath .\vs_buildtools.exe -ArgumentList "--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

3. Install the official LLVM Windows release

Download the latest Windows installer (`LLVM-*-win64.exe`) from the official LLVM release page:

- [LLVM official releases](https://github.com/llvm/llvm-project/releases)

During installation, enable the option to add LLVM to `PATH` if the installer offers it. The default install location is usually:

```text
C:\Program Files\LLVM\bin
```

4. Make sure the MSVC and LLVM compiler directories are on `PATH`

Open a new terminal, then run:

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

On a typical machine these resolve to paths like:

```text
C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\<version>\bin\Hostx64\x64
C:\Program Files\LLVM\bin
```

If `where.exe cl` or `where.exe link` still cannot find anything, check that the MSVC directory above exists first. If it does not, re-run the Build Tools installer.

If `where.exe clang-cl` still cannot find anything, LLVM is either not installed yet or was not added to `PATH`. In that case, check whether `C:\Program Files\LLVM\bin\clang-cl.exe` exists and add `C:\Program Files\LLVM\bin` to `PATH`.

For `cargo build`, the important part is simply that `cl.exe`, `link.exe`, and `clang-cl.exe` are reachable on `PATH`.

### CLI and Rust SDK

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

## Documentation

Key entry points:

- [Documentation Index](docs/README.md)
- [Current AppFS milestone (v4)](docs/v4/README.md)
- [Connectorization milestone (v3)](docs/v3/README.md)
- [Backend-native milestone (v2)](docs/v2/README.md)
- [examples/appfs guide](examples/appfs/README.md)
- [cli/TEST-WINDOWS.md](cli/TEST-WINDOWS.md)
- [Runtime closure design plan](docs/plans/2026-03-26-appfs-runtime-closure-design.md)

## Architecture

AppFS is organized into three layers:

- AgentFS Core: the engine beneath AppFS, including SQLite filesystem, generic overlay behavior, sync, and platform mount backends
- AppFS Engine: registry, structure sync, runtime lifecycle, snapshot read-through, and connector adapters
- AppFS UX: `agentfs appfs up` plus the mounted control plane under `/_appfs/*`

```mermaid
flowchart TD
    A["Shell / PowerShell / bash"] --> B["agentfs appfs up"]
    B --> C["AgentFS Core"]
    B --> D["AppFS Engine"]
    D --> E["/_appfs/apps.registry.json"]
    C --> F["Mounted AppFS tree"]
    D --> F
    D --> G["AppTreeSyncService"]
    D --> H["AppConnector"]
    G --> H
    H --> I["in-process / HTTP / gRPC adapters"]
    I --> J["real app backend or demo backend"]
```

Notes:

- `AppConnector` is the canonical runtime-facing connector surface
- `_meta/manifest.res.json` is a derived view, not the runtime source of truth
- snapshot cold misses auto-expand on ordinary file reads through the mount path
- the current CLI layering is intentional: AppFS is exposed as an `agentfs` subcommand because it still depends on AgentFS storage and mount infrastructure
- `agentfs init --base` remains an AgentFS feature, but it is not part of the recommended AppFS path

## Repository Layout

- `cli/`: the `agentfs` CLI, including AppFS subcommands, runtime, and mount integration
- `sdk/rust/`: Rust SDK and filesystem implementations
- `sdk/typescript/`: TypeScript SDK
- `sdk/python/`: Python SDK
- `sandbox/`: Linux-only syscall interception sandbox
- `examples/appfs/`: AppFS examples, split into `fixtures/`, `bridges/`, `templates/`, and `legacy/`
- `docs/`: ADRs, plans, contracts, and release notes

## Testing

Core validation paths:

- Rust CLI tests: `cargo test --manifest-path cli/Cargo.toml --package agentfs`
- Rust SDK tests: `cargo test --manifest-path sdk/rust/Cargo.toml`
- Linux contract suite: `cli/tests/test-appfs-connector-contract.sh`
- Windows managed lifecycle regression: `cli/test-windows-appfs-managed.ps1`

Linux remains the primary required CI platform. Windows has dedicated manual regression coverage for the managed AppFS flow.

## Status

Current repository status:

- `v0.3` connectorization is merged and remains the release baseline
- `v0.4` structure sync, unified connector, managed lifecycle, and `appfs up` are implemented in-tree
- multi-app managed runtime is available
- real app pilot work is the next validation milestone before a stable release claim

## License

MIT
