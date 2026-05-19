# Architecture

`omini` is a Rust workspace for exploring how coding agents work. It is split into small crates so UI, runtime behavior, and shared protocol types stay separate.

## Crate Responsibilities

- `omini-cli` is the binary entrypoint. It initializes the omini root, loads config, opens the project state and database, builds `Settings`, then starts the TUI.
- `omini-tui` owns the terminal interface. It handles input, local UI state, rendering, selection, widgets, permission drawers, and the async event loop that connects the UI to the runtime.
- `omini-runtime` owns agent behavior. It contains provider API clients, the agent engine, command handling, tool execution, permissions, prompts, project config, database access, sessions, and subagent orchestration.
- `omini-types` owns shared data structures. Cross-crate messages, events, config types, display models, permission config, tool schemas, and subagent data belong here.

Keep crate boundaries strict: UI behavior should stay in `omini-tui`, agent execution should stay in `omini-runtime`, and dependency-light shared contracts should stay in `omini-types`.

## Runtime Flow

1. `omini-cli` initializes the environment and calls `omini_tui::run_ui(settings, project)`.
2. `omini-tui` creates terminal state, starts input polling, creates UI/runtime channels, and runs `AgentRuntime`.
3. User input is converted into `UiToRuntimeEvent` values and sent from the TUI to the runtime.
4. `AgentRuntime` receives UI requests, applies command/session state, and drives the query engine.
5. The engine builds prompts, calls `LlmClient`, streams provider events, executes requested tools through `ToolRegistry`, and emits runtime events.
6. Runtime events are converted into `RuntimeToUiEvent` values and sent back to the TUI.
7. The TUI updates `UiState`, handles permission/user-input pauses, and redraws the terminal.

The channel boundary is intentional. Avoid direct UI calls from runtime code and avoid embedding runtime execution details in TUI modules.

## Shared Types

Use `omini-types` when a type crosses crate boundaries or describes the protocol between UI, runtime, tools, providers, or persisted messages.

Prefer keeping implementation details out of `omini-types`. If a type is only used inside one crate, define it in that crate until there is a real cross-crate need.

## Where Changes Usually Go

- CLI startup, config loading order, or database initialization: `omini-cli`.
- Terminal layout, keyboard/mouse handling, rendering, selection, widgets, or permission UI: `omini-tui`.
- Agent loop behavior, command semantics, tool behavior, provider calls, permissions, sessions, subagents, or persistence: `omini-runtime`.
- Event payloads, message shapes, config contracts, display records, permission schemas, or tool definitions shared by multiple crates: `omini-types`.

When a change touches more than one crate, update the shared type first, then adjust runtime behavior, then update the UI presentation.

## Validation Strategy

For ordinary development, run focused crate-level checks for the crate you changed, such as `cargo check -p omini-runtime` or `cargo test -p omini-tui`.

For final acceptance of non-trivial Rust changes, run workspace-level validation:

```bash
cargo clippy --workspace
cargo fmt --all --check
```
