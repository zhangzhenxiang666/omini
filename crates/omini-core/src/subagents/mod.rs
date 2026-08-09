use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

mod builtins;
mod generator;
mod parser;
mod tasks;

pub(crate) use generator::{GenerateAgentDraftError, generate_agent_draft_checked_from_settings};
use omini_domain::subagents::{AgentDraft, AgentRecord, AgentSourceKind, AgentSummary};
pub(crate) use tasks::AgentTaskCompletion;
pub use tasks::AgentTaskSupervisor;

#[derive(Debug, Clone)]
pub struct AgentTaskRequest {
    pub name: String,
    pub prompt: String,
    pub title: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentSpec {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) short_description: Option<String>,
    pub(crate) instructions: String,
    pub(crate) tool_policy: AgentToolPolicy,
    pub(crate) model: Option<AgentModelSpec>,
    source: AgentSource,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AgentToolPolicy {
    pub(crate) allow: Option<Vec<String>>,
    pub(crate) deny: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentModelSpec {
    pub(crate) provider: String,
    pub(crate) model: String,
}

#[derive(Debug, Clone)]
enum AgentSource {
    BuiltIn,
    File(PathBuf),
}

impl AgentSource {
    fn display(&self) -> String {
        match self {
            AgentSource::BuiltIn => "<built-in>".to_string(),
            AgentSource::File(path) => path.display().to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentLoadDiagnostic {
    message: String,
}

impl AgentLoadDiagnostic {
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone)]
pub struct AgentRegistry {
    pub(crate) agents: HashMap<String, AgentSpec>,
    pub diagnostics: Vec<AgentLoadDiagnostic>,
}

impl AgentRegistry {
    pub fn summaries(&self) -> Vec<AgentSummary> {
        let mut summaries: Vec<_> = self
            .agents
            .values()
            .map(|agent| AgentSummary {
                name: agent.name.clone(),
                description: agent.description.clone(),
                short_description: agent.short_description.clone(),
                location: agent.source.display(),
            })
            .collect();
        summaries.sort_by(|a, b| a.name.cmp(&b.name));
        summaries
    }

    pub(crate) fn get(&self, name: &str) -> Option<&AgentSpec> {
        self.agents.get(name)
    }

    pub(crate) fn sorted_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.agents.keys().cloned().collect();
        names.sort();
        names
    }
}

pub(crate) fn load_agent_summaries(cwd: &Path) -> Vec<AgentSummary> {
    load_agent_registry(cwd).summaries()
}

pub(crate) fn load_agent_registry(cwd: &Path) -> AgentRegistry {
    let mut agent_dirs = Vec::new();
    if let Some(home_dir) = dirs::home_dir().filter(|path| !path.as_os_str().is_empty()) {
        agent_dirs.push(home_dir.join(".omini").join("agents"));
    }
    agent_dirs.push(cwd.join(".omini").join("agents"));
    load_agent_registry_from_dirs(agent_dirs)
}

pub fn list_agent_records(cwd: &Path) -> Vec<AgentRecord> {
    let mut records = Vec::new();
    for spec in builtins::built_in_agents() {
        records.push(record_from_spec(
            &spec,
            AgentSourceKind::BuiltIn,
            None,
            false,
        ));
    }

    if let Some(home_dir) = dirs::home_dir().filter(|path| !path.as_os_str().is_empty()) {
        records.extend(list_agent_records_from_dir(
            &home_dir.join(".omini").join("agents"),
            AgentSourceKind::User,
        ));
    }
    records.extend(list_agent_records_from_dir(
        &cwd.join(".omini").join("agents"),
        AgentSourceKind::Project,
    ));

    records.sort_by(|a, b| {
        source_sort(a.source_kind)
            .cmp(&source_sort(b.source_kind))
            .then_with(|| a.name.cmp(&b.name))
    });
    records
}

pub fn write_agent_file(
    cwd: &Path,
    source_kind: AgentSourceKind,
    draft: &AgentDraft,
) -> Result<PathBuf, String> {
    if source_kind == AgentSourceKind::BuiltIn {
        return Err("内置 agent 不能写入".to_string());
    }
    validate_agent_draft(draft)?;
    let dir = agent_dir(cwd, source_kind)?;
    fs::create_dir_all(&dir).map_err(|e| format!("创建 agent 目录失败: {e}"))?;
    let path = dir.join(format!("{}.md", slugify_agent_name(&draft.name)));
    let content = render_agent_file(draft);
    fs::write(&path, content).map_err(|e| format!("写入 agent 文件失败: {e}"))?;
    Ok(path)
}

pub fn delete_agent_file(path: &Path) -> Result<(), String> {
    fs::remove_file(path).map_err(|e| format!("删除 agent 文件失败: {e}"))
}

pub fn agent_name_exists(cwd: &Path, name: &str, except_path: Option<&Path>) -> bool {
    list_agent_records(cwd).into_iter().any(|record| {
        record.name == name
            && except_path.is_none_or(|except| record.path.as_deref() != Some(except))
    })
}

fn list_agent_records_from_dir(
    agents_dir: &Path,
    source_kind: AgentSourceKind,
) -> Vec<AgentRecord> {
    let entries = match fs::read_dir(agents_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut paths = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            parser::parse_agent_file(&path)
                .ok()
                .map(|spec| record_from_spec(&spec, source_kind, Some(path), true))
        })
        .collect()
}

fn record_from_spec(
    spec: &AgentSpec,
    source_kind: AgentSourceKind,
    path: Option<PathBuf>,
    editable: bool,
) -> AgentRecord {
    let tools = spec
        .tool_policy
        .allow
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|tool| !matches!(tool.as_str(), "spawn_agent" | "get_task" | "cancel_task"))
        .collect();
    let disallow_tools = spec
        .tool_policy
        .deny
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|tool| !matches!(tool.as_str(), "spawn_agent" | "get_task" | "cancel_task"))
        .collect();
    let model = spec
        .model
        .as_ref()
        .map(|model| format!("{}/{}", model.provider, model.model));
    AgentRecord {
        name: spec.name.clone(),
        description: spec.description.clone(),
        short_description: spec.short_description.clone(),
        instructions: spec.instructions.clone(),
        tools,
        disallow_tools,
        model,
        source_kind,
        path,
        editable,
    }
}

