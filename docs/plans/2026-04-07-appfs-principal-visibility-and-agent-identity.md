# AppFS Principal Visibility And Agent Identity Design

**Date:** 2026-04-07  
**Status:** Future plan, not current mainline scope  
**Depends on:** AppFS attach contract v1.1, stable IC-0 / IC-1 / IC-2 baselines

## 1. Goal

Define a future model for:

1. shared vs private app views inside one AppFS mount;
2. stable app identity separate from transient agent process identity;
3. connector-side session/account isolation for apps such as chat software.

This design is intentionally **not** the current primary goal. The current mainline remains:

1. stabilize the attach contract;
2. keep the AppFS mounted app loop working;
3. preserve `appfs-agent` as a general-purpose agent.

## 2. Recommendation

The distinction between shared and private app data should be modeled primarily at the **AppFS layer**, not at the `appfs-agent` layer.

`appfs-agent` should declare who is attaching. AppFS should decide which app view that identity is allowed to see. Connectors should map that view to real upstream app sessions, accounts, or profiles.

In short:

1. `appfs-agent` provides identity hints;
2. AppFS owns visibility, path routing, and view isolation;
3. connectors own upstream login/session/account binding.

## 3. Why This Belongs In AppFS

If shared/private behavior only exists in `appfs-agent`, the semantics become client-specific. A different client, script, or direct filesystem consumer could bypass those assumptions and observe a different app surface.

That would create three problems:

1. the filesystem would stop being the source of truth for what is visible;
2. app identity isolation would depend on which client happened to mount or browse the tree;
3. connector behavior would become harder to reason about because upstream session binding would be hidden in one consumer.

The cleaner model is:

1. AppFS exposes an explicit view;
2. all clients see that same view for the same principal;
3. connector session binding follows the AppFS-selected principal/profile.

## 4. Identity Layers

These identities should remain separate:

### 4.1 Runtime identity

`runtime_session_id`

Meaning:

1. identifies one shared AppFS runtime / mount lifecycle;
2. common to all attached agents on the same mount.

### 4.2 Agent instance identity

`attach_id`

Meaning:

1. identifies one attached `appfs-agent` process instance;
2. may be ephemeral;
3. should not be used as the durable app account identity.

### 4.3 Principal identity

Recommended future field:

`principal_id`

Meaning:

1. identifies the stable app-facing identity or persona;
2. may correspond to a user, role, bot identity, or named workspace profile;
3. should be the key used for private app views.

Examples:

1. `planner`
2. `reviewer`
3. `ops-bot`

### 4.4 Visibility policy

Recommended future field:

`visibility`

Initial policy values:

1. `shared`
2. `private`
3. `team`

Meaning:

1. `shared` means all attached principals may observe the same AppFS app view;
2. `private` means the app view is scoped to one `principal_id`;
3. `team` means the view is shared among an allowed subset of principals.

## 5. Target Responsibility Split

### 5.1 `appfs-agent`

Future responsibility:

1. declare attach identity;
2. optionally declare `principal_id`;
3. optionally declare desired role and visibility hint;
4. stay usable without AppFS.

Non-responsibility:

1. it should not define the authoritative isolation rules for app views;
2. it should not be the only place where private/shared policy exists.

### 5.2 AppFS

Future responsibility:

1. store or resolve principal-aware app view policy;
2. map principals to visible mount paths;
3. enforce whether an app is shared or private;
4. ensure different clients see the same view for the same principal.

### 5.3 Connector

Future responsibility:

1. bind `principal_id` to upstream app account/session/profile;
2. keep multiple upstream identities isolated where needed;
3. expose connector-owned paths and data for the selected principal.

This is especially important for apps like chat software, where two agents may share the same AppFS mount but must not share the same chat identity.

## 6. Preferred Data Model

When this work starts, the attach surface should grow from:

1. `runtime_session_id`
2. `attach_id`
3. `attach_role`

to a future model such as:

1. `runtime_session_id`
2. `attach_id`
3. `attach_role`
4. `principal_id`
5. `visibility`
6. optional `profile_id`

Notes:

1. `principal_id` is the semantic identity;
2. `profile_id` is optional when one principal may hold multiple app-side profiles;
3. `attach_id` remains process-scoped and should not replace either of them.

## 7. Preferred Namespace Shape

Avoid making private/shared behavior depend only on invisible runtime state. Prefer explicit and debuggable paths.

Recommended long-term shape:

1. `/workspace/...`
2. `/apps/shared/<app_id>/...`
3. `/apps/private/<principal_id>/<app_id>/...`
4. `/apps/team/<team_id>/<app_id>/...`

Optional future convenience aliases may exist, but they should be derived views rather than the only source of truth.

Example:

1. `agent-a` and `agent-b` share one mount and one `runtime_session_id`;
2. `agent-a` attaches as `principal_id=planner`;
3. `agent-b` attaches as `principal_id=reviewer`;
4. both can see `/apps/shared/notion/...`;
5. `planner` sees `/apps/private/planner/chat/...`;
6. `reviewer` sees `/apps/private/reviewer/chat/...`;
7. connector binds those two chat views to different upstream sessions.

## 8. Policy Rules

Recommended future rules:

1. shared/private/team is decided by AppFS app policy, not by agent-local convention;
2. private apps must never implicitly fall back to another principal's view;
3. connectors must not reuse upstream session state across principals unless the app policy is explicitly shared;
4. AppFS should make the effective principal and visibility observable in status/control output.

## 9. Why This Is Deferred

This design should wait until the current baseline is stable because it introduces a new semantic layer on top of attach.

Doing it now would mix together:

1. attach correctness;
2. multi-agent correctness;
3. app visibility policy;
4. connector account/session isolation.

That would make regressions harder to localize.

The recommended order is:

1. finish and stabilize attach contract v1.1;
2. automate IC-2 multi-agent attach baseline;
3. keep registered app loop green;
4. only then start principal/visibility design work.

## 10. Suggested Future Milestones

### P1. Principal-aware attach metadata

Add optional attach metadata:

1. `principal_id`
2. `visibility`
3. optional `profile_id`

No path isolation yet. Status/control output only.

### P2. App policy model in AppFS

Add AppFS-side app policy for:

1. `shared`
2. `private`
3. `team`

No connector-side enforcement yet beyond metadata plumbing.

### P3. Principal-aware namespace routing

Expose explicit shared/private/team mount paths and derive view resolution from AppFS policy.

### P4. Connector session/profile isolation

Require relevant connectors to bind upstream session/account/profile by `principal_id` or `profile_id`.

## 11. Open Questions

These do not block current work:

1. should `principal_id` be supplied by launcher env, control action, or both?
2. should private app views live in one shared mount namespace or separate virtual roots?
3. how should team-scoped views express membership and authorization?
4. should AppFS persist principal-to-app policy centrally or let app registration supply it?

## 12. Final Position

For future shared/private app data support:

1. the main policy belongs in AppFS;
2. `appfs-agent` should only declare attach and principal intent;
3. connectors should isolate upstream app sessions accordingly;
4. this is future work, not the current primary delivery target.
