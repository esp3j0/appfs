# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Project Overview

AgentFS is a specialized filesystem for AI agents, storing all agent state (files, key-value data, tool call history) in a single SQLite database file. The project consists of:

- **CLI** (`cli/`): Rust command-line tool for managing agent filesystems
- **AppFS Runtime** (`cli/src/cmd/appfs/`): Managed app protocol runtime, structure sync, registry, snapshot read-through, and lifecycle control
- **Rust SDK** (`sdk/rust/`): Core SDK used by the CLI
- **TypeScript SDK** (`sdk/typescript/`): JavaScript/TypeScript SDK for Node.js and browsers
- **Python SDK** (`sdk/python/`): Python SDK
- **Sandbox** (`sandbox/`): Linux-only syscall interception for process isolation (uses Facebook's reverie)
- **Reference Fixtures and Bridges** (`examples/appfs/`): Demo app tree, HTTP bridge, and gRPC bridge used by AppFS conformance tests and manual validation

The repository now has two overlapping concerns:

1. **AgentFS Core**: Generic SQLite-backed filesystem, overlay behavior, sync, sandboxing, and mount backends
2. **AppFS**: Filesystem-native app protocol built on AgentFS, with managed runtime startup via `agentfs appfs up`

## Architecture

### Three-Layer Storage Model
All agent data is stored in SQLite via the [Turso](https://github.com/tursodatabase/turso) database:

1. **Virtual Filesystem** (`fs_inode`, `fs_dentry`, `fs_data`, `fs_symlink` tables): POSIX-like filesystem with inode design, supporting hard links, symlinks, and chunked file storage
2. **Key-Value Store** (`kv_store` table): Simple get/set for agent state
3. **Tool Call Audit** (`tool_calls` table): Insert-only audit log for tool invocations

### OverlayFS
When `base` option is set, the filesystem operates as copy-on-write overlay on a host directory. Modifications are stored in the delta layer (SQLite) while the base directory remains read-only. The `fs_whiteout` table tracks deleted paths.

### AppFS Managed Runtime
AppFS is now managed-first:

- Recommended startup path: `agentfs appfs up <id-or-path> <mountpoint>`
- Shared runtime state: `/_appfs/apps.registry.json`
- App lifecycle control plane:
  - `/_appfs/register_app.act`
  - `/_appfs/unregister_app.act`
  - `/_appfs/list_apps.act`
- Per-app structure control plane:
  - `/_app/<enter_scope|refresh_structure>.act`
- Snapshot `*.res.jsonl` resources auto-expand on ordinary file reads through mount-side read-through

The canonical runtime-facing connector surface is now `AppConnector` in [`sdk/rust/src/appfs_connector.rs`](sdk/rust/src/appfs_connector.rs). Legacy `AppConnectorV2` / `AppConnectorV3` remain as compatibility layers behind transport adapters.

### Platform Differences
- **Linux**: FUSE mounting, sandbox with syscall interception (reverie)
- **macOS**: NFSv3 mounting (no FUSE), no sandbox support
- **Windows**: WinFsp mounting and AppFS demo/runtime paths are available; sandbox support is not available and Linux remains the primary required gate platform

## Build Commands

### Rust (CLI & SDK)
```bash
cd cli
cargo build                    # Debug build
cargo build --release          # Release build
cargo test                     # Run Rust SDK tests
cargo test --package agentfs   # Run CLI tests
```

On Linux ARM, `libunwind-dev` is required for sandbox functionality.

### TypeScript SDK
```bash
cd sdk/typescript
npm install
npm run build           # Compile TypeScript
npm test                # Run tests with vitest
npm run test:browser    # Run browser tests (Chromium + Firefox)
```

### Python SDK
```bash
cd sdk/python
uv sync                    # Install dependencies with uv
uv run pytest              # Run tests
uv run ruff check .        # Lint with ruff
uv run ruff format .       # Format code
```

## CLI Commands

The CLI binary is `agentfs`. Key commands:

```bash
agentfs init <id>                    # Create agent database at .agentfs/<id>.db
agentfs init <id> --base <dir>       # Create overlay filesystem on base directory
agentfs appfs up <id-or-path> <mountpoint>    # Start managed AppFS mount + runtime together
agentfs fs ls <id-or-path> [/path]   # List directory contents
agentfs fs cat <id-or-path> <file>   # Read file contents
agentfs fs write <id-or-path> <file> <content>  # Write to file
agentfs mount <id-or-path> <mountpoint>        # Mount filesystem (Linux: FUSE, macOS: NFS)
agentfs mount ... --managed-appfs              # Low-level AppFS debug surface
agentfs serve appfs --managed                  # Low-level AppFS runtime debug surface
agentfs run [--sandbox] <command>             # Run command in sandbox with /agent mounted
agentfs exec <id-or-path> <command> [args...] # Execute command in existing agent context
agentfs diff <id-or-path>                    # Show overlay filesystem changes
agentfs timeline <id-or-path>                # Show tool call history
agentfs sync pull|push|checkpoint|stats <id-or-path>  # Sync operations
```

For AppFS manual testing, the usual sequence is:

1. `agentfs init <id> --force`
2. `agentfs appfs up .agentfs/<id>.db <mountpoint> --backend <platform backend>`
3. append JSON to `/_appfs/register_app.act`
4. interact with the mounted app tree using ordinary file reads and `*.act` sinks

## SDK Usage Patterns

### Rust
```rust
let agent = AgentFS::open(AgentFSOptions::with_id("my-agent")).await?;
agent.fs.mkdir("/output", 0, 0).await?;
agent.kv.set("key", &"value").await?;
agent.tools.record("tool_name", start, end, params, result).await?;
```

### TypeScript
```typescript
import { AgentFS } from 'agentfs-sdk';

const agent = await AgentFS.open({ id: 'my-agent' });
await agent.fs.mkdir('/output');
await agent.kv.set('key', { data: 'value' });
await agent.tools.record('tool', startTs, endTs, params, result);
```

### Python
```python
from agentfs_sdk import AgentFS

agent = await AgentFS.open(id="my-agent")
await agent.fs.mkdir("/output")
await agent.kv.set("key", {"data": "value"})
await agent.tools.record("tool", start_ts, end_ts, params, result)
```

## Schema Migrations

Schema version is tracked in `schema_version` table. When schema changes, run:
```bash
agentfs migrate <id-or-path>        # Apply schema migrations
agentfs migrate <id-or-path> --dry-run  # Preview changes
```

## Testing

### CLI Integration Tests
```bash
cd cli
./tests/all.sh                   # Run all integration tests
```

### AppFS Contract and Runtime Validation

- Linux contract suite: `cli/tests/test-appfs-v2-contract.sh`
- Windows managed lifecycle regression: `cli/test-windows-appfs-managed.ps1`
- Windows manual guide: `cli/TEST-WINDOWS.md`

Linux remains the primary required CI platform. Windows has dedicated managed lifecycle regression coverage but is still treated as a secondary validation platform.

### Filesystem Testing (Linux)
For POSIX compliance testing, see `TESTING.md` for pjdfstest and xfstests setup.

## Release Notes

- Current release line: `0.7.0-beta.1`
- GitHub prereleases now publish binaries to GitHub Releases by default
- External package registry publishing (`npm`, `crates.io`, `PyPI`) is disabled by default and only runs when repository variable `RELEASE_PUBLISH_REGISTRIES=true`

## Code Organization

- `sdk/rust/src/lib.rs`: Main `AgentFS` struct, options, connection pool
- `sdk/rust/src/appfs_connector.rs`: Canonical AppFS connector trait used by runtime and mount-side read-through
- `sdk/rust/src/filesystem/`: Filesystem implementations (AgentFS, OverlayFS, HostFS)
- `sdk/rust/src/kvstore.rs`: Key-value store
- `sdk/rust/src/toolcalls.rs`: Tool call tracking
- `sdk/rust/src/schema.rs`: Schema version management and migrations
- `cli/src/cmd/`: CLI command handlers
- `cli/src/cmd/appfs/`: AppFS engine modules (`core`, `tree_sync`, `registry`, `registry_manager`, `runtime_config`, `runtime_entry`, `runtime_supervisor`, `mount_runtime`, `supervisor_control`, `snapshot_cache`, `events`, `paging`)
- `sandbox/src/syscall/`: Syscall interception (Linux only)
