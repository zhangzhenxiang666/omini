# Architecture

`omini` uses a local daemon architecture. The active crate set is:

```text
crates/
  omini-cli
  omini-config
  omini-core
  omini-domain
  omini-mcp-client
  omini-provider-api
  omini-protocol
  omini-runtime-api
  omini-server
  omini-tui
```

## Crate Responsibilities

- `omini-cli` is the user-facing binary entrypoint. It starts or connects to the installed `omini-server` daemon binary, registers the current project, and starts the TUI/client flow.
- `omini-config` owns user and project configuration management: user config schema/loading/validation, project-level partial config merge, resolved runtime settings, Omini root paths, managed project state paths, and project/session directory handles.
- `omini-domain` owns stable data types and small domain helpers shared by core, protocol, TUI, and server-adjacent code: messages, tool definitions, usage, display/history records, proposed plan parsing, shared provider/model view enums, plan approval payloads, tool pause payloads, session summaries, compact event payloads, and subagent event payloads.
- `omini-mcp-client` owns client-side MCP server runtime concerns: stdio and streamable HTTP connections, rmcp service lifecycle, catalog loading, status snapshots, and tool/resource/prompt calls. It stays independent from core, protocol, server, and TUI.
- `omini-provider-api` owns provider-facing API clients and shared LLM provider request/response glue used by core.
- `omini-core` owns the agent implementation: provider clients, engine loop, tools, MCP capability adapters, runtime permission decisions, prompts, skills, subagents, compaction, plan handling, and session runtime service. It consumes resolved settings from `omini-config` instead of owning user/project config loading.
- `omini-protocol` owns public HTTP and WebSocket request/response envelopes. It reuses or re-exports `omini-domain` types when the wire shape matches, and it must stay independent from core, server, and TUI implementation types.
- `omini-runtime-api` owns the narrow server-core runtime contract: server-to-runtime events, runtime-to-server events, session commands/snapshots, project agent mutation commands, runtime capability/status snapshots, and runtime persistence events consumed by server. It must stay independent from core, server, protocol, CLI, and TUI implementation types.
- `omini-server` owns the local daemon transport. It exposes HTTP endpoints, session-scoped WebSocket streams, session routing, multi-subscriber fanout, and per-session controller enforcement.
- `omini-tui` owns the terminal client: input, rendering, local slash commands, mention parsing, attachment parsing, local UI state, permission drawers, and protocol request construction.

`omini-domain` must not grow into a runtime or config crate. Config loading, API keys, runtime services, TUI state machines, HTTP envelopes, daemon/session orchestration, persistence, and provider clients do not belong in domain.

## Dependency Direction

The intended final dependency direction is:

```text
omini-domain
      ^
      |
      +-- omini-config
      |
      +-- omini-protocol
      |
      +-- omini-runtime-api

omini-protocol
      ^
      |
      +-- omini-server
      |
      +-- omini-tui

omini-runtime-api
      ^
      |
      +-- omini-server --> omini-core
      |
      +-- omini-core

omini-provider-api --> omini-domain
omini-mcp-client

omini-core --> omini-domain + omini-config + omini-runtime-api + omini-provider-api + omini-mcp-client
omini-server --> omini-domain + omini-config + omini-protocol + omini-runtime-api + omini-core
omini-tui  --> omini-domain + omini-protocol
omini-cli --> omini-protocol + omini-tui
```

Current boundary notes:

