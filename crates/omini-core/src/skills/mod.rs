use crate::frontmatter;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SkillSpec {
    pub name: String,
    pub description: String,
    pub short_description: Option<String>,
    pub argument_hint: Option<String>,
    pub body: String,
    pub directory: PathBuf,
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
    source: SkillSource,
}

impl SkillSpec {
    pub fn source_kind(&self) -> SkillSourceKind {
        self.source.kind()
    }
}

#[derive(Debug, Clone)]
enum SkillSource {
    BuiltIn(PathBuf),
    Project(PathBuf),
    User(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSourceKind {
    BuiltIn,
    Project,
    User,
}

impl SkillSource {
    fn kind(&self) -> SkillSourceKind {
        match self {
            SkillSource::BuiltIn(_) => SkillSourceKind::BuiltIn,
            SkillSource::Project(_) => SkillSourceKind::Project,
            SkillSource::User(_) => SkillSourceKind::User,
        }
    }

    fn display(&self) -> String {
        match self {
            SkillSource::BuiltIn(path) | SkillSource::Project(path) | SkillSource::User(path) => {
                path.display().to_string()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillSummary {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) short_description: Option<String>,
    pub(crate) directory: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SkillLoadDiagnostic {
    message: String,
}

impl SkillLoadDiagnostic {
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone)]
pub struct SkillRegistry {
    pub(crate) skills: HashMap<String, SkillSpec>,
    pub diagnostics: Vec<SkillLoadDiagnostic>,
}

impl SkillRegistry {
    pub(crate) fn injected_summaries(&self) -> Vec<SkillSummary> {
        let mut summaries = self
            .skills
            .values()
            .filter(|skill| !skill.disable_model_invocation)
            .map(|skill| SkillSummary {
                name: skill.name.clone(),
                description: skill.description.clone(),
                short_description: skill.short_description.clone(),
                directory: skill.directory.clone(),
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|a, b| a.name.cmp(&b.name));
        summaries
    }

    pub fn get(&self, name: &str) -> Option<&SkillSpec> {
        self.skills.get(name)
    }

    pub fn skills(&self) -> impl Iterator<Item = &SkillSpec> {
        self.skills.values()
    }

    pub(crate) fn sorted_names(&self) -> Vec<String> {
        let mut names = self.skills.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }
}

pub fn load_skill_registry(cwd: &Path) -> SkillRegistry {
    let mut skill_dirs = Vec::new();
    if let Some(home_dir) = dirs::home_dir().filter(|path| !path.as_os_str().is_empty()) {
        skill_dirs.push((
            SkillDirectoryKind::User,
            home_dir.join(".omini").join("skills"),
        ));
    }
    skill_dirs.push((
        SkillDirectoryKind::Project,
        cwd.join(".omini").join("skills"),
    ));
    load_skill_registry_from_dirs(skill_dirs)
}

pub(crate) fn load_skill_summaries(cwd: &Path) -> Vec<SkillSummary> {
    load_skill_registry(cwd).injected_summaries()
}

pub(crate) fn render_skill_invocation(spec: &SkillSpec, prompt: Option<&str>) -> String {
    render_skill_invocation_inner(spec, prompt, None)
}

fn render_skill_invocation_inner(
    spec: &SkillSpec,
    prompt: Option<&str>,
    source: Option<&str>,
) -> String {
    let mut output = String::new();
    output.push_str("<skill>\n");
    output.push_str("<skill_name>");
    output.push_str(&spec.name);
    output.push_str("</skill_name>\n");
    if let Some(source) = source {
        output.push_str("<skill_invocation>\n");
        output.push_str("<source>");
        output.push_str(source);
        output.push_str("</source>\n");
        output.push_str("</skill_invocation>\n");
    }
    output.push_str("<skill_directory>");
    output.push_str(&spec.directory.display().to_string());
    output.push_str("</skill_directory>\n");
    output.push_str("<skill_body>\n");
    output.push_str(spec.body.trim());
    output.push_str("\n</skill_body>");
    if let Some(prompt) = prompt.map(str::trim).filter(|prompt| !prompt.is_empty()) {
        output.push_str("\n<user_prompt>\n");
        output.push_str(prompt);
        output.push_str("\n</user_prompt>");
    }
    output.push_str("\n</skill>");
    output
}

#[derive(Debug, Clone, Copy)]
enum SkillDirectoryKind {
    Project,
    User,
}

fn load_skill_registry_from_dirs(
    skill_dirs: impl IntoIterator<Item = (SkillDirectoryKind, PathBuf)>,
) -> SkillRegistry {
    let mut registry = SkillRegistry {
        skills: HashMap::new(),
        diagnostics: Vec::new(),
    };

    load_built_in_skills(&mut registry);

    for (source_kind, skills_dir) in skill_dirs {
        load_skills_from_dir(&mut registry, source_kind, &skills_dir);
    }

    registry
}

fn load_skills_from_dir(
    registry: &mut SkillRegistry,
    source_kind: SkillDirectoryKind,
    skills_dir: &Path,
) {
    let entries = match fs::read_dir(skills_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            registry.diagnostics.push(SkillLoadDiagnostic {
                message: format!(
                    "failed to read skill directory {}: {e}",
                    skills_dir.display()
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
                if path.is_dir() && path.join("SKILL.md").is_file() {
                    paths.push(path);
                }
            }
            Err(e) => registry.diagnostics.push(SkillLoadDiagnostic {
                message: format!("failed to read skill directory entry: {e}"),
            }),
        }
    }
    paths.sort();

    for directory in paths {
        match parse_skill_directory(&directory, source_kind) {
            Ok(spec) => insert_skill(registry, spec),
            Err(e) => registry.diagnostics.push(SkillLoadDiagnostic {
                message: format!("skipped {}: {e}", directory.join("SKILL.md").display()),
            }),
        }
    }
}

fn parse_skill_directory(
    directory: &Path,
    source_kind: SkillDirectoryKind,
) -> Result<SkillSpec, String> {
    let path = directory.join("SKILL.md");
    let content = fs::read_to_string(&path).map_err(|e| format!("failed to read file: {e}"))?;
    let directory = absolute_existing_path(directory);
    let source = match source_kind {
        SkillDirectoryKind::Project => SkillSource::Project(directory.clone()),
        SkillDirectoryKind::User => SkillSource::User(directory.clone()),
    };

    parse_skill_content(&content, directory, source)
}

fn parse_skill_content(
    content: &str,
    directory: PathBuf,
    source: SkillSource,
) -> Result<SkillSpec, String> {
    let (raw, body) = frontmatter::parse(content)?;

    let name = frontmatter::required_string(&raw, "name")?;
    let description = frontmatter::required_string(&raw, "description")?;
    let short_description = frontmatter::optional_string(&raw, "short-description")?;
    let argument_hint = frontmatter::optional_string(&raw, "argument-hint")?;
    let disable_model_invocation =
        frontmatter::optional_bool_path(&raw, &["disable-model-invocation"])?.unwrap_or(false);
    let user_invocable =
        frontmatter::optional_bool_path(&raw, &["user-invocable"])?.unwrap_or(true);
    let body = body.trim().to_string();
    if body.is_empty() {
        return Err("skill body must not be empty".to_string());
    }

    Ok(SkillSpec {
        name,
        description,
        short_description,
        argument_hint,
        body,
        directory,
        disable_model_invocation,
        user_invocable,
        source,
    })
}

fn load_built_in_skills(registry: &mut SkillRegistry) {
    for (name, content) in [("skill-creator", include_str!("builtins/skill-creator.md"))] {
        let directory = PathBuf::from("<built-in>").join(name);
        match parse_skill_content(content, directory.clone(), SkillSource::BuiltIn(directory)) {
            Ok(spec) => insert_skill(registry, spec),
            Err(e) => registry.diagnostics.push(SkillLoadDiagnostic {
                message: format!("skipped built-in skill {name}: {e}"),
            }),
        }
    }
}

fn absolute_existing_path(path: &Path) -> PathBuf {
    if let Ok(path) = path.canonicalize() {
        return path;
    }
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn insert_skill(registry: &mut SkillRegistry, spec: SkillSpec) {
    if let Some(existing) = registry.skills.get(&spec.name) {
        registry.diagnostics.push(SkillLoadDiagnostic {
            message: format!(
                "skill '{}' from {} overrides {}",
                spec.name,
                spec.source.display(),
                existing.source.display()
            ),
        });
    }
    registry.skills.insert(spec.name.clone(), spec);
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_root() -> PathBuf {
        let path = std::env::temp_dir().join(format!("omini_skill_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_skill(root: &Path, name: &str, content: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), content).unwrap();
        dir
    }

    fn load_project_skill_registry(root: &Path) -> SkillRegistry {
        load_skill_registry_from_dirs([(SkillDirectoryKind::Project, root.to_path_buf())])
    }

    #[test]
    fn loads_built_in_skills() {
        let registry = load_skill_registry_from_dirs(std::iter::empty());

        let creator = registry.get("skill-creator").unwrap();
        let commit_message = registry.get("commit-message").unwrap();

        assert!(
            creator
                .directory
                .display()
                .to_string()
                .contains("<built-in>")
        );
        assert!(creator.body.contains("Project skill: `.omini/skills"));
        assert!(commit_message.body.contains("git log --oneline -n 10"));
        assert!(
            registry
                .injected_summaries()
                .iter()
                .any(|summary| summary.name == "skill-creator")
        );
    }

    #[test]
    fn built_in_invocation_includes_virtual_directory() {
        let registry = load_skill_registry_from_dirs(std::iter::empty());
        let spec = registry.get("commit-message").unwrap();

        let output = render_skill_invocation(spec, Some("Draft commits."));

        assert!(output.contains("<skill_name>commit-message</skill_name>"));
        assert!(output.contains("<skill_directory><built-in>/commit-message</skill_directory>"));
        assert!(output.contains("<user_prompt>\nDraft commits.\n</user_prompt>"));
        assert!(!output.contains("<skill_invocation>"));
    }

    #[test]
    fn loads_directory_skills() {
        let root = temp_root();
        write_skill(
            &root,
            "writer",
            r#"---
name: writer
description: Write carefully
---
Use the writing workflow.
"#,
        );

        let registry = load_project_skill_registry(&root);
        let skill = registry.get("writer").unwrap();

        assert!(registry.diagnostics.is_empty());
        assert_eq!(skill.description, "Write carefully");
        assert!(skill.directory.is_absolute());
        assert_eq!(skill.body, "Use the writing workflow.");
    }

    #[test]
    fn inject_defaults_to_true() {
        let root = temp_root();
        write_skill(
            &root,
            "default-inject",
            r#"---
name: default-inject
description: Inject by default
---
Body
"#,
        );

        let registry = load_project_skill_registry(&root);

        assert!(
            registry
                .injected_summaries()
                .iter()
                .any(|summary| summary.name == "default-inject")
        );
    }

    #[test]
    fn disable_model_invocation_true_suppresses_prompt_summary() {
        let root = temp_root();
        write_skill(
            &root,
            "hidden",
            r#"---
name: hidden
description: Hidden skill
disable-model-invocation: true
---
Body
"#,
        );

        let registry = load_project_skill_registry(&root);

        assert!(registry.get("hidden").is_some());
        assert!(
            !registry
                .injected_summaries()
                .iter()
                .any(|summary| summary.name == "hidden")
        );
    }

    #[test]
    fn metadata_disable_model_invocation_is_ignored() {
        let root = temp_root();
        write_skill(
            &root,
            "legacy-hidden",
            r#"---
name: legacy-hidden
description: Legacy hidden skill
metadata:
  disable-model-invocation: true
---
Body
"#,
        );

        let registry = load_project_skill_registry(&root);

        assert!(registry.get("legacy-hidden").is_some());
        assert!(
            registry
                .injected_summaries()
                .iter()
                .any(|summary| summary.name == "legacy-hidden")
        );
    }

    #[test]
    fn user_invocable_defaults_to_true() {
        let root = temp_root();
        write_skill(
            &root,
            "default-invocable",
            r#"---
name: default-invocable
description: Invocable by default
---
Body
"#,
        );

        let registry = load_project_skill_registry(&root);

        assert!(registry.get("default-invocable").unwrap().user_invocable);
    }

    #[test]
    fn user_invocable_false_is_parsed() {
        let root = temp_root();
        write_skill(
            &root,
            "background",
            r#"---
name: background
description: Background knowledge
user-invocable: false
---
Body
"#,
        );

        let registry = load_project_skill_registry(&root);

        assert!(!registry.get("background").unwrap().user_invocable);
    }

    #[test]
    fn later_skill_sources_override_earlier_sources() {
        let user = temp_root();
        let project = temp_root();
        write_skill(
            &user,
            "helper",
            r#"---
name: helper
description: User helper
---
User body
"#,
        );
        write_skill(
            &project,
            "helper",
            r#"---
name: helper
description: Project helper
---
Project body
"#,
        );

        let registry = load_skill_registry_from_dirs([
            (SkillDirectoryKind::User, user),
            (SkillDirectoryKind::Project, project),
        ]);

        assert_eq!(
            registry.get("helper").unwrap().description,
            "Project helper"
        );
        assert!(registry.diagnostics.iter().any(|diagnostic| {
            diagnostic.message().contains("skill 'helper' from")
                && diagnostic.message().contains("overrides")
        }));
    }

    #[test]
    fn project_skill_can_override_built_in_skill() {
        let project = temp_root();
        write_skill(
            &project,
            "skill-creator",
            r#"---
name: skill-creator
description: Project-specific skill creator
---
Use the local skill workflow.
"#,
        );

        let registry = load_project_skill_registry(&project);
        let skill = registry.get("skill-creator").unwrap();

        assert_eq!(skill.description, "Project-specific skill creator");
        assert_eq!(skill.body, "Use the local skill workflow.");
        assert!(registry.diagnostics.iter().any(|diagnostic| {
            diagnostic.message().contains("skill 'skill-creator' from")
                && diagnostic.message().contains("<built-in>/skill-creator")
        }));
    }

    #[test]
    fn malformed_skill_is_skipped_with_diagnostic() {
        let root = temp_root();
        write_skill(
            &root,
            "bad",
            r#"---
name: bad
---
Body
"#,
        );

        let registry = load_project_skill_registry(&root);

        assert!(registry.get("bad").is_none());
        assert!(registry.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("missing required frontmatter field 'description'")
        }));
    }

    #[test]
    fn invocation_includes_body_directory_and_prompt() {
        let root = temp_root();
        let dir = write_skill(
            &root,
            "writer",
            r#"---
name: writer
description: Write carefully
---
Use the writing workflow.
"#,
        );
        let spec = parse_skill_directory(&dir, SkillDirectoryKind::Project).unwrap();

        let output = render_skill_invocation(&spec, Some("Draft this."));

        assert!(output.contains("<skill_name>writer</skill_name>"));
        assert!(output.contains("<skill_directory>"));
        assert!(output.contains(dir.canonicalize().unwrap().to_str().unwrap()));
        assert!(output.contains("<skill_body>\nUse the writing workflow.\n</skill_body>"));
        assert!(output.contains("<user_prompt>\nDraft this.\n</user_prompt>"));
    }
}
