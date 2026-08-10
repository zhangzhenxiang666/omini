# AGENTS.md

Project instructions for coding agents working in this repository.

## Working Style

- Before cross-crate or core-flow changes, read `ARCHITECTURE.md` to understand crate responsibilities and event flow.
- Think before coding. State assumptions when they affect behavior, and ask when ambiguity would change the implementation.
- Prefer the smallest implementation that solves the request. Do not add speculative features, abstractions, configurability, or error handling.
- Make surgical changes. Touch only files and lines that directly support the requested work.
- Match nearby style, even when it differs from personal preference.
- Use comments in proportion to complexity: add a short orienting note when behavior is non-obvious or easy to misread, expand only for genuinely complex logic, and omit comments when the code is self-explanatory.
- Do not refactor unrelated code, rewrite comments, or clean up pre-existing dead code unless explicitly asked.
- If your changes make imports, variables, functions, or tests unused, clean up only the unused code introduced by your changes.
- For non-trivial work, define success criteria and verify them with the narrowest useful check.

## Rust Style

- Prefer absolute `use` paths for imports, such as `crate::...`, `omini_core::...`, `omini_protocol::...`, `omini_server::...`, `omini_tui::...`, `std::...`, or dependency crate paths.
- Avoid adding new `super::...` imports when an absolute path is clear. Keep existing `super::...` imports if changing them is unrelated churn or if nearby code strongly favors that style.
- When multiple nested `if let` or `let Some(...)` checks can be expressed clearly as a Rust 2024 `if let` chain, prefer the chain form.
- Do not rewrite existing imports or nested conditionals solely to satisfy these Rust style rules. Apply them to new code and code already being edited for the task.

## Documentation Maintenance

- When a change involves user-visible features, configuration schema, CLI/TUI behavior, installation layout, permissions, MCP, provider, or model behavior, update the relevant documentation in `docs/` or `README.md` accordingly.
- If a change does not require documentation updates, note the reason in the PR or commit message (e.g., "internal refactor, no doc update needed").
- Before merging a PR that affects documentation, verify that the docs reflect the new behavior and that examples still work.

## Verification

- Prefer focused checks over broad ones when the change is narrow.
- This is a Cargo workspace. For ordinary checks and tests, target the specific crate that changed instead of running the whole workspace.
- For Rust code changes, use crate-scoped checks such as `cargo check -p <crate>` or `cargo test -p <crate>` as appropriate.
- For final acceptance of non-trivial Rust changes, run workspace-level validation with `cargo clippy --workspace` and `cargo fmt --all --check`.
- For documentation-only changes, no build or test command is required unless the documentation includes generated examples or checked snippets.