fn agent_dir(cwd: &Path, source_kind: AgentSourceKind) -> Result<PathBuf, String> {
    match source_kind {
        AgentSourceKind::BuiltIn => Err("内置 agent 没有文件目录".to_string()),
        AgentSourceKind::Project => Ok(cwd.join(".omini").join("agents")),
        AgentSourceKind::User => dirs::home_dir()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.join(".omini").join("agents"))
            .ok_or_else(|| "无法定位用户目录".to_string()),
    }
}

fn validate_agent_draft(draft: &AgentDraft) -> Result<(), String> {
    if draft.name.trim().is_empty() {
        return Err("agent 名称不能为空".to_string());
    }
    if draft.description.trim().is_empty() {
        return Err("agent 描述不能为空".to_string());
    }
    if draft.instructions.trim().is_empty() {
        return Err("系统指令不能为空".to_string());
    }
    Ok(())
}

fn render_agent_file(draft: &AgentDraft) -> String {
    let tools = draft
        .tools
        .iter()
        .filter(|tool| !matches!(tool.as_str(), "spawn_agent" | "get_task" | "cancel_task"))
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let disallow_tools = draft
        .disallow_tools
        .iter()
        .filter(|tool| !matches!(tool.as_str(), "spawn_agent" | "get_task" | "cancel_task"))
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let mut content = String::new();
    content.push_str("---\n");
    content.push_str(&format!("name: {}\n", quote_scalar(&draft.name)));
    content.push_str(&format!(
        "description: {}\n",
        quote_scalar(&draft.description)
    ));
    if let Some(short_description) = &draft.short_description {
        content.push_str(&format!(
            "short-description: {}\n",
            quote_scalar(short_description)
        ));
    }
    if !tools.is_empty() {
        content.push_str(&format!("tools: {}\n", quote_scalar(&tools)));
    }
    if !disallow_tools.is_empty() {
        content.push_str(&format!(
            "disallow_tools: {}\n",
            quote_scalar(&disallow_tools)
        ));
    }
    if let Some(model) = draft
        .model
        .as_ref()
        .filter(|model| !model.trim().is_empty())
    {
        content.push_str(&format!("model: {}\n", quote_scalar(model)));
    }
    content.push_str("---\n");
    content.push_str(draft.instructions.trim());
    content.push('\n');
    content
}

fn quote_scalar(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

fn slugify_agent_name(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            slug.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "agent".to_string()
    } else {
        slug.to_string()
    }
}

fn source_sort(kind: AgentSourceKind) -> u8 {
    match kind {
        AgentSourceKind::BuiltIn => 0,
        AgentSourceKind::Project => 1,
        AgentSourceKind::User => 2,
    }
}

