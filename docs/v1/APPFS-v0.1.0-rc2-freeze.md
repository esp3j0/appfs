# AppFS v0.1.0-rc2 Freeze Declaration

- Version: `v0.1.0-rc2`
- Date: `2026-03-17`
- Status: `RC Freeze Candidate`
- Scope: `Protocol + Adapter Contract + Conformance Gates`

## 1. Purpose

This document declares the freeze boundary for AppFS `v0.1.0-rc2`.

Goal:

1. Freeze Core protocol semantics for release candidate validation.
2. Avoid late-cycle breaking changes before `v0.1.0`.
3. Keep only low-risk, test-backed, additive updates.

## 2. Frozen Artifact Set

The following files are part of the `rc2` frozen set:

1. `docs/v1/APPFS-v0.1.md`
2. `docs/v1/APPFS-adapter-requirements-v0.1.md`
3. `docs/v1/APPFS-conformance-v0.1.md`
4. `docs/v1/APPFS-contract-tests-v0.1.md`
5. `docs/v1/APPFS-adapter-http-bridge-v0.1.md`
6. `docs/v1/APPFS-adapter-grpc-bridge-v0.1.md`

## 3. Frozen Core Semantics

The following semantics are frozen for `v0.1.0-rc2`:

1. Colocated path model: `*.res.json`, `*.act`, `*.evt.jsonl`.
2. Action commit boundary: `write + close` only.
3. Stream-first lifecycle and replay: `_stream/events.evt.jsonl`, `cursor`, `from-seq`.
4. Paging control actions: `/_paging/fetch_next.act` and `/_paging/close.act`.
5. Path safety and cross-platform segment restrictions.
6. Adapter compatibility contract centered on `AppAdapterV1` semantics.
7. Core conformance gate pass requirement (`CT-001` to `CT-017` in current suite).

## 4. Allowed During Freeze

Allowed changes:

1. Documentation clarifications with no semantic change.
2. Bug fixes that preserve frozen behavior and pass full CI.
3. Additive non-Core extension fields (`x_*`) that do not break Core clients.
4. Test stabilization that removes flakiness without weakening assertions.

Disallowed changes:

1. Breaking path model changes.
2. Changing `.act` commit semantics.
3. Changing stream event lifecycle semantics for accepted requests.
4. Removing or redefining existing Core-required nodes.

## 5. Change Control Rules

For freeze-period changes:

1. Every behavior change must include contract-test evidence.
2. Any proposed semantic change must be deferred to `v0.2` and tracked as ADR/backlog.
3. PR titles should include scope tags such as `fix(appfs)`, `docs(appfs)`, `test(appfs)`.

## 6. Release Exit Criteria

`v0.1.0-rc2` is considered ready for final `v0.1.0` cut when:

1. Required CI checks are green on `main`.
2. AppFS bridge-mode gates (HTTP and gRPC) are green.
3. No open P0/P1 defects for Core semantics.
4. Migration note is published and validated.


