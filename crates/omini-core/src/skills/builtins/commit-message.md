---
name: commit-message
description: Suggest git commit messages from current repository changes without staging, committing, or mutating repository state. Use when the user asks for commit message ideas, an atomic commit plan, or help wording commits from staged or unstaged changes.
---

# Commit Message

Use this skill to suggest commit messages from current repository changes. This skill is read-only: do not stage files, create commits, stash changes, reset changes, clean files, or write persistent cache/state as part of this workflow.

## Workflow

1. Inspect repository state with read-only commands.
   - Run `git status --short`.
   - Inspect staged changes with `git diff --cached`.
   - Inspect unstaged changes with `git diff`.
   - Review recent message style with `git log --oneline -n 10`.

2. Understand the change set.
   - Group files by coherent intent, not by file type.
   - Identify unrelated, accidental, generated, or risky changes.
   - If staged and unstaged changes are mixed, keep that distinction visible in the recommendation.

3. Suggest atomic commit groups when useful.
   - Prefer one commit per coherent intent, and recommend splitting only when the changes can stand alone.
   - Separate feature work, bug fixes, refactors, tests, docs, and formatting when they are independently meaningful.
   - Include the related files for each suggested atomic commit whenever the grouping is clear.
   - If the grouping is ambiguous or the changes are tightly coupled, suggest one message and explain the ambiguity.

4. Match project message style.
   - Infer the dominant language, prefix style, scope style, tense, casing, and subject length from recent commits.
   - Preserve the commit language used by recent history by default; only switch languages when the user explicitly asks.
   - If the repo uses Conventional Commits, follow it.
   - If the repo uses Chinese, English, or mixed-language subjects, match that language pattern.
   - Keep the subject concise and specific.

## Output

Report only suggestions:

- Suggested commit message for each clear group.
- Related files for each atomic group, when useful.
- Notes for unrelated, risky, or ambiguous changes.

Do not include `git add`, `git commit`, stash, reset, cleanup, or cache-writing commands unless the user explicitly asks for separate operational guidance outside this skill.