fn load_agent_registry_from_dirs(agent_dirs: impl IntoIterator<Item = PathBuf>) -> AgentRegistry {
    let mut registry = AgentRegistry {
        agents: HashMap::new(),
        diagnostics: Vec::new(),
    };

    for spec in builtins::built_in_agents() {
        insert_agent(&mut registry, spec);
    }

    for agents_dir in agent_dirs {
        load_agents_from_dir(&mut registry, &agents_dir);
    }

    registry
}

fn load_agents_from_dir(registry: &mut AgentRegistry, agents_dir: &Path) {
    let entries = match fs::read_dir(agents_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            registry.diagnostics.push(AgentLoadDiagnostic {
                message: format!(
                    "failed to read subagent directory {}: {e}",
                    agents_dir.display()
                ),
            });
            return;
        }
    };

    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "md") {
                    paths.push(path);
                }
            }
            Err(e) => registry.diagnostics.push(AgentLoadDiagnostic {
                message: format!("failed to read subagent directory entry: {e}"),
            }),
        }
    }
    paths.sort();

    for path in paths {
        match parser::parse_agent_file(&path) {
            Ok(spec) => insert_agent(registry, spec),
            Err(e) => registry.diagnostics.push(AgentLoadDiagnostic {
                message: format!("skipped {}: {e}", path.display()),
            }),
        }
    }
}

fn insert_agent(registry: &mut AgentRegistry, spec: AgentSpec) {
    if let Some(existing) = registry.agents.get(&spec.name) {
        registry.diagnostics.push(AgentLoadDiagnostic {
            message: format!(
                "duplicate subagent name '{}'; keeping {}, skipping {}",
                spec.name,
                existing.source.display(),
                spec.source.display()
            ),
        });
        return;
    }
    registry.agents.insert(spec.name.clone(), spec);
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_project() -> PathBuf {
        let path = std::env::temp_dir().join(format!("omini_subagent_test_{}", Uuid::new_v4()));
        fs::create_dir_all(path.join(".omini").join("agents")).unwrap();
        path
    }

    fn write_agent(cwd: &Path, file_name: &str, content: &str) {
        fs::write(cwd.join(".omini").join("agents").join(file_name), content).unwrap();
    }

    fn load_project_agent_registry(cwd: &Path) -> AgentRegistry {
        load_agent_registry_from_dirs([cwd.join(".omini").join("agents")])
    }

    #[test]
    fn built_in_explorer_returns_parent_agent_evidence_summary() {
        let registry = load_agent_registry_from_dirs(std::iter::empty());
        let explorer = registry.agents.get("explorer").unwrap();

        assert!(explorer.instructions.contains("parent agent"));
        // 新版 instructions 用 <analysis>/<results>/<next_steps> 描述输出形态
        assert!(explorer.instructions.contains("<results>"));
        assert!(explorer.instructions.contains("<answer>"));
        assert!(explorer.instructions.contains("<next_steps>"));
    }

    #[test]
    fn built_in_explorer_is_read_only_with_parallel_strategy() {
        let registry = load_agent_registry_from_dirs(std::iter::empty());
        let explorer = registry.agents.get("explorer").unwrap();

        // read-only / 禁止写文件约束
        assert!(explorer.instructions.contains("Read-only"));
        assert!(explorer.instructions.contains("No file creation"));
        // 并发首动作约束
        assert!(explorer.instructions.contains("3+ tools simultaneously"));
        // 失败清单(相对路径/漏检/无 <results> 等)
        assert!(explorer.instructions.contains("relative"));
        assert!(explorer.instructions.contains("`<results>` block"));
        // 工具策略改写为 search/read/bash
        assert!(explorer.instructions.contains("`search`"));
        assert!(explorer.instructions.contains("`read`"));
        assert!(explorer.instructions.contains("`bash`"));
    }

    #[test]
    fn parses_custom_agent_with_string_tools() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "translator.md",
            r#"---
name: code-comment-translator
description: "Translate code comments"
tools: "Bash, Read, RunAgent"
---
Translate comments without changing code.
"#,
        );

        let registry = load_project_agent_registry(&cwd);
        let agent = registry.agents.get("code-comment-translator").unwrap();

        assert!(registry.diagnostics.is_empty());
        assert_eq!(agent.description, "Translate code comments");
        assert_eq!(
            agent.tool_policy.allow.as_deref(),
            Some(
                [
                    "bash".to_string(),
                    "read".to_string(),
                    "run_agent".to_string()
                ]
                .as_slice()
            )
        );
        assert!(agent.tool_policy.deny.is_none());
        assert_eq!(
            agent.instructions,
            "Translate comments without changing code."
        );
    }

    #[test]
    fn parses_custom_agent_with_array_tools() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "reader.md",
            r#"---
