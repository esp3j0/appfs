# AppFS ChatApp And Agent Interaction Backbone Design

**Date:** 2026-04-22  
**Status:** Draft future plan  
**Depends on:** AppFS attach contract v1.1, AppFS registered-app/runtime registry baseline, AppFS event stream baseline, [AppFS Principal Visibility And Agent Identity Design](./2026-04-07-appfs-principal-visibility-and-agent-identity.md)

## 1. Goal

Define the intended long-term interaction model between AppFS and `appfs-agent`:

1. one AppFS-native `ChatApp` becomes the primary surface for:
   1. agent-to-user conversation;
   2. agent-to-agent direct messaging;
   3. shared multi-agent collaboration;
2. legacy tool concepts such as `SendMessage` and `AskUserQuestion` are no longer treated as
   fundamental protocol primitives;
3. before implementing `ChatApp`, the stack first lands the enabling capabilities that make a
   chat-native architecture viable.

This document is intentionally about the **backbone** needed before `ChatApp` is built, not only
about the eventual app schema.

## 2. Main Recommendation

The preferred architecture is:

1. AppFS provides one chat-native app contract rather than separate protocol surfaces for
   "send message" and "ask user question";
2. `appfs-agent` consumes that contract as a general interaction plane;
3. "asking a question" is represented as a normal chat message with optional interaction
   metadata, not as a separate first-class question object;
4. AppFS, not `appfs-agent`, owns visibility and principal-aware routing for private/shared/team
   app views;
5. `appfs-agent` must be able to discover visible apps, inject concise app capability context
   into the model prompt, and resume waiting sessions when matching AppFS events arrive.

The immediate implementation priority is therefore **not** "build `ChatApp` first". It is:

1. strengthen attached agent identity;
2. expose visible app inventory to attached agents;
3. enforce private/shared/team app visibility at the AppFS layer;
4. let `appfs-agent` subscribe to and route AppFS events in code;
5. then add `ChatApp` on top of those primitives.

## 3. Why This Direction Is Better Than Tool-Parity Migration

Trying to re-create TypeScript-era `SendMessage` and `AskUserQuestion` directly inside
`appfs-agent` would keep interaction semantics client-local.

That has several drawbacks:

1. a shell client, GUI client, and `appfs-agent` would not necessarily observe the same
   interaction state;
2. private vs shared visibility would be hidden inside one consumer rather than expressed by the
   filesystem/runtime;
3. future connectors for real chat systems would have to map to agent-local semantics instead of
   to a reusable AppFS app contract;
4. "conversation", "question", and "multi-agent coordination" would become separate vertical
   systems when they are better modeled as one underlying chat surface.

The cleaner model is:

1. AppFS exposes a chat-native app surface;
2. all clients see the same visible chat state for the same principal;
3. `appfs-agent` treats chat as one more mounted app, not as a special in-process side channel.

## 4. Long-Term Conceptual Model

### 4.1 Participants and identities

The stack should distinguish at least these identities:

1. `runtime_session_id`
   1. identifies one AppFS runtime / mount lifecycle;
2. `attach_id`
   1. identifies one attached `appfs-agent` process instance;
3. `principal_id`
   1. identifies a stable actor identity such as `planner`, `reviewer`, `user`, or `ops-bot`.

`attach_id` is not enough for principled chat routing or app visibility. Stable chat membership
and private app access should be keyed by `principal_id`.

### 4.2 Spaces

The foundational chat abstraction should be a `space`.

Expected initial space kinds:

1. direct/private message between two principals;
2. group/team conversation among multiple principals.

This gives one unified surface for:

1. user-agent private conversation;
2. agent-agent private conversation;
3. multi-agent shared collaboration.

### 4.3 Messages

The primary primitive should be a message record, not a question record.

Important message fields are expected to include:

1. `id`
2. `space_id`
3. `ts`
4. `author`
5. `text`
6. `reply_to` optional
7. `correlation_id` optional
8. `visibility` optional
9. `interaction` optional
10. `attachments` optional

### 4.4 Structured reply metadata

"Question asking" should be modeled as interaction metadata on a message.

Examples:

1. expects a free-text reply;
2. expects one option from a small list;
3. includes a correlation identifier that a waiting agent session can match later.

This keeps the system chat-native while still supporting human-in-the-loop and workflow prompts.

### 4.5 Visibility

Chat visibility should align with the principal/visibility design already proposed for AppFS:

1. `shared`
2. `private`
3. `team`

This is not just a UI hint. It must affect what filesystem view is visible to which attached
principal.

## 5. Prerequisite Capabilities Before `ChatApp`

The following work should be completed before `ChatApp` becomes the primary interaction surface.

### 5.1 Attached agent identity must be richer than process identity

`appfs-agent` already has an initial notion of attach identity, but the design must be extended
so AppFS can reason about:

1. who the current attached process is;
2. which stable principal that process represents;
3. which visibility scope that principal is allowed to observe.

Near-term consequence:

1. `appfs-agent` should continue to declare attach identity hints;
2. AppFS should become the source of truth for effective principal and visibility.

### 5.2 Agents need visible app discovery and prompt injection

`appfs-agent` should not assume only one mounted app or only hard-coded tool semantics.

Instead it should be able to:

1. inspect the registered app inventory visible from the current principal;
2. read concise app descriptions and usage hints;
3. inject a compact app catalog into the agent context.

The injected context should be concise and capability-oriented, for example:

