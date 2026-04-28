# Huoyan Case Structure Revision Fast Path

Date: 2026-04-28
Status: Implemented

## Summary

Huoyan `case:*` scope startup is still slow on restart even after DB bulk materialization, because `get_app_structure()` currently rebuilds the full case tree before it can decide whether `known_revision` is unchanged.

The current goal is to add a **cheap shallow revision fast path** for Huoyan case scopes:

- unchanged restart should only inspect the **evidence layer** and the **top app layer**
- full recursive node traversal should only happen when that shallow revision changes
- AppFS should keep the existing behavior of full tree build on first load and on actual structure changes

This plan is intended to reduce “second startup” latency from ~10s+ to near the cost of:

- `list_evidences(case_id)`
- `list_nodes(analysis_cid, pid=ROOT_NODE_ID)`

without changing the existing AppFS structure protocol.

## Current Behavior

Huoyan case scope revision is currently derived from a full recursive tree walk:

1. `list_evidences(case_id)`
2. BFS over `list_nodes(analysis_cid, pid=...)` for the entire tree
3. build full AppFS structure paths
4. hash the full `revision_payload`
5. compare against `known_revision`

This means `known_revision` only avoids DB rewrite when unchanged. It does **not** avoid expensive connector-side tree construction.

Current revision semantics are “structure/path version”, not “content version”:

- included today:
  - evidence `eid`
  - evidence `name`
  - each node `nid`
  - final mapped `path`
  - whether node is `leaf`
- not included today:
  - `*.res.jsonl` content
  - record payload changes
  - leaf row count changes unless reflected through structure

## Requirements

### Functional

- A repeated `compose up` against an unchanged Huoyan case should avoid full recursive tree rebuild.
- First load of a case must still build the full tree and produce a snapshot.
- If the case structure actually changes, AppFS must still rebuild the full tree and update the DB.

### Non-Functional

- No AppFS protocol change required.
- Existing managed runtime / DB bulk materialization flow remains unchanged.
- Change should be isolated to the Huoyan bridge logic.

## Recommendation

Use a **hybrid revision** strategy:

1. Compute a **shallow probe revision** from the first two layers only
2. If the caller passes `known_revision` and the probe component matches, return `unchanged` immediately
3. Otherwise, build the full tree and compute the full structure payload as today
4. Return a revision string that contains both the probe hash and the full hash

This keeps unchanged restart cheap, while preserving a richer full-tree fingerprint for debugging and rollout safety.

## Why Not “Shallow Only” As The Entire Revision?

A pure shallow revision is viable only if we are certain that every app refresh changes second-layer app node metadata. That may be true in practice, but it is a product assumption rather than a protocol guarantee.

A hybrid revision has two advantages:

- unchanged restart can still short-circuit on the cheap probe
- when a change does occur, we still record a full-tree fingerprint in the returned revision string

This gives us a safer migration path and better diagnostics if a false-unchanged suspicion appears later.

## Proposed Revision Format

For case scopes, return a revision like:

```text
huoyan-case:4-p:1a2b3c4d5e6f-f:7a8b9c0d1e2f
```

Where:

- `p` = shallow probe hash
- `f` = full-tree structure hash

On startup:

- if `known_revision` contains a probe component and `probe == known_probe`, return `unchanged`
- if the format is old / probe missing / parsing fails, fall back to full-tree rebuild once

This makes rollout backward compatible:

- old revision strings still work
- first restart after deployment may still be slow once
- subsequent restarts can use the probe fast path

## Shallow Probe Definition

For case scopes, compute probe payload from:

### Evidence layer

From `list_evidences(case_id)`:

- `eid`
- sanitized/visible evidence name

### Top app layer

From `list_nodes(analysis_cid, pid=ROOT_NODE_ID)`:

- `eid`
- `nid`
- visible node name
- `NodeType`
- `SubNodeType`
- `HasChildNode`
- stable count-style fields if available (`RecordCount` / `Count`)

Recommended payload shape:

```json
{
  "case_id": 4,
  "evidences": [
    {"eid": 101, "name": "手机-1"},
    {"eid": 102, "name": "手机-2"}
  ],
  "apps": [
    {
      "eid": 101,
      "nid": 658000891,
      "name": "微信",
      "node_type": "...",
      "sub_node_type": "...",
      "has_child": true,
      "record_count": 123
    }
  ]
}
```

Hash with the same `_compact_json(...) + sha1[:12]` pattern currently used.

## Operating Assumption

This fast path assumes Huoyan uses incremental parsing for app trees and does not silently reparse an existing second-layer app node without changing the shallow app inventory. Under that assumption, a probe built from:

- app node `Nid`
- app node visible `Name`
- `NodeType`
- `SubNodeType`
- `HasChildNode`
- `RecordCount` / `Count` when present

is sufficient for restart-time `unchanged` detection.

## Implementation Sketch

### 1. Add probe helpers

In `huoyan_backend.py`:

- `_build_case_probe(case_id) -> dict`
- `_known_case_probe_digest(revision: str) -> str | None`
- `_case_revision(scope, probe_digest, payload) -> str`

### 2. Update `get_app_structure()`

For case scopes:

1. compute shallow probe revision only
2. compare against `known_revision`
3. if equal: return `unchanged`
4. otherwise: build full scope, compute full hash, return snapshot

### 3. Update `refresh_app_structure()`

Use the same logic for non-`enter_scope` refresh:

- cheap probe first
- only full rebuild on mismatch

For `enter_scope`, full build is still required when changing to a different case scope, because the caller needs the target snapshot if the target scope differs.

### 4. Keep home scope behavior unchanged

Home scope revision is already cheap and does not need this optimization.

## Failure Modes

### False unchanged

Risk:

- app internal structure changes
- second-layer metadata does not change
- probe matches
- AppFS skips full refresh incorrectly

Mitigations:

- validate actual Huoyan metadata behavior first
- include as many stable second-layer change signals as possible
- optionally add a configurable periodic forced full rebuild

### Backward compatibility

Risk:

- existing `known_revision` strings do not contain a probe component

Mitigation:

- treat unknown revision format as “no probe available”
- do one full rebuild
- emit new hybrid revision format afterward

## Suggested Rollout Controls

Add a bridge env flag:

```text
APPFS_HUOYAN_REVISION_MODE=full|probe2|hybrid
```

Recommended rollout:

- default to `hybrid`
- allow fallback to `full` during debugging

Optional diagnostic env:

```text
APPFS_HUOYAN_LOG_REVISION_TIMING=1
```

to log:

- probe build duration
- full scope build duration
- whether fast path hit or missed

## Test Plan

### Unit tests

- probe revision is deterministic
- probe parser reads probe component from hybrid revision
- duplicate/sanitized evidence/app names still produce stable probe payload
- old revision format falls back to full build

### Integration tests

- first `get_app_structure(case)` returns snapshot
- second `get_app_structure(case)` with unchanged hybrid revision returns `unchanged` without recursive traversal
- top-layer app metadata change causes snapshot rebuild

### Manual validation

On a large Huoyan case:

1. first `compose up` still performs full build
2. second `compose up` on unchanged case should skip recursive tree build
3. reparsing one app in Huoyan should cause probe mismatch and full rebuild

## Recommendation

This optimization is **reasonable and worth doing** if Huoyan’s second-layer app nodes reliably reflect reparsed app updates.

The best implementation is:

- **not** “skip structure checks entirely”
- **not** “replace revision with top-layer-only hash blindly”
- but a **hybrid probe/full revision design**

That gives us:

- cheap unchanged restart
- low implementation blast radius
- safer rollout than a pure shallow hash
