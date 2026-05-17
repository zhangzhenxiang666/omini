use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

mod builtins;
mod parser;
mod runner;

pub use runner::{RuntimeSubagentRunner, SubagentRunRequest};

#[derive(Debug, Clone)]
pub(crate) struct AgentSpec {
    pub(crate) name: String,
    pub(crate) description: String,
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
pub struct AgentSummary {
    pub name: String,
    pub description: String,
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
    fn parses_custom_agent_with_string_tools() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "translator.md",
            r#"---
name: code-comment-translator
description: "Translate code comments"
tools: "Bash, Read, Subagent"
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
            Some(["bash".to_string(), "read".to_string()].as_slice())
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
disallow_tools: "Write, Edit, Subagent"
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
                    "subagent".to_string()
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
            "default.md",
            r#"---
name: default
description: Conflicts with built-in
---
This should be skipped.
"#,
        );

        let registry = load_project_agent_registry(&cwd);
        let agent = registry.agents.get("default").unwrap();

        assert_eq!(agent.description, "General purpose isolated coding agent.");
        assert!(registry.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("duplicate subagent name 'default'")
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
}
