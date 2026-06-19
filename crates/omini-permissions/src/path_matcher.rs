use std::path::{Path, PathBuf};

/// 路径通配符匹配器，支持 `*` 和 `/**/` 两种通配语法。
#[derive(Debug, Clone)]
pub(crate) struct PathMatcher {
    pattern: String,
}

impl PathMatcher {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            pattern: normalize_path_string(&path),
        }
    }

    pub(crate) fn matches(&self, path: &Path) -> bool {
        let text = normalize_path_string(path);
        wildcard_match(&self.pattern, &text)
            || self
                .pattern
                .contains("/**/")
                .then(|| self.pattern.replace("/**/", "/"))
                .is_some_and(|pattern| wildcard_match(&pattern, &text))
    }
}

/// 将路径标准化为 `/` 分隔的字符串，兼容 Windows 反斜杠。
pub(crate) fn normalize_path_string(path: &Path) -> String {
    path.components()
        .as_path()
        .to_string_lossy()
        .replace('\\', "/")
}

/// 通配符匹配：`*` 匹配任意字符序列，其余字符精确匹配。
pub(crate) fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star = None;
    let mut match_i = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            match_i = ti;
            pi += 1;
        } else if let Some(star_i) = star {
            pi = star_i + 1;
            match_i += 1;
            ti = match_i;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// 从 `PermissionPreview` 或原始 JSON 输入中提取通用路径（read/edit/write/search 共用）。
pub(crate) fn permission_path(
    preview: Option<&omini_domain::events::PermissionPreview>,
    raw_input: &serde_json::Value,
) -> Option<PathBuf> {
    use omini_domain::events::PermissionPreview;
    match preview {
        Some(PermissionPreview::Read(preview)) => Some(PathBuf::from(&preview.file_path)),
        Some(PermissionPreview::Search(preview)) => Some(PathBuf::from(&preview.path)),
        Some(PermissionPreview::Edit(preview)) | Some(PermissionPreview::Write(preview)) => {
            Some(PathBuf::from(&preview.path))
        }
        _ => raw_input
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from),
    }
}

/// 从 search 专用 preview 或原始 JSON 输入中提取搜索路径。
pub(crate) fn search_path(
    preview: Option<&omini_domain::events::PermissionPreview>,
    raw_input: &serde_json::Value,
) -> Option<PathBuf> {
    use omini_domain::events::PermissionPreview;
    match preview {
        Some(PermissionPreview::Search(preview)) => Some(PathBuf::from(&preview.path)),
        _ => raw_input
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from),
    }
}

/// 从 read 专用 preview 或原始 JSON 输入中提取读取路径。
pub(crate) fn read_path(
    preview: Option<&omini_domain::events::PermissionPreview>,
    raw_input: &serde_json::Value,
) -> Option<PathBuf> {
    use omini_domain::events::PermissionPreview;
    match preview {
        Some(PermissionPreview::Read(preview)) => Some(PathBuf::from(&preview.file_path)),
        _ => raw_input
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from),
    }
}

/// 判断路径是否指向敏感文件（`.env`、私钥、SSH 配置、含 token/secret 的文件名等）。
pub(crate) fn is_private_path(path: &Path) -> bool {
    let normalized = normalize_path_string(path).to_ascii_lowercase();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name == "id_rsa"
        || name == "id_ed25519"
        || normalized.contains("/.ssh/")
        || (name.contains("token") || name.contains("secret") || name.contains("credential"))
}
