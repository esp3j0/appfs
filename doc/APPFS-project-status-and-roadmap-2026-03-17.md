# AppFS Project Status and Next Steps (2026-03-17)

- Date: `2026-03-17`
- Scope: `AppFS on top of AgentFS`
- Status: `v0.1 release-candidate stabilization`

## 1. Current Status

### 1.1 What Is Working

1. Core AppFS protocol draft and adapter requirements are documented and versioned.
2. Runtime command path (`agentfs serve appfs`) supports in-process and bridge modes.
3. HTTP and gRPC adapter bridge references are available.
4. Contract tests (`CT-001`..`CT-017`) are integrated and exercised in CI.
5. CI pipeline is green on the current branch after recent conflict and lint fixes.
6. AppFS documentation set has been centralized under `doc/`.
7. Compatibility matrix and developer troubleshooting path are documented.

### 1.2 Quality Level

1. Conformance quality: `High` for current Core scope (validated by CI gates).
2. Implementation portability: `Medium-High` (transport mappings ready; ecosystem templates still limited).
3. Operational maturity: `Medium` (good test coverage, but release governance and onboarding can still improve).

## 2. Known Gaps

1. Adapter author onboarding still needs more end-to-end real-app examples.
2. Release governance docs exist, but `rc2 -> v0.1.0` cadence is not yet formalized as a single checklist.
3. Some non-Core roadmap items (multi-tenant context, richer QoS, advanced subscriptions) are not yet ADR-tracked.

## 3. Recommended Next Steps

## 3.1 Immediate (v0.1.0 Finalization)

1. Finalize `rc2` freeze and keep only bugfix/additive PRs.
2. Publish a final release checklist for `v0.1.0` tag cut (owner, evidence, rollback notes).
3. Ensure required/informational CI policy is reflected in branch protection and team docs.

## 3.2 Short Term (v0.1.x Experience)

1. Add adapter starter templates for at least one additional language (Go or TypeScript).
2. Add one real-app adapter walkthrough based on the developer guide.
3. Keep bridge conformance bootstrap scripts aligned with CI defaults.

## 3.3 Mid Term (v0.2 Design)

1. ADR: multi-tenant/user context model.
2. ADR: stream QoS and delivery tuning.
3. ADR: paging/cursor capability extensions for high-volume apps.

## 4. Delivery Plan (Suggested)

1. Milestone A (1-3 days): `rc2` freeze docs + final release checklist + CI policy sync.
2. Milestone B (1 week): adapter DX uplift (template + real-app walkthrough + troubleshooting hardening).
3. Milestone C (2-3 weeks): `v0.2` ADR set and prototype decisions.
