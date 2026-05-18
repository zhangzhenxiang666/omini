use crate::subagents::AgentSummary;
use crate::types::display::{DisplayMention, MentionKind};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputMention {
    pub start_char: usize,
    pub end_char: usize,
    pub kind: MentionKind,
    pub label: String,
    pub target: String,
    pub description: String,
}

impl InputMention {
    pub fn display_mention(&self) -> DisplayMention {
        DisplayMention {
            start_char: self.start_char,
            end_char: self.end_char,
            kind: self.kind,
            label: self.label.clone(),
            target: self.target.clone(),
            description: self.description.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionCandidate {
    pub kind: MentionKind,
    pub label: String,
    pub target: String,
    pub description: String,
}

impl MentionCandidate {
    pub fn insert_display(&self) -> String {
        format!("@{}", self.label)
    }

    pub fn drawer_display(&self) -> String {
        match self.kind {
            MentionKind::Subagent => self.insert_display(),
            MentionKind::Directory | MentionKind::File => self.label.clone(),
            MentionKind::Command => format!("/{}", self.label),
        }
    }

    fn sort_key(&self) -> (u8, String) {
        let kind = match self.kind {
            MentionKind::Subagent => 0,
            MentionKind::Directory => 1,
            MentionKind::File => 2,
            MentionKind::Command => 3,
        };
        (kind, self.label.to_ascii_lowercase())
    }

    pub fn is_directory(&self) -> bool {
        self.kind == MentionKind::Directory
    }
}

#[derive(Debug, Clone, Default)]
pub struct MentionAutocomplete {
    pub visible: bool,
    pub all_candidates: Vec<MentionCandidate>,
    pub filtered: Vec<MentionCandidate>,
    pub selected: usize,
    pub active_start: usize,
    pub active_end: usize,
    pub query: String,
    cwd: PathBuf,
    dir_cache: HashMap<PathBuf, Vec<MentionCandidate>>,
}

impl MentionAutocomplete {
    pub fn set_candidates(&mut self, candidates: Vec<MentionCandidate>) {
        self.all_candidates = candidates;
    }

    pub fn set_cwd(&mut self, cwd: impl Into<PathBuf>) {
        self.cwd = cwd.into();
        self.clear_session_cache();
    }

    pub fn clear_session_cache(&mut self) {
        self.dir_cache.clear();
    }

    pub fn update(&mut self, input: &str, cursor_char: usize) {
        let Some(active) = active_mention(input, cursor_char) else {
            if self.visible {
                self.clear_session_cache();
            }
            self.visible = false;
            self.filtered.clear();
            self.query.clear();
            return;
        };

        if !self.visible {
            self.clear_session_cache();
        }
        let previous_query = self.query.clone();
        let previous_start = self.active_start;
        self.visible = true;
        self.active_start = active.start_char;
        self.active_end = active.end_char;
        self.query = active.query.clone();

        let Some(query) = MentionQuery::parse(&active.query) else {
            self.filtered.clear();
            self.selected = 0;
            return;
        };

        self.filtered = self.candidates_for_query(&query);

        let max = self.filtered.len().saturating_sub(1);
        if previous_start != self.active_start || previous_query != self.query {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(max);
        }
    }

    pub fn selected_candidate(&self) -> Option<&MentionCandidate> {
        if self.filtered.is_empty() {
            return None;
        }
        self.filtered.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.filtered.len();
    }

    pub fn select_prev(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
    }

    fn candidates_for_query(&mut self, query: &MentionQuery) -> Vec<MentionCandidate> {
        let mut candidates = Vec::new();

        if !query.path_mode {
            candidates.extend(
                self.all_candidates
                    .iter()
                    .filter(|candidate| candidate.kind == MentionKind::Subagent)
                    .cloned(),
            );
        }

        let parent = query.parent_path.as_deref().unwrap_or("");
        if let Some(mut fs_candidates) = self.load_dir_candidates(parent) {
            if query.path_mode && query.search_term.is_empty() && !parent.is_empty() {
                fs_candidates.insert(0, directory_self_candidate(parent));
            }
            candidates.extend(fs_candidates);
        }

        let search = query.search_term.to_ascii_lowercase();
        let mut scored: Vec<_> = candidates
            .into_iter()
            .filter_map(|candidate| {
                mention_match_score(&candidate, &search, query.path_mode)
                    .map(|score| (score, candidate))
            })
            .collect();
        scored.sort_by(|(left_score, left), (right_score, right)| {
            left_score
                .cmp(right_score)
                .then_with(|| left.sort_key().cmp(&right.sort_key()))
                .then_with(|| {
                    left.target
                        .chars()
                        .count()
                        .cmp(&right.target.chars().count())
                })
        });
        scored.into_iter().map(|(_, candidate)| candidate).collect()
    }

    fn load_dir_candidates(&mut self, relative_dir: &str) -> Option<Vec<MentionCandidate>> {
        let dir = safe_relative_path(relative_dir)?;
        if let Some(candidates) = self.dir_cache.get(&dir) {
            return Some(candidates.clone());
        }

        let absolute_dir = self.cwd.join(&dir);
        let entries = fs::read_dir(&absolute_dir).ok()?;
        let mut candidates = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if is_noisy_entry(file_name) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let target = join_relative(&dir, file_name);
            if file_type.is_dir() {
                candidates.push(directory_candidate(target));
            } else if file_type.is_file() {
                candidates.push(file_candidate(target));
            }
        }
        candidates.sort_by_key(MentionCandidate::sort_key);
        self.dir_cache.insert(dir, candidates.clone());
        Some(candidates)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveMention {
    start_char: usize,
    end_char: usize,
    query: String,
}

fn active_mention(input: &str, cursor_char: usize) -> Option<ActiveMention> {
    let chars: Vec<char> = input.chars().collect();
    let cursor_char = cursor_char.min(chars.len());
    let mut start = cursor_char;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }

    if chars.get(start).copied()? != '@' {
        return None;
    }
    if start > 0 && !chars[start - 1].is_whitespace() {
        return None;
    }
    if cursor_char < start + 1 {
        return None;
    }

    Some(ActiveMention {
        start_char: start,
        end_char: cursor_char,
        query: chars[start + 1..cursor_char].iter().collect(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MentionQuery {
    parent_path: Option<String>,
    search_term: String,
    path_mode: bool,
}

impl MentionQuery {
    fn parse(query: &str) -> Option<Self> {
        if query.starts_with('/') || query.contains("..") {
            return None;
        }
        let normalized = query.replace('\\', "/");
        let path_mode = normalized.contains('/');
        let (parent_path, search_term) = match normalized.rsplit_once('/') {
            Some((parent, term)) => {
                let parent = (!parent.is_empty()).then(|| parent.to_string());
                (parent, term.to_string())
            }
            None => (None, normalized),
        };
        Some(Self {
            parent_path,
            search_term,
            path_mode,
        })
    }
}

#[cfg(test)]
fn mention_matches(candidate: &MentionCandidate, query: &str) -> bool {
    mention_match_score(candidate, query, false).is_some()
}

fn mention_match_score(
    candidate: &MentionCandidate,
    query: &str,
    match_basename_only: bool,
) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let label = if match_basename_only {
        candidate
            .target
            .rsplit('/')
            .next()
            .unwrap_or(&candidate.target)
            .to_ascii_lowercase()
    } else {
        candidate.label.to_ascii_lowercase()
    };
    let target = candidate.target.to_ascii_lowercase();
    let description = candidate.description.to_ascii_lowercase();
    if label.starts_with(query) || (!match_basename_only && target.starts_with(query)) {
        Some(0)
    } else if label.contains(query)
        || (!match_basename_only && target.contains(query))
        || (!match_basename_only && description.contains(query))
    {
        Some(1)
    } else if fuzzy_subsequence(&label, query)
        || (!match_basename_only && fuzzy_subsequence(&target, query))
    {
        Some(2)
    } else {
        None
    }
}

fn fuzzy_subsequence(value: &str, query: &str) -> bool {
    let mut chars = value.chars();
    query
        .chars()
        .all(|needle| chars.by_ref().any(|ch| ch == needle))
}

pub fn agent_summaries_to_mention_candidates(agents: Vec<AgentSummary>) -> Vec<MentionCandidate> {
    let mut candidates = Vec::new();
    for agent in agents {
        candidates.push(MentionCandidate {
            kind: MentionKind::Subagent,
            label: agent.name.clone(),
            target: agent.name,
            description: agent.description,
        });
    }

    candidates.sort_by_key(MentionCandidate::sort_key);
    candidates
}

fn safe_relative_path(relative: &str) -> Option<PathBuf> {
    if relative.starts_with('/') || relative.contains("..") {
        return None;
    }
    let mut path = PathBuf::new();
    for part in relative.split('/').filter(|part| !part.is_empty()) {
        path.push(part);
    }
    Some(path)
}

fn join_relative(parent: &Path, file_name: &str) -> String {
    if parent.as_os_str().is_empty() {
        file_name.to_string()
    } else {
        parent.join(file_name).to_string_lossy().replace('\\', "/")
    }
}

fn is_noisy_entry(file_name: &str) -> bool {
    matches!(
        file_name,
        ".git" | "target" | "node_modules" | ".cache" | ".idea" | ".vscode"
    )
}

fn directory_candidate(relative: String) -> MentionCandidate {
    MentionCandidate {
        kind: MentionKind::Directory,
        label: relative.clone(),
        target: relative.clone(),
        description: "directory".to_string(),
    }
}

fn directory_self_candidate(relative: &str) -> MentionCandidate {
    MentionCandidate {
        kind: MentionKind::Directory,
        label: relative.to_string(),
        target: relative.to_string(),
        description: "current directory".to_string(),
    }
}

fn file_candidate(relative: String) -> MentionCandidate {
    let description = if is_supported_image_file_name(&relative) {
        "image"
    } else {
        "file"
    };
    MentionCandidate {
        kind: MentionKind::File,
        label: relative.clone(),
        target: relative.clone(),
        description: description.to_string(),
    }
}

fn is_supported_image_file_name(file_name: &str) -> bool {
    Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn active_mention_triggers_at_start() {
        assert_eq!(
            active_mention("@exp", 4),
            Some(ActiveMention {
                start_char: 0,
                end_char: 4,
                query: "exp".to_string(),
            })
        );
    }

    #[test]
    fn active_mention_triggers_after_space_only() {
        assert!(active_mention("hello @src", 10).is_some());
        assert!(active_mention("hello@src", 9).is_none());
    }

    #[test]
    fn active_mention_ends_at_whitespace() {
        assert!(active_mention("hello @src more", 15).is_none());
        assert!(active_mention("hello @src more", 10).is_some());
    }

    #[test]
    fn fuzzy_filter_matches_subsequence() {
        let candidate = MentionCandidate {
            kind: MentionKind::File,
            label: "src/main.rs".to_string(),
            target: "src/main.rs".to_string(),
            description: "file".to_string(),
        };
        assert!(mention_matches(&candidate, "smr"));
        assert!(!mention_matches(&candidate, "zzz"));
    }

    #[test]
    fn mention_query_parses_path_shapes() {
        assert_eq!(
            MentionQuery::parse("").unwrap(),
            MentionQuery {
                parent_path: None,
                search_term: String::new(),
                path_mode: false,
            }
        );
        assert_eq!(
            MentionQuery::parse("src").unwrap(),
            MentionQuery {
                parent_path: None,
                search_term: "src".to_string(),
                path_mode: false,
            }
        );
        assert_eq!(
            MentionQuery::parse("src/").unwrap(),
            MentionQuery {
                parent_path: Some("src".to_string()),
                search_term: String::new(),
                path_mode: true,
            }
        );
        assert_eq!(
            MentionQuery::parse("src/main").unwrap(),
            MentionQuery {
                parent_path: Some("src".to_string()),
                search_term: "main".to_string(),
                path_mode: true,
            }
        );
        assert!(MentionQuery::parse("../src").is_none());
        assert!(MentionQuery::parse("/tmp").is_none());
    }

    #[test]
    fn path_mode_loads_one_directory_level() {
        let root = temp_dir();
        fs::create_dir_all(root.join("src").join("nested")).unwrap();
        fs::write(root.join("src").join("main.rs"), "").unwrap();
        fs::write(root.join("src").join("nested").join("deep.rs"), "").unwrap();

        let mut autocomplete = MentionAutocomplete::default();
        autocomplete.set_cwd(root.clone());
        autocomplete.update("@src/", 5);

        let targets: Vec<_> = autocomplete
            .filtered
            .iter()
            .map(|candidate| candidate.target.as_str())
            .collect();
        assert!(targets.contains(&"src"));
        assert!(targets.contains(&"src/main.rs"));
        assert!(targets.contains(&"src/nested"));
        assert!(!targets.contains(&"src/nested/deep.rs"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn path_mode_matches_current_level_basename() {
        let root = temp_dir();
        fs::create_dir_all(root.join("src").join("types")).unwrap();
        fs::write(root.join("src").join("main.rs"), "").unwrap();

        let mut autocomplete = MentionAutocomplete::default();
        autocomplete.set_cwd(root.clone());
        autocomplete.update("@src/t", 6);

        let targets: Vec<_> = autocomplete
            .filtered
            .iter()
            .map(|candidate| candidate.target.as_str())
            .collect();
        assert_eq!(targets, vec!["src/types"]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn image_files_are_described_as_images() {
        let root = temp_dir();
        fs::write(root.join("photo.png"), "").unwrap();
        fs::write(root.join("notes.txt"), "").unwrap();

        let mut autocomplete = MentionAutocomplete::default();
        autocomplete.set_cwd(root.clone());
        autocomplete.update("@", 1);

        let image = autocomplete
            .filtered
            .iter()
            .find(|candidate| candidate.target == "photo.png")
            .unwrap();
        let text = autocomplete
            .filtered
            .iter()
            .find(|candidate| candidate.target == "notes.txt")
            .unwrap();
        assert_eq!(image.description, "image");
        assert_eq!(text.description, "file");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cache_refreshes_after_new_mention_session() {
        let root = temp_dir();
        fs::write(root.join("a.rs"), "").unwrap();

        let mut autocomplete = MentionAutocomplete::default();
        autocomplete.set_cwd(root.clone());
        autocomplete.update("@", 1);
        fs::write(root.join("b.rs"), "").unwrap();
        autocomplete.update("@", 1);
        assert!(
            !autocomplete
                .filtered
                .iter()
                .any(|candidate| candidate.target == "b.rs")
        );

        autocomplete.update("", 0);
        autocomplete.update("@", 1);
        assert!(
            autocomplete
                .filtered
                .iter()
                .any(|candidate| candidate.target == "b.rs")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn drawer_display_prefixes_only_subagents() {
        let subagent = MentionCandidate {
            kind: MentionKind::Subagent,
            label: "worker".to_string(),
            target: "worker".to_string(),
            description: "agent".to_string(),
        };
        let directory = MentionCandidate {
            kind: MentionKind::Directory,
            label: "src".to_string(),
            target: "src".to_string(),
            description: "directory".to_string(),
        };
        let file = MentionCandidate {
            kind: MentionKind::File,
            label: "Cargo.toml".to_string(),
            target: "Cargo.toml".to_string(),
            description: "file".to_string(),
        };

        assert_eq!(subagent.drawer_display(), "@worker");
        assert_eq!(directory.drawer_display(), "src");
        assert_eq!(file.drawer_display(), "Cargo.toml");
        assert_eq!(directory.insert_display(), "@src");
        assert_eq!(file.insert_display(), "@Cargo.toml");
    }

    fn temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("omini-mention-test-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
