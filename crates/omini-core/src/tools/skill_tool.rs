use super::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SkillInput {
    /// Skill name to load.
    pub name: String,
}

#[derive(Debug)]
pub struct SkillRequest {
    name: String,
}

pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    type Input = SkillInput;
    type Prepared = SkillRequest;

    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        concat!(
            "Load a skill's full instructions on demand.\n",
            "\n",
            "Input fields:\n",
            "  name    The skill name to load.\n",
            "\n",
            "Skills are progressive-disclosure instruction bundles. The system prompt may list ",
            "only selected skill descriptions; this tool returns the full SKILL.md body and the ",
            "absolute skill directory for bundled resources."
        )
    }

    async fn prepare(&self, input: SkillInput) -> Result<Self::Prepared, ToolResult> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err(ToolResult::error("name must not be empty"));
        }
        Ok(SkillRequest {
            name: name.to_string(),
        })
    }

    async fn execute_prepared(
        &self,
        request: Self::Prepared,
        ctx: ToolExecutionContext,
    ) -> ToolResult {
        let Some(runtime) = ctx.runtime.clone() else {
            return ToolResult::error("skill requires runtime context");
        };
        let Some(spec) = runtime.skill_registry.get(&request.name).cloned() else {
            let available = runtime.skill_registry.sorted_names();
            let mut msg = format!(
                "unknown skill '{}'. Available skills: {}",
                request.name,
                available.join(", ")
            );
            if !runtime.skill_registry.diagnostics.is_empty() {
                msg.push_str("\n\nSkill load warnings:");
                for diagnostic in &runtime.skill_registry.diagnostics {
                    msg.push_str("\n- ");
                    msg.push_str(diagnostic.message());
                }
            }
            return ToolResult::error(msg);
        };

        ToolResult::ok(crate::skills::render_skill_invocation(&spec, None))
    }
}