- `omini-domain` owns the shared stable type surface used by core, TUI, and protocol.
- `omini-config` owns user-level `~/.omini/config.toml`, project-level `<cwd>/.omini/config.toml`, merge/validation into effective settings, `OminiRoot`, and Omini-managed project/session directory handles. Project config is a partial overlay and project fields take precedence over user config fields.
- `omini-provider-api` owns provider HTTP/SSE adapters and may depend on stable domain config/types, but it must not depend on core, server, protocol, CLI, or TUI.
- `omini-mcp-client` owns the client-side MCP runtime layer and must not depend on core, server, protocol, CLI, or TUI.
- `omini-runtime-api` is the explicit communication contract shared by `omini-server` and `omini-core`. Server should import runtime command/event/snapshot types from it directly instead of deep-linking core modules.
- `EngineToRuntimeEvent`, `QueryEngine`, provider request/stream types, tool execution internals, config loading, API keys, project state loading, and persistence implementations do not belong in `omini-runtime-api`; they stay in `omini-config`, core, or their existing owner crates.
- `RuntimePersistenceEvent` is part of the server-core contract because it is core output consumed by server persistence. SQLite schema, transactions, replay trimming, and store errors remain in `omini-server`.
- `AgentCoreSession` exposes runtime-api/domain snapshots and commands. `omini-server` owns protocol response/event projection for those snapshots.
- `omini-core` owns MCP capability adapter behavior: runtime tool registration, permission previews, tool result metadata, and MCP runtime snapshot production. `omini-runtime-api` owns only the snapshot structs consumed by server. `omini-server` owns protocol status projection. `omini-mcp-client` owns only the client-side MCP lifecycle/catalog/call layer.
- Skills and subagents discovery/file management are core implementation details. `omini-server` should call root-level core project capability facade functions and session snapshots instead of deep-linking `omini_core::skills` or `omini_core::subagents`.
- Stable shared display/message/event/usage/subagent payloads are imported from `omini-domain` directly; `omini-core` does not provide `types/*` compatibility re-exports for those payloads. External crates should not deep-link through `omini_core::types`; server-core session command/snapshot and project agent mutation command types are exposed by `omini-runtime-api`, while `AgentCoreSession` and root-level core facade functions consume or return those contract types.
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

`RuntimeEvent` carries a required typed protocol event. WebSocket clients consume `TypedRuntimeEvent` directly instead of decoding legacy `{ kind, payload }` runtime JSON.

Typed runtime events cover the current TUI-consumed runtime stream, including:

- run started/finished
- notifications
- tool pause requested
- plan submitted
- session snapshot
- model, usage, title, thinking display, stream delta, tool use/result, agent, compact, and subagent events
- controller changes through `ServerEnvelope::ControllerChanged`

## Where Changes Usually Go

- CLI startup, installed daemon binary lookup, and client registration/attach: `omini-cli`.
- Config schema, user/project config loading and merge, resolved settings, Omini root paths, project state paths, and project/session directory handles: `omini-config`.
- Project attach orchestration and database initialization: `omini-server`.
- Stable shared messages, usage records, display/history records, plan parsing helpers, no-secret provider/model view types, plan approval payloads, tool pause payloads, compact payloads, and subagent payloads: `omini-domain`.
- Provider HTTP clients, OpenAI/Anthropic adapters, SSE parsing, provider request/stream errors, and `LlmClient`: `omini-provider-api`.
- Server-core runtime contract events, session command/snapshot types, project agent mutation commands, runtime MCP snapshots, and runtime persistence events: `omini-runtime-api`.
- Agent behavior, tools, provider orchestration, MCP capability adapters, runtime permission decisions, prompts, skills, subagents, compaction, and plans: `omini-core`.
- Client-side MCP connection lifecycle, catalog loading, status snapshots, and tool/resource/prompt calls: `omini-mcp-client`.
- Public endpoint bodies, user input DTOs, attachments, controller DTOs, and typed WebSocket event DTO envelopes: `omini-protocol`.
- HTTP routes, WebSocket fanout, session registry, controller lease enforcement, event persistence/replay, and daemon concerns: `omini-server`.
- Terminal rendering, input/editing state, local commands, mention parsing, and observer/controller UI affordances: `omini-tui`.

## Validation Strategy

For ordinary development, run focused crate-level checks:

```bash
cargo test -p omini-domain
cargo test -p omini-config
cargo test -p omini-provider-api
cargo test -p omini-mcp-client
cargo check -p omini-runtime-api
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
