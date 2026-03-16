# AppFS Contract Test Skeleton

This directory contains shell-first contract tests for AppFS v0.1.

## Run

```bash
cd cli
APPFS_CONTRACT_TESTS=1 ./tests/test-appfs-contract.sh
```

For static fixture validation (without mounted runtime):

```bash
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT=/mnt/c/Users/esp3j/rep/agentfs/examples/appfs ./tests/test-appfs-contract.sh
```

To run through the existing aggregate test entry:

```bash
cd cli
APPFS_CONTRACT_TESTS=1 ./tests/all.sh
```

## Environment

| Variable | Default |
|---|---|
| `APPFS_ROOT` | `/app` |
| `APPFS_APP_ID` | `aiim` |
| `APPFS_TEST_ACTION` | `/app/aiim/contacts/zhangsan/send_message.act` |
| `APPFS_PAGEABLE_RESOURCE` | `/app/aiim/chats/chat-001/messages.res.json` |
| `APPFS_TIMEOUT_SEC` | `10` |
| `APPFS_STATIC_FIXTURE` | `0` |

## Notes

1. Tests are currently gated by `APPFS_CONTRACT_TESTS=1` so they do not affect existing CI by default.
2. Some checks require `jq`; if missing, JSON field-level assertions are skipped.
3. `APPFS_STATIC_FIXTURE=1` runs only static contract checks (layout/replay/manifest policy).
4. This is a skeleton focused on protocol gates, not full adapter business behavior.
