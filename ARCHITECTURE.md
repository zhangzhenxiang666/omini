# Architecture

`omini` is a terminal client backed by a local daemon. The client handles interaction,
the server owns project and thread orchestration, and core runs the agent.

```text
omini-cli / omini-tui
          |
          | HTTP + WebSocket (omini-protocol)
          v
     omini-server
          |
          | commands, events, snapshots (omini-runtime-contract)
          v
      omini-core
```

## Crate Boundaries

| Crate | Responsibility |
| --- | --- |
| `omini-cli` | Binary entrypoint, daemon startup/discovery, project registration, and TUI launch. |
| `omini-tui` | Terminal input, rendering, client-side interaction state, and protocol requests. |
| `omini-protocol` | Public HTTP and WebSocket DTOs shared by clients and the server. |
| `omini-server` | Local daemon, project/thread lifecycle, controllers, event projection, replay, and SQLite persistence. |
| `omini-runtime-contract` | Internal commands, events, snapshots, and persistence requests exchanged by server and core. |
| `omini-core` | Agent loop, tools, prompts, skills, subagents, compaction, plans, and provider/MCP orchestration. |
| `omini-config` | User/project config resolution and Omini-managed filesystem paths. |
| `omini-domain` | Stable shared value types with no transport, runtime, or persistence behavior. |
| `omini-permissions` | Permission policy parsing and allow/ask/deny decisions. |
| `omini-provider-api` | Provider HTTP/SSE clients and provider-facing request/response handling. |
| `omini-mcp-client` | MCP connections, lifecycle, catalog loading, and remote calls. |

The main dependency rules are:

- `omini-protocol` is the public client/server boundary; `omini-runtime-contract` is the
  private server/core boundary. Neither contains runtime implementation details.
- `omini-domain` contains shared vocabulary only. Config loading, API keys, orchestration,
  persistence, transport envelopes, and UI state stay with their owning crates.
- Provider, MCP, and permission implementations remain independent services consumed by
  core rather than leaking through protocol or runtime-contract types.
- Server persistence consumes `RuntimePersistenceEvent`; SQLite schema, transactions,
  and replay remain server concerns.
- Server code uses core's public project/session capabilities instead of deep-linking
  into skills, subagents, tools, or engine internals.

## Project Identity and Storage

Projects persist four distinct values:

| Value | Meaning |
| --- | --- |
| `id` | Stable public identity, daemon cache key, and thread foreign key. |
| `path` | Canonical working directory; may change through relinking. |
| `storage_key` | Stable directory name under `~/.omini/projects/`. |
| `name` | User-facing display name. |

`id` and `storage_key` never change. Relinking updates only `path` and is rejected while
a cached thread is running or connected. Every thread, including forks and subagents,
belongs to a project.

## Runtime Flow

1. `omini-cli` starts or connects to the daemon, registers the canonical current
   directory, and opens the returned project UUID.
2. `omini-server` resolves the project from SQLite and lazily creates its
   `ProjectManager` using the current path and stable project storage directory.
3. `omini-tui` creates or selects a thread, claims controller status, and subscribes to
   its event stream.
4. The server validates requests and controller ownership, then invokes the relevant
   core capability through the runtime boundary.
5. Core runs the agent and emits runtime and persistence events. The server persists,
   projects, and broadcasts them to the controller and observers.
6. Reconnecting clients reopen the project by UUID; the current path is not used as
   project identity.
