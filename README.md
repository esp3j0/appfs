# AppFS

Filesystem-native app protocol for shell-first AI agents.

[中文 README](README.zh-CN.md)

AppFS makes different apps look and feel like one filesystem contract, so an agent can use the same primitives across tools:

1. `cat` for reading resources.
2. `echo > *.act` for triggering actions.
3. `tail -f` on stream files for async results.

This repository currently hosts the AppFS spec, adapter contracts, reference fixtures, conformance tests, and runtime implementation on top of AgentFS.

## Why AppFS

The design target is practical LLM + bash operation:

1. One interaction model across many apps instead of one MCP schema per app.
2. Low token overhead with path-native operations.
3. Stream-first async model with replay support.
4. Runtime-generated request IDs, so clients do not need UUID management.
5. Cross-language adapter compatibility through a frozen contract surface.

## Core Interaction Model

```bash
# 1) subscribe app event stream first
tail -f /app/aiim/_stream/events.evt.jsonl

# 2) trigger an action by write+close
echo "hello" > /app/aiim/contacts/zhangsan/send_message.act

# 3) read resources directly
cat /app/aiim/contacts/zhangsan/profile.res.json

# 4) page long content via unified paging actions
cat /app/aiim/chats/chat-001/messages.res.json
echo '{"handle_id":"<from-page>"}' > /app/aiim/_paging/fetch_next.act
```

## Available Actions (AIIM Fixture)

Source of truth: `examples/appfs/aiim/_meta/manifest.res.json`.

1. `contacts/{contact_id}/send_message.act`
   - `kind`: `action`
   - `execution_mode`: `inline`
   - `input_mode`: `text_or_json`
2. `files/{file_id}/download.act`
   - `kind`: `action`
   - `execution_mode`: `streaming`
   - `input_mode`: `json`
3. `/_paging/fetch_next.act`
   - `kind`: `action`
   - `execution_mode`: `inline`
   - `input_mode`: `text_or_json`
4. `/_paging/close.act`
   - `kind`: `action`
   - `execution_mode`: `inline`
   - `input_mode`: `text_or_json`

## Runtime Quick Start (HTTP Bridge)

### Windows (PowerShell, 4 Steps)

1. Mount AgentFS (Terminal A).

```powershell
cd C:\Users\esp3j\rep\agentfs\cli
cargo run -- init win-real
cargo run -- mount .agentfs\win-real.db C:\mnt\win-real --foreground
```

2. Place AIIM fixture into the mountpoint (Terminal B).

```powershell
cd C:\Users\esp3j\rep\agentfs
Copy-Item -Recurse -Force .\examples\appfs\aiim C:\mnt\win-real\aiim
```

3. Start HTTP bridge (Terminal C).

```powershell
cd C:\Users\esp3j\rep\agentfs\examples\appfs\http-bridge\python
uv run python bridge_server.py
```

4. Start AppFS runtime and operate files (Terminal D/E).

```powershell
cd C:\Users\esp3j\rep\agentfs\cli
$env:APPFS_ADAPTER_HTTP_ENDPOINT = "http://127.0.0.1:8080"
cargo run -- serve appfs --root C:\mnt\win-real --app-id aiim
```

```powershell
# watch stream (separate terminal)
Get-Content C:\mnt\win-real\aiim\_stream\events.evt.jsonl -Wait

# trigger action (confirmed PowerShell form)
'{"test":"hello"}' | Set-Content C:\mnt\win-real\aiim\contacts\zhangsan\send_message.act -NoNewline

# read resource
Get-Content C:\mnt\win-real\aiim\contacts\zhangsan\profile.res.json -Raw
```

### Linux (bash, 4 Steps)

1. Mount AgentFS (Terminal A).

```bash
cd /path/to/agentfs/cli
cargo run -- init linux-real
mkdir -p /tmp/appfs-real
cargo run -- mount .agentfs/linux-real.db /tmp/appfs-real --foreground
```

2. Place AIIM fixture into the mountpoint (Terminal B).

```bash
cd /path/to/agentfs
cp -R ./examples/appfs/aiim /tmp/appfs-real/aiim
```

3. Start HTTP bridge (Terminal C).

```bash
cd /path/to/agentfs/examples/appfs/http-bridge/python
uv run python bridge_server.py
```

4. Start AppFS runtime and operate files (Terminal D/E).

```bash
cd /path/to/agentfs/cli
APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 cargo run -- serve appfs --root /tmp/appfs-real --app-id aiim
```

```bash
# watch stream (separate terminal)
tail -f /tmp/appfs-real/aiim/_stream/events.evt.jsonl

# trigger action
echo '{"text":"hello"}' > /tmp/appfs-real/aiim/contacts/zhangsan/send_message.act

# read resource
cat /tmp/appfs-real/aiim/contacts/zhangsan/profile.res.json
```

