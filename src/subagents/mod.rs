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
    pub(crate) allowed_tools: Vec<String>,
    source: AgentSource,
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
    let mut registry = AgentRegistry {
        agents: HashMap::new(),
        diagnostics: Vec::new(),
    };

    for spec in builtins::built_in_agents() {
        insert_agent(&mut registry, spec);
    }

    let agents_dir = cwd.join(".omini").join("agents");
    let entries = match fs::read_dir(&agents_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return registry,
        Err(e) => {
            registry.diagnostics.push(AgentLoadDiagnostic {
                message: format!(
                    "failed to read subagent directory {}: {e}",
                    agents_dir.display()
                ),
            });
            return registry;
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
            Ok(spec) => insert_agent(&mut registry, spec),
            Err(e) => registry.diagnostics.push(AgentLoadDiagnostic {
                message: format!("skipped {}: {e}", path.display()),
            }),
        }
    }

    registry
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

        let registry = load_agent_registry(&cwd);
        let agent = registry.agents.get("code-comment-translator").unwrap();

        assert!(registry.diagnostics.is_empty());
        assert_eq!(agent.description, "Translate code comments");
        assert_eq!(agent.allowed_tools, vec!["bash", "read"]);
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

        let registry = load_agent_registry(&cwd);
        let agent = registry.agents.get("reader").unwrap();

        assert!(registry.diagnostics.is_empty());
        assert_eq!(agent.allowed_tools, vec!["read", "bash"]);
    }

    #[test]
    fn inherits_non_recursive_tools_when_tools_missing() {
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

        let registry = load_agent_registry(&cwd);
        let agent = registry.agents.get("general").unwrap();

        assert!(agent.allowed_tools.contains(&"ask_user".to_string()));
        assert!(agent.allowed_tools.contains(&"bash".to_string()));
        assert!(agent.allowed_tools.contains(&"read".to_string()));
        assert!(agent.allowed_tools.contains(&"edit".to_string()));
        assert!(agent.allowed_tools.contains(&"write".to_string()));
        assert!(!agent.allowed_tools.contains(&"subagent".to_string()));
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

        let registry = load_agent_registry(&cwd);
        let agent = registry.agents.get("default").unwrap();

        assert_eq!(agent.description, "General purpose isolated coding agent.");
        assert!(registry.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("duplicate subagent name 'default'")
        }));
    }

    #[test]
    fn invalid_unknown_tool_is_skipped_with_diagnostic() {
        let cwd = temp_project();
        write_agent(
            &cwd,
            "bad.md",
            r#"---
name: bad
description: Bad tools
tools: "Read, Nope"
---
This should be skipped.
"#,
        );

        let registry = load_agent_registry(&cwd);

        assert!(!registry.agents.contains_key("bad"));
        assert!(
            registry
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message().contains("unknown tool 'Nope'"))
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

        let registry = load_agent_registry(&cwd);

        assert!(!registry.agents.contains_key("missing-description"));
        assert!(registry.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("missing required frontmatter field 'description'")
        }));
    }
}
