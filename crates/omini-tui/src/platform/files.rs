use crate::features::composer::mention::{
    MentionCandidate, directory_candidate, file_candidate, is_noisy_entry, join_relative,
};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum LocalRequest {
    Directory {
        cwd: PathBuf,
        relative: PathBuf,
        generation: u64,
    },
    Image {
        path: PathBuf,
        start: usize,
        end: usize,
        source: String,
        revision: u64,
    },
}
#[derive(Debug)]
pub enum LocalEvent {
    Directory {
        cwd: PathBuf,
        relative: PathBuf,
        generation: u64,
        candidates: Vec<MentionCandidate>,
    },
    Image {
        path: PathBuf,
        start: usize,
        end: usize,
        source: String,
        revision: u64,
        exists: bool,
    },
}
/// 文件探测在应用的阻塞任务中执行；结果回到事件队列，编辑器不会读取文件系统。
pub fn execute(request: LocalRequest) -> LocalEvent {
    match request {
        LocalRequest::Directory {
            cwd,
            relative,
            generation,
        } => {
            let candidates = std::fs::read_dir(cwd.join(&relative))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().into_string().ok()?;
                    if is_noisy_entry(&name) {
                        return None;
                    }
                    let kind = entry.file_type().ok()?;
                    let target = join_relative(&relative, &name);
                    if kind.is_dir() {
                        Some(directory_candidate(target))
                    } else if kind.is_file() {
                        Some(file_candidate(target))
                    } else {
                        None
                    }
                })
                .collect();
            LocalEvent::Directory {
                cwd,
                relative,
                generation,
                candidates,
            }
        }
        LocalRequest::Image {
            path,
            start,
            end,
            source,
            revision,
        } => {
            let exists = path.is_file();
            LocalEvent::Image {
                path,
                start,
                end,
                source,
                revision,
                exists,
            }
        }
    }
}