## Architecture

- Draw.io source: [docs/v1/architecture/appfs-v0.1-architecture.drawio](docs/v1/architecture/appfs-v0.1-architecture.drawio)
- SVG preview: [docs/v1/architecture/appfs-v0.1-architecture.svg](docs/v1/architecture/appfs-v0.1-architecture.svg)
- Spec baseline: [APPFS-v0.1.md](docs/v1/APPFS-v0.1.md)

The architecture has four layers:

1. Agent shell operations (`cat`, `echo`, `tail`).
2. AppFS namespace and contract files (`_meta`, `_stream`, `_paging`, domain paths).
3. AppFS runtime in `agentfs serve appfs` (routing, validation, stream persistence, replay).
4. Business adapter implementations (in-process or HTTP/gRPC bridge) that call real app backends.

![AppFS v0.1 Architecture](docs/v1/architecture/appfs-v0.1-architecture.svg)

## Conformance Quick Start

### 1) Static Contract Checks

```bash
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT="$PWD/../examples/appfs" sh ./tests/test-appfs-contract.sh
```

### 2) Live Conformance (In-Process Adapter)

Linux + FUSE environment required:

```bash
cd examples/appfs
sh ./run-conformance.sh inprocess
```

### 3) Live Conformance (Out-of-Process Bridges)

```bash
cd examples/appfs
sh ./run-conformance.sh http-python
sh ./run-conformance.sh grpc-python
```

## Adapter Developer Path

Start here:

1. [APPFS-adapter-developer-guide-v0.1.md](docs/v1/APPFS-adapter-developer-guide-v0.1.md)
2. [examples/appfs/ADAPTER-QUICKSTART.md](examples/appfs/ADAPTER-QUICKSTART.md)
3. [APPFS-adapter-requirements-v0.1.md](docs/v1/APPFS-adapter-requirements-v0.1.md)
4. [APPFS-compatibility-matrix-v0.1.md](docs/v1/APPFS-compatibility-matrix-v0.1.md)
5. [APPFS-conformance-v0.1.md](docs/v1/APPFS-conformance-v0.1.md)
6. [APPFS-contract-tests-v0.1.md](docs/v1/APPFS-contract-tests-v0.1.md)
7. [APPFS-adapter-structure-mapping-v0.1.md](docs/v1/APPFS-adapter-structure-mapping-v0.1.md)

Key compatibility commitments:

1. Language-neutral implementation is allowed.
2. Compatibility is judged by behavior and conformance tests.
3. Adapter interface surface is frozen for `v0.1.x` (additive changes only).
4. Troubleshooting baseline is documented in the developer guide (`port`, `uv`, `grpc`, `CT-017`, mount issues).

## Repository Map (AppFS-Relevant)

1. `docs/v1/APPFS-v0.1.md`: core protocol.
2. `docs/v1/APPFS-adapter-requirements-v0.1.md`: adapter requirements.
3. `docs/v1/APPFS-adapter-developer-guide-v0.1.md`: end-to-end developer workflow and troubleshooting.
4. `docs/v1/APPFS-adapter-structure-mapping-v0.1.md`: app structure definition and node-to-handler mapping workflow.
5. `docs/v1/APPFS-compatibility-matrix-v0.1.md`: language/transport/capability compatibility and acceptance commands.
6. `docs/v1/APPFS-adapter-implementation-plan-v0.1.md`: implementation plan and milestones.
7. `examples/appfs/`: reference fixtures and bridge examples.
8. `examples/appfs/new-adapter.sh`: scaffold generator for Python HTTP bridge adapters.
9. `cli/src/cmd/appfs.rs`: AppFS runtime command implementation.
10. `cli/tests/appfs/`: live contract and resilience suites (`CT-001` to `CT-017`).

## Current Status

Current branch has AppFS v0.1 contract suite and RC closure artifacts, including:

1. Release checklist and notes.
2. RC closure record.
3. Static and live conformance gates for in-process and bridge modes.

For release details, see:

1. [APPFS-release-checklist-v0.1-rc1.md](docs/v1/APPFS-release-checklist-v0.1-rc1.md)
2. [APPFS-release-notes-v0.1-rc1.md](docs/v1/APPFS-release-notes-v0.1-rc1.md)
3. [APPFS-rc-closure-v0.1.md](docs/v1/APPFS-rc-closure-v0.1.md)
4. [APPFS-v0.1.0-rc2-freeze.md](docs/v1/APPFS-v0.1.0-rc2-freeze.md)
5. [APPFS-migration-note-v0.1.0-rc2.md](docs/v1/APPFS-migration-note-v0.1.0-rc2.md)
6. [APPFS-project-status-and-roadmap-2026-03-17.md](docs/v1/APPFS-project-status-and-roadmap-2026-03-17.md)

## License

MIT