1. app name and short description;
2. important readable resources;
3. important writable actions;
4. usage examples and constraints;
5. visibility notes.

This catalog should be derived from AppFS-visible metadata, not from `appfs-agent` hard-coded
knowledge.

### 5.3 App visibility must be enforced at the AppFS layer

If private/public/team behavior exists only in `appfs-agent`, a different client could bypass it.

So AppFS must own:

1. whether an app is shared, private, or team-visible;
2. which principals can observe which app data;
3. which filesystem paths are shown for the current principal.

For this design, the key requirement is:

1. an agent should only be able to read the app surface that AppFS has made visible for its
   principal;
2. `appfs-agent` should not be the only component enforcing those visibility rules.

### 5.4 `appfs-agent` must consume AppFS events in code

Watching `tail -f` manually is not enough for runtime integration.

`appfs-agent` needs a code-level event subscription/reading path so it can:

1. observe action completions;
2. observe new messages;
3. observe replies that should wake a waiting session;
4. observe relevant app changes without polling every app directory blindly.

This is the minimum needed for future "wait for reply" behavior.

### 5.5 Event routing needs demultiplexing and resume semantics

It is not enough to receive all AppFS events globally. `appfs-agent` must be able to route them
to the correct waiting consumer.

Matching keys are expected to include:

1. `principal_id`
2. `space_id`
3. `reply_to`
4. `correlation_id`
5. optional explicit target or mention metadata

This enables:

1. one waiting agent session to resume on a matching human reply;
2. one group message to notify only the relevant principals or subscribers;
3. multiple attached agents on the same runtime to avoid trampling each other's wait state.

### 5.6 Code-write isolation remains a separate concern

Even if `ChatApp` replaces `SendMessage` and `AskUserQuestion`, it does not replace workspace
write isolation.

If multiple agents still edit the same repository in parallel, worktree or equivalent isolation
may still be needed.

This design should therefore not be interpreted as:

1. "chat replaces worktree";
2. "messaging isolation is enough for safe parallel code edits".

## 6. Responsibility Split

### 6.1 AppFS

AppFS should own:

1. registered app inventory;
2. principal-aware visible app surface;
3. app metadata readable by clients;
4. event stream publication;
5. app-native contracts such as a future `ChatApp`;
6. routing-relevant filesystem state that remains correct regardless of client.

### 6.2 `appfs-agent`

`appfs-agent` should own:

1. declaring attach identity hints when attaching;
2. reading visible app metadata and injecting a compact app catalog into model context;
3. mapping visible app usage into agent behavior;
4. subscribing to AppFS events in code;
5. resuming waiting local sessions when matching AppFS replies/events arrive.

`appfs-agent` should not own the authoritative rules for:

1. private/shared/team app visibility;
2. which principal can see which app data;
3. canonical app-level interaction semantics.

### 6.3 Connectors

Connectors should own:

1. binding AppFS principals/profiles to upstream accounts or sessions;
2. preserving upstream isolation across principals where required;
3. translating upstream APIs into AppFS snapshots/actions/events.

This matters especially for real chat apps, where two different principals should not
accidentally share the same upstream account state.

### 6.4 `appfs-platform`

The platform monorepo should own:

1. cross-project ADRs when both repositories need coordinated changes;
2. integration fixtures and end-to-end scenarios;
3. combined rollout/testing plans.

It should not be the primary source of truth for the AppFS chat contract itself.

## 7. Deferred `ChatApp` Surface Sketch

Once the prerequisites above are in place, the future `ChatApp` can remain thin and regular.

Illustrative resources/actions:

1. `spaces.res.json`
2. `participants.res.json`
3. `spaces/<space_id>/members.res.json`
4. `spaces/<space_id>/messages.res.jsonl`
5. `spaces/<space_id>/post_message.act`

Illustrative event types:

1. `message.created`
2. `message.updated`
3. `message.deleted`
4. `member.joined`
5. `member.left`
6. `interaction.responded`
7. `action.completed`
8. `action.failed`

The exact path layout may evolve with the principal/visibility namespace decision. This section is
therefore intentionally illustrative, not frozen.

## 8. Suggested Delivery Order

Recommended sequence:

1. finish principal-aware attach metadata and visibility groundwork;
2. expose visible app catalog/metadata in AppFS;
3. let `appfs-agent` read that catalog and inject compact app usage context;
4. add code-level AppFS event consumption to `appfs-agent`;
5. add wait/resume routing on top of correlation-aware event matching;
6. only then implement `ChatApp` as the primary interaction app.

This keeps the product direction aligned with AppFS-native interaction while avoiding a premature
chat-only implementation that still lacks identity, visibility, and wake/resume support.

## 9. Explicit Non-Goals

This document does not yet define:

1. the final frozen `ChatApp` schema;
2. a specific UI for rendering chat or reply prompts;
3. a replacement for worktree isolation;
4. a full multi-agent coordinator policy;
5. every connector-specific mapping for real external chat providers.

## 10. Open Questions

Questions that should be answered before or during implementation:

1. should app-level events be published only in app-local streams, or also projected into a
   global control/event stream for efficient subscription?
2. what is the canonical AppFS metadata surface for app descriptions, examples, and usage hints?
3. how should AppFS represent the end user as a principal when the user is not an attached
   `appfs-agent` process?
4. should direct/private message spaces be discoverable by both participants through one shared
   path or through principal-filtered mirrored views?
5. how much app metadata should `appfs-agent` inject by default before token cost becomes too
   high?
