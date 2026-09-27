//! 有序输入片段与命令协议。

use super::*;
use utoipa::ToSchema;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputPart {
    Text {
        text: String,
    },
    Skill {
        name: String,
    },
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Directory {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Subagent {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunCommand {
    Init,
}

impl From<InputPart> for omini_domain::input::InputPart {
    fn from(part: InputPart) -> Self {
        match part {
            InputPart::Text { text } => Self::Text { text },
            InputPart::Skill { name } => Self::Skill { name },
            InputPart::File { path, label } => Self::File { path, label },
            InputPart::Directory { path, label } => Self::Directory { path, label },
            InputPart::Subagent { name, label } => Self::Subagent { name, label },
        }
    }
}

impl From<RunCommand> for omini_domain::input::RunCommand {
    fn from(command: RunCommand) -> Self {
        match command {
            RunCommand::Init => Self::Init,
        }
    }
}
