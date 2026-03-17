# AppFS Migration Note to v0.1.0-rc2

- Version: `v0.1.0-rc2`
- Date: `2026-03-17`
- Status: `Draft`
- Audience: `Runtime maintainers`, `Adapter implementers`, `Docs users`

## 1. Summary

This migration note covers the `rc2` stabilization phase.

Key points:

1. Core AppFS protocol semantics remain compatible with previous `v0.1` drafts.
2. AppFS specification files are now organized under `doc/`.
3. CI gate expectations remain strict for Core and bridge-mode live conformance.

## 2. What Changed

### 2.1 Documentation Layout

Moved:

1. `APPFS-*.md` (repo root) -> `doc/APPFS-*.md`

Updated references:

1. `README.md`
2. `CHANGELOG.md`
3. `examples/appfs/ADAPTER-QUICKSTART.md`

Impact:

1. Update internal links/bookmarks/scripts that referenced root-level `APPFS-*.md`.

### 2.2 Runtime and Adapter Behavior

No intentional breaking behavior change is introduced by this migration note itself.

Recent related hardening before `rc2`:

1. Bridge resilience and retry/circuit-breaker behavior aligned with contract tests.
2. Clippy and formatting compliance fixes to keep CI gate stable.

## 3. Compatibility Statement

For adapters implementing current `v0.1` Core semantics:

1. No path contract migration is required.
2. No event schema shape migration is required for Core fields.
3. No action submission model migration is required (`write+close` unchanged).

## 4. Action Items for Maintainers

1. Update any hardcoded doc paths in automation from `APPFS-*.md` to `doc/APPFS-*.md`.
2. Keep Core contract test suite (`CT-001`..`CT-017`) green before merging.
3. Treat semantic changes as `v0.2` candidates unless critical defect.

## 5. Validation Checklist

1. `README` links open correctly after doc relocation.
2. Conformance docs are discoverable from repository root entry points.
3. Existing adapter quickstart still points to valid spec files.
4. CI required checks remain green on PR.

