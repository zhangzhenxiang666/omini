use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(super) struct InstructionFile {
    path: PathBuf,
    content: String,
}

pub(super) fn project_instructions_section(
    global: Option<&InstructionFile>,
    project: Option<&InstructionFile>,
) -> String {
    let mut section = String::new();
    section.push_str("<project_instructions>\n");
    section.push_str("## Priority\n\n");
    section.push_str(
        "- Project `AGENTS.md` instructions override global `~/.omini/AGENTS.md` instructions.\n",
    );
    section.push_str(
        "- Apply the most specific instruction when multiple instruction sources overlap.\n",
    );
    section.push_str("- If project instructions conflict with the user's latest request, explain the conflict before proceeding.\n\n");

    match global {
        Some(file) => {
            section.push_str("## Global Instructions\n\n");
            append_instruction_file(&mut section, file);
            section.push('\n');
        }
        None => {
            section.push_str("## Global Instructions\n\n");
            section.push_str("- No global `~/.omini/AGENTS.md` file was found.\n\n");
        }
    }

    match project {
        Some(file) => {
            section.push_str("## Project Instructions\n\n");
            append_instruction_file(&mut section, file);
        }
        None => {
            section.push_str("## Project Instructions\n\n");
            section.push_str(
                "- No project `AGENTS.md` file was found in the current working directory.\n",
            );
        }
    }

    section.push_str("</project_instructions>");
    section
}

pub(super) fn load_global_instructions() -> Option<InstructionFile> {
    let path = dirs::home_dir()?.join(".omini").join("AGENTS.md");
    load_instruction_file(path)
}

pub(super) fn load_project_instructions(cwd: &Path) -> Option<InstructionFile> {
    load_instruction_file(cwd.join("AGENTS.md"))
}

fn append_instruction_file(section: &mut String, file: &InstructionFile) {
    section.push_str(&format!("Source: `{}`\n\n", file.path.display()));
    section.push_str("```text\n");
    section.push_str(file.content.trim());
    section.push_str("\n```\n");
}

fn load_instruction_file(path: PathBuf) -> Option<InstructionFile> {
    let content = std::fs::read_to_string(&path).ok()?;
    let content = content.trim().to_string();
    if content.is_empty() {
        return None;
    }
    Some(InstructionFile { path, content })
}
