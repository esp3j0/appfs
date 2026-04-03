# AppFS Examples

This directory is the current AppFS example and integration entrypoint.

It is organized around the shipping runtime model:

1. start AppFS with `agentfs appfs up`
2. register apps through `/_appfs/register_app.act`
3. use ordinary file reads, scope switches, and `.act` writes against the mounted tree

`mount` and `serve appfs` still exist for debugging, but they are no longer the recommended examples path.

## Layout

1. `fixtures/aiim/`
   Canonical demo fixture used by contract tests, live harnesses, and smoke examples.
2. `bridges/http-python/`
   Current Python HTTP connector bridge reference.
3. `bridges/grpc-python/`
   Current Python gRPC connector bridge reference.
4. `templates/http-python/`
   Current Python connector scaffold used by `new-connector.sh`.
5. `legacy/v1/`
   Historical `AppAdapterV1` assets kept only for reference.
6. `.well-known/apps.res.json`
   Discovery/fixture example, not the primary startup path.

## Recommended Start

Use the repository root README for platform-specific startup commands.
Once AppFS is mounted, register an app through the root control plane:

```bash
echo '{"app_id":"aiim","transport":{"kind":"http","endpoint":"http://127.0.0.1:8080","http_timeout_ms":5000,"grpc_timeout_ms":5000,"bridge_max_retries":2,"bridge_initial_backoff_ms":100,"bridge_max_backoff_ms":1000,"bridge_circuit_breaker_failures":5,"bridge_circuit_breaker_cooldown_ms":3000},"client_token":"reg-http-001"}' >> /mount/_appfs/register_app.act
```

After registration:

1. read snapshot resources directly, for example `chats/chat-001/messages.res.jsonl`
2. switch structure with `/_app/enter_scope.act`
3. refresh structure with `/_app/refresh_structure.act`
4. submit actions by appending JSON to `.act` files

`_snapshot/refresh.act` remains a control-plane example under the fixture, but it is not the normal snapshot read path.

## Conformance

From this directory:

```bash
sh ./run-conformance.sh inprocess
sh ./run-conformance.sh http-python
sh ./run-conformance.sh grpc-python
```

These scripts exercise the live AppFS harness and the current connector transport examples.

## Adapter / Connector Onboarding

See:

1. `ADAPTER-QUICKSTART.md`
2. `templates/http-python/`
3. `new-connector.sh`

If you need historical `AppAdapterV1` reference material, use `legacy/v1/`.
