---
name: skill-creator
description: Create or update Omini skills from reusable workflows, prior conversation context, artifacts, or user examples. Use when the user asks to turn a completed process into a skill, create a new skill, update an existing skill, or package domain-specific instructions/resources for future reuse.
---

# Skill Creator

Use this skill to create or update an Omini skill. A skill is a directory with a required `SKILL.md` file and optional bundled resources that teach future Omini sessions a reusable workflow.

## Workflow

1. Understand the intended skill before writing files.
   - Gather concrete examples of requests that should trigger the skill.
   - If the user says to turn the current or prior workflow into a skill, summarize the reusable procedure from the conversation and ask only for missing high-impact details.
   - Identify whether the workflow is project-specific, user-wide, or reusable across many repositories.

2. Ask where to create or update the skill unless the user already specified it.
   - Project skill: `.omini/skills/<skill-name>/SKILL.md`
   - User skill: `~/.omini/skills/<skill-name>/SKILL.md`
   - Recommend project scope for repo-specific conventions, schemas, local tools, or product context.
   - Recommend user scope for general workflows the user wants across projects.

3. Choose a skill name.
   - Use lowercase letters, digits, and hyphens only.
   - Prefer short verb-led names under 64 characters.
   - Name the folder exactly after the skill name.

4. Write `SKILL.md`.
   - Use YAML frontmatter with required `name` and `description`.
   - Put trigger information in `description`; the body is loaded only after the skill is selected.
   - Use `metadata.inject: false` only when the skill should be available by command/tool but not listed in the system prompt.
   - Keep the body concise and procedural. Do not explain obvious general coding behavior.

5. Add resources only when they reduce repeated work.
   - `references/`: detailed docs, schemas, policies, API notes, examples.
   - `scripts/`: deterministic repeatable operations; test scripts before relying on them.
   - `assets/`: templates, images, boilerplate, or files used as output material.
   - Do not create README, CHANGELOG, installation guides, or auxiliary docs unless explicitly requested.

6. Validate the result.
   - Re-read the created `SKILL.md`.
   - Confirm frontmatter parses, name matches the folder, description is trigger-rich, and body is non-empty.
   - For substantial skills, mentally test against one realistic user request and tighten the instructions if the trigger or workflow is vague.

## Writing Guidance

- Assume the future model is capable; include only non-obvious procedural knowledge.
- Prefer concise examples over long explanations.
- Move large variant-specific details into directly referenced files under `references/`.
- Keep references one level from `SKILL.md`; avoid chains of nested documentation.
- Use imperative instructions that tell the model what to do when the skill is active.

## Output

When reporting back, state the skill name, scope, path, and any resources created or updated. If the user asked only for a draft, provide the proposed `SKILL.md` content without writing files.