name: reader
description: Read only
tools: ["Read", "bash"]
---
Inspect files and report findings.
"#,
        );

        let registry = load_project_agent_registry(&cwd);
        let agent = registry.agents.get("reader").unwrap();

        assert!(registry.diagnostics.is_empty());
        assert_eq!(
            agent.tool_policy.allow.as_deref(),
            Some(["read".to_string(), "bash".to_string()].as_slice())
        );
    }

    #[test]
    fn leaves_allowlist_unset_when_tools_missing() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "general.md",
            r#"---
name: general
description: General helper
---
Handle the assigned task.
"#,
        );

        let registry = load_project_agent_registry(&cwd);
        let agent = registry.agents.get("general").unwrap();

        assert!(agent.tool_policy.allow.is_none());
        assert!(agent.tool_policy.deny.is_none());
    }

    #[test]
    fn parses_custom_agent_with_disallowed_tools() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "safe-worker.md",
            r#"---
name: safe-worker
description: Worker without writes
disallow_tools: "Write, Edit, RunAgent"
---
Inspect and report.
"#,
        );

        let registry = load_project_agent_registry(&cwd);
        let agent = registry.agents.get("safe-worker").unwrap();

        assert!(registry.diagnostics.is_empty());
        assert!(agent.tool_policy.allow.is_none());
        assert_eq!(
            agent.tool_policy.deny.as_deref(),
            Some(
                [
                    "write".to_string(),
                    "edit".to_string(),
                    "run_agent".to_string()
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn parses_custom_agent_model() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "modelled.md",
            r#"---
name: modelled
description: Uses a selected model
model: "openai/gpt-5.4"
---
Handle the task.
"#,
        );

        let registry = load_project_agent_registry(&cwd);
        let agent = registry.agents.get("modelled").unwrap();

        assert!(registry.diagnostics.is_empty());
        assert_eq!(
            agent.model,
            Some(AgentModelSpec {
                provider: "openai".to_string(),
                model: "gpt-5.4".to_string()
            })
        );
    }

    #[test]
    fn duplicate_names_are_skipped_with_diagnostic() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "explorer.md",
            r#"---
name: explorer
description: Conflicts with built-in
---
This should be skipped.
"#,
        );

        let registry = load_project_agent_registry(&cwd);
        let agent = registry.agents.get("explorer").unwrap();

        assert_eq!(
            agent.description,
            "Read-only codebase exploration agent. Use for finding files by pattern, searching definitions/symbols, tracing dependencies, and understanding architecture across multiple files. Specify thoroughness: 'quick' (narrow), 'medium', or 'very thorough' (comprehensive cross-file analysis)."
        );
        assert!(registry.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("duplicate subagent name 'explorer'")
        }));
    }

    #[test]
    fn invalid_model_is_skipped_with_diagnostic() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "bad.md",
            r#"---
name: bad
description: Bad model
model: "missing-separator"
---
This should be skipped.
"#,
        );

        let registry = load_project_agent_registry(&cwd);

        assert!(!registry.agents.contains_key("bad"));
        assert!(
            registry
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message().contains("model must use"))
        );
    }

    #[test]
    fn missing_required_fields_are_skipped_with_diagnostic() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "missing.md",
            r#"---
