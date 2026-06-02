use super::{Tool, ToolExecutionContext, ToolResult};
use crate::types::events::{
    ToolPauseResponse, UserInputOption, UserInputPreview, UserInputQuestion,
};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskUserInput {
    /// Questions to ask. Prefer one and do not exceed five.
    pub questions: Vec<AskUserQuestionInput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskUserQuestionInput {
    /// Stable identifier for mapping the answer. Use snake_case.
    pub id: String,
    /// Very short label shown in the UI, 12 characters or fewer.
    pub header: String,
    /// A clear, specific, single-sentence question for the user.
    pub question: String,
    /// Mutually exclusive choices to show before the automatic custom answer.
    pub options: Vec<AskUserOption>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AskUserOption {
    /// Concise user-facing label, ideally 1-5 words.
    pub label: String,
    /// One short sentence explaining the impact or tradeoff.
    pub description: String,
}

pub struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    type Input = AskUserInput;
    type Prepared = AskUserInput;

    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        concat!(
            "Ask the user one to five focused questions and wait for the answers.\n",
            "\n",
            "Use this only when continuing safely requires user preference, clarification, ",
            "or a decision that cannot be inferred from the repository or prior messages.\n",
            "\n",
            "Rules:\n",
            "  - Prefer one clear question. If several details are needed, ask at most five.\n",
            "  - The questions array must contain 1-5 items.\n",
            "  - Each question needs a stable snake_case id and an options array with 2-4 ",
            "mutually exclusive items.\n",
            "  - Put the recommended option first and suffix its label with \"(Recommended)\".\n",
            "  - Do not include \"Other\", \"Custom\", or catch-all options. The UI adds a fixed ",
            "\"None of the above\" option automatically; the user can press Tab to enter notes.\n",
            "  - Do not ask for generic permission to proceed.\n",
            "\n",
            "Questions are shown to the user one at a time. The tool result is JSON: ",
            "{\"answers\":{\"<id>\":{\"label\":\"...\",\"note\":\"...\"}}}. ",
            "For predefined choices, note is null unless the user supplied one."
        )
    }

    async fn prepare(&self, input: AskUserInput) -> Result<Self::Prepared, ToolResult> {
        if !(1..=5).contains(&input.questions.len()) {
            return Err(ToolResult::error("questions must contain 1-5 items"));
        }
        for question in &input.questions {
            if question.id.trim().is_empty() {
                return Err(ToolResult::error("question id must not be empty"));
            }
            if !is_snake_case_identifier(&question.id) {
                return Err(ToolResult::error("question id must be snake_case"));
            }
            if question.header.trim().is_empty() {
                return Err(ToolResult::error("question header must not be empty"));
            }
            if question.question.trim().is_empty() {
                return Err(ToolResult::error("question text must not be empty"));
            }
            if !(2..=4).contains(&question.options.len()) {
                return Err(ToolResult::error("each question must contain 2-4 choices"));
            }
            if question.options.iter().any(|o| o.label.trim().is_empty()) {
                return Err(ToolResult::error("option labels must not be empty"));
            }
        }
        Ok(input)
    }

    async fn execute_prepared(
        &self,
        prepared: Self::Prepared,
        ctx: ToolExecutionContext,
    ) -> ToolResult {
        let questions = prepared
            .questions
            .into_iter()
            .map(|prepared_question| UserInputQuestion {
                id: prepared_question.id,
                header: prepared_question.header,
                question: prepared_question.question,
                options: prepared_question
                    .options
                    .into_iter()
                    .map(|o| UserInputOption {
                        label: o.label,
                        description: o.description,
                    })
                    .collect(),
            })
            .collect();

        match ctx.request_user_input(UserInputPreview { questions }).await {
            ToolPauseResponse::UserInput { value } => ToolResult::ok(
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
            ),
            ToolPauseResponse::Cancelled => ToolResult::error("User input request cancelled"),
            ToolPauseResponse::Permission { .. } => {
                ToolResult::error("Received permission response for user input request")
            }
        }
    }
}

fn is_snake_case_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
