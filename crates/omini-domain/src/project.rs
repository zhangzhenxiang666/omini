use std::path::Path;

/// Converts a project path into the stable project id used in `~/.omini`.
pub fn sanitize_project_path(path: &Path) -> String {
    path.to_string_lossy().replace(['/', '_', ' '], "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_project_path() {
        assert_eq!(
            sanitize_project_path(Path::new("/home/user/my_project")),
            "-home-user-my-project"
        );
    }
}