name: missing-description
---
This should be skipped.
"#,
        );

        let registry = load_project_agent_registry(&cwd);

        assert!(!registry.agents.contains_key("missing-description"));
        assert!(registry.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("missing required frontmatter field 'description'")
        }));
    }

    #[test]
    fn loads_global_and_project_agent_directories() {
        let global = temp_project();
        let project = temp_project();
        write_agent(
            &global,
            "global.md",
            r#"---
name: global-helper
description: Global helper
---
Help across projects.
"#,
        );
        write_agent(
            &project,
            "project.md",
            r#"---
name: project-helper
description: Project helper
---
Help in this project.
"#,
        );

        let registry = load_agent_registry_from_dirs([
            global.join(".omini").join("agents"),
            project.join(".omini").join("agents"),
        ]);

        assert!(registry.diagnostics.is_empty());
        assert!(registry.agents.contains_key("global-helper"));
        assert!(registry.agents.contains_key("project-helper"));
    }

    #[test]
    fn writes_agent_file_that_parser_can_load() {
        let cwd = temp_project();
        let draft = AgentDraft {
            name: "comment-helper".to_string(),
            description: "Help with comments".to_string(),
            short_description: None,
            instructions: "Translate comments carefully.".to_string(),
            tools: vec![
                "read".to_string(),
                "write".to_string(),
                "run_agent".to_string(),
            ],
            disallow_tools: vec!["edit".to_string()],
            model: Some("openai/gpt-test".to_string()),
        };

        let path = write_agent_file(&cwd, AgentSourceKind::Project, &draft).unwrap();
        let parsed = parser::parse_agent_file(&path).unwrap();

        assert_eq!(parsed.name, "comment-helper");
        assert_eq!(parsed.description, "Help with comments");
        assert_eq!(
            parsed.tool_policy.allow.as_deref(),
            Some(
                &[
                    "read".to_string(),
                    "write".to_string(),
                    "run_agent".to_string()
                ][..]
            )
        );
        assert_eq!(
            parsed.tool_policy.deny.as_deref(),
            Some(&["edit".to_string()][..])
        );
        assert_eq!(
            parsed.model,
            Some(AgentModelSpec {
                provider: "openai".to_string(),
                model: "gpt-test".to_string()
            })
        );
    }

    #[test]
    fn agent_name_exists_checks_builtins_and_custom_agents() {
        let cwd = temp_project();
        assert!(agent_name_exists(&cwd, "explorer", None));
        assert!(agent_name_exists(&cwd, "general", None));

        let draft = AgentDraft {
            name: "local-helper".to_string(),
            description: "Local helper".to_string(),
            short_description: None,
            instructions: "Help locally.".to_string(),
            tools: vec!["read".to_string()],
            disallow_tools: Vec::new(),
            model: None,
        };
        let path = write_agent_file(&cwd, AgentSourceKind::Project, &draft).unwrap();

        assert!(agent_name_exists(&cwd, "local-helper", None));
        assert!(!agent_name_exists(&cwd, "local-helper", Some(&path)));
    }

    #[test]
    fn built_in_general_agent_inherits_main_mode_body_without_task_routing() {
        let registry = load_agent_registry_from_dirs(std::iter::empty());
        let general = registry.agents.get("general").unwrap();

        assert_eq!(general.name, "general");
        assert_eq!(
            general.description,
            "General-purpose coding agent for multi-step implementation and research. Use for writing tests, refactoring modules, making code changes, or complex questions requiring multiple tools. Can parallelize independent subtasks. Unlike explorer, this agent can modify files."
        );
        assert_eq!(general.tool_policy.allow, None);
        assert_eq!(general.tool_policy.deny, None);
        // general 的 instructions 就是 agents/general.md 的全文
        let expected = include_str!("agents/general.md");
        assert_eq!(general.instructions, expected.trim());
        // Task Routing 段已删除(避免引导 subagent 再去用 explorer)
        assert!(!general.instructions.contains("## Task Routing"));
        assert!(
            !general
                .instructions
                .contains("`subagent` tool with the `explorer`")
        );
    }

    #[test]
    fn built_in_explorer_agent_instructions_contain_analysis_and_results_blocks() {
        let registry = load_agent_registry_from_dirs(std::iter::empty());
        let explorer = registry.agents.get("explorer").unwrap();

        assert_eq!(explorer.name, "explorer");
        // 引导"先分析、再并发、最后结构化报告"的新修辞
        assert!(explorer.instructions.contains("<analysis>"));
        assert!(explorer.instructions.contains("<results>"));
        assert!(explorer.instructions.contains("<next_steps>"));
        assert!(
            explorer
                .instructions
                .contains("Launch **3+ tools simultaneously**")
        );
        // 工具策略改写为 search/read/bash 三件套
        assert!(explorer.instructions.contains("`search`"));
        assert!(explorer.instructions.contains("`read`"));
        assert!(explorer.instructions.contains("`bash`"));
        // 旧版"Findings/Key references/Uncertainty"格式已替换
        assert!(!explorer.instructions.contains("Findings:"));
        assert!(!explorer.instructions.contains("Key references:"));
    }

    #[test]
    fn summaries_include_location_for_built_in_and_file_agents() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "foo.md",
            r#"---
name: foo
description: Foo helper
---
Help with foo.
"#,
        );

        let registry = load_project_agent_registry(&cwd);
        let summaries = registry.summaries();

        let explorer = summaries.iter().find(|s| s.name == "explorer").unwrap();
        assert_eq!(explorer.location, "<built-in>");

        let general = summaries.iter().find(|s| s.name == "general").unwrap();
        assert_eq!(general.location, "<built-in>");

        let foo = summaries.iter().find(|s| s.name == "foo").unwrap();
        assert_eq!(
            foo.location,
            cwd.join(".omini")
                .join("agents")
                .join("foo.md")
                .display()
                .to_string()
        );
    }
}
