# Architecture

`omini` uses a local daemon architecture. The active crate set is:

```text
crates/
  omini-cli
  omini-core
  omini-domain
  omini-protocol
  omini-server
  omini-tui
```

## Crate Responsibilities

- `omini-cli` is the user-facing binary entrypoint. It starts or connects to the installed `omini-server` daemon binary, registers the current project, and starts the TUI/client flow.
- `omini-domain` owns stable data types and small domain helpers shared by core, protocol, TUI, and server-adjacent code: messages, tool definitions, usage, display/history records, proposed plan parsing, shared provider/model view enums, plan approval payloads, tool pause payloads, session summaries, compact event payloads, and subagent event payloads.
- `omini-core` owns the agent implementation: provider clients, engine loop, tools, MCP, permissions, prompts, skills, subagents, compaction, plan handling, session runtime service, config, project state, and SQLite helpers during migration.
- `omini-protocol` owns public HTTP and WebSocket request/response envelopes. It reuses or re-exports `omini-domain` types when the wire shape matches, and it must stay independent from core, server, and TUI implementation types.
- `omini-server` owns the local daemon transport. It exposes HTTP endpoints, session-scoped WebSocket streams, session routing, multi-subscriber fanout, and per-session controller enforcement.
- `omini-tui` owns the terminal client: input, rendering, local slash commands, mention parsing, attachment parsing, local UI state, permission drawers, and protocol request construction.

`omini-domain` must not grow into a runtime or config crate. Config loading, API keys, runtime services, TUI state machines, HTTP envelopes, daemon/session orchestration, persistence, and provider clients do not belong in domain.

## Dependency Direction

The intended final dependency direction is:

```text
omini-domain
      ^
      |
omini-protocol
      ^
      |
      +-- omini-server --> omini-core
      |
      +-- omini-tui

omini-core --> omini-domain + omini-protocol
omini-tui  --> omini-domain + omini-protocol
omini-cli --> omini-protocol + omini-tui
```

Current boundary notes:

- `omini-domain` owns the shared stable type surface used by core, TUI, and protocol.
- `omini-core` still contains protocol adapter code in `AgentCoreSession`; that adapter should move toward the server boundary so core can become protocol-independent.
- `omini-core` still contains a crate-private legacy command registry for compatibility; do not expose it or add new command behavior there.

## Runtime Flow

1. `omini-cli` starts or connects to the local `omini-server` daemon binary, attaches the current project, and starts the local client flow.
2. `omini-tui` starts or connects to the local server, creates/selects a session, claims controller status, and subscribes to `/sessions/{session_id}/ws`.
3. TUI input is translated locally: slash commands stay client UX, `@` mentions become semantic context refs, and image markers become attachment refs.
4. `omini-server` validates HTTP requests, applies controller conflict rules, and routes accepted requests to `omini-core`.
5. `omini-core` runs the agent loop and emits internal runtime events.
6. Runtime events are wrapped as protocol events and broadcast by `omini-server` to every subscriber for that session.
7. Observers receive the same events as the controller. Running-related user actions from connected clients first take over controller status, while stricter mutations such as agent edits, attachments, and session metadata changes still require the active controller.

## Protocol Events

`RuntimeEvent` currently preserves the legacy `{ kind, payload }` shape for TUI compatibility and carries an optional typed `event` overlay for key events. New clients should prefer typed event data when present.

Stable first-pass typed events include:

- run started/finished
- notifications
- tool pause requested
- plan submitted
- session snapshot
- controller changes through `ServerEnvelope::ControllerChanged`

Non-critical UI events may continue to use legacy payloads temporarily.

## Where Changes Usually Go

- CLI startup, installed daemon binary lookup, and client registration/attach: `omini-cli`.
- Config loading, project initialization, and database initialization: `omini-server`.
- Stable shared messages, usage records, display/history records, plan parsing helpers, no-secret provider/model view types, plan approval payloads, tool pause payloads, compact payloads, and subagent payloads: `omini-domain`.
- Agent behavior, tools, providers, permissions, prompts, skills, subagents, compaction, plans, config, and project state: `omini-core`.
- Public endpoint bodies, user input DTOs, attachments, controller DTOs, and typed WebSocket event DTO envelopes: `omini-protocol`.
- HTTP routes, WebSocket fanout, session registry, controller lease enforcement, event persistence/replay, and daemon concerns: `omini-server`.
- Terminal rendering, input/editing state, local commands, mention parsing, and observer/controller UI affordances: `omini-tui`.

## Validation Strategy

For ordinary development, run focused crate-level checks:

```bash
cargo test -p omini-domain
cargo check -p omini-protocol
cargo check -p omini-core
cargo check -p omini-server
cargo check -p omini-tui
```

For final acceptance of non-trivial Rust changes:

```bash
cargo clippy --workspace
cargo fmt --all --check
```
