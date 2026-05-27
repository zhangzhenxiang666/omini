use super::{Tool, ToolExecutionContext, ToolResult};
use crate::types::config::InputModality;
use crate::types::events::{PermissionPreview, ReadPermissionPreview};
use crate::types::message::ContentBlock;
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ViewImageInput {
    /// The absolute path to a PNG, JPEG, WebP, or GIF image file.
    pub path: String,
}

pub struct ViewImageTool;

#[derive(Debug, Clone)]
pub struct PreparedViewImage {
    path: PathBuf,
    media_type: String,
}

#[async_trait]
impl Tool for ViewImageTool {
    type Input = ViewImageInput;
    type Prepared = PreparedViewImage;

    fn name(&self) -> &str {
        "view_image"
    }

    fn description(&self) -> &str {
        concat!(
            "Read a local image file and include it in the next model request.\n",
            "\n",
            "Input:\n",
            "  path  Absolute path to a png, jpg, jpeg, webp, or gif image file.\n",
            "\n",
            "Rules:\n",
            "  - path must be absolute; relative paths are rejected.\n",
            "  - Use this only when the current model supports image input."
        )
    }

    async fn prepare(&self, input: ViewImageInput) -> Result<Self::Prepared, ToolResult> {
        let path = PathBuf::from(input.path);
        validate_path(&path)
            .map(|media_type| PreparedViewImage { path, media_type })
            .map_err(ToolResult::error)
    }

    fn permission_preview(&self, prepared: &Self::Prepared) -> Option<PermissionPreview> {
        Some(PermissionPreview::Read(ReadPermissionPreview {
            file_path: prepared.path.display().to_string(),
        }))
    }

    async fn execute_prepared(
        &self,
        prepared: Self::Prepared,
        ctx: ToolExecutionContext,
    ) -> ToolResult {
        let Some(settings) = ctx.settings.as_deref() else {
            return ToolResult::error("view_image requires query settings");
        };
        if !settings.supports_input_modality(InputModality::Image) {
            return ToolResult::error(format!(
                "view_image requires image input, but current model '{}' does not declare support for image input",
                settings.model
            ));
        }

        let bytes = match fs::read(&prepared.path).await {
            Ok(bytes) => bytes,
            Err(e) => {
                return ToolResult::error(format!(
                    "Failed to read image {}: {e}",
                    prepared.path.display()
                ));
            }
        };

        let encoded = STANDARD.encode(&bytes);
        let image = ContentBlock::from_base64_image(prepared.media_type.clone(), encoded);
        ToolResult::ok(format!(
            "Loaded image: {} ({} bytes, {})",
            prepared.path.display(),
            bytes.len(),
            prepared.media_type
        ))
        .with_extra_blocks(vec![image])
    }
}

fn validate_path(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err(format!("path must be absolute: {}", path.display()));
    }
    if !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }
    if !path.is_file() {
        return Err(format!("Path is not a file: {}", path.display()));
    }
    media_type_for_path(path)
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "Unsupported image extension for {}. Supported extensions: png, jpg, jpeg, webp, gif",
                path.display()
            )
        })
}

fn media_type_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::config::{ModelConfig, ProviderProfile, ProviderType, Settings};
    use std::collections::HashMap;

    fn image_settings() -> Settings {
        let provider = "test".to_string();
        let model = "vision-model".to_string();
        let mut providers = HashMap::new();
        providers.insert(
            provider.clone(),
            ProviderProfile {
                name: "Test".to_string(),
                endpoint: ProviderType::OpenAI,
                api_key: String::new(),
                base_url: String::new(),
                models: vec![ModelConfig {
                    id: model.clone(),
                    name: None,
                    limit: 256000,
                    thinking: false,
                    input_modalities: Some(vec![InputModality::Text, InputModality::Image]),
                }],
            },
        );
        Settings {
            api_key: String::new(),
            base_url: String::new(),
            model,
            endpoint: ProviderType::OpenAI,
            providers,
            active_provider: provider,
            system_prompt: None,
            language: None,
            max_turns: None,
            cwd: std::env::temp_dir(),
            thinking_effort: None,
            permissions: None,
            compact: Default::default(),
            mcp_servers: HashMap::new(),
        }
    }

    fn temp_image_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "omini-view-image-test-{}-{name}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn view_image_returns_text_result_and_extra_image_block() {
        let path = temp_image_path("ok.png");
        fs::write(&path, b"png-bytes").await.unwrap();
        let prepared = ViewImageTool
            .prepare(ViewImageInput {
                path: path.display().to_string(),
            })
            .await
            .unwrap();
        let mut ctx = ToolExecutionContext::test("view_image");
        ctx.settings = Some(std::sync::Arc::new(image_settings()));

        let result = ViewImageTool.execute_prepared(prepared, ctx).await;

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("Loaded image"));
        let blocks = result.extra_blocks.expect("image block should be attached");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Image(image) => {
                assert_eq!(image.source.media_type, "image/png");
                assert_eq!(image.source.data, STANDARD.encode(b"png-bytes"));
            }
            other => panic!("expected image block, got {other:?}"),
        }

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn view_image_rejects_relative_path() {
        let err = ViewImageTool
            .prepare(ViewImageInput {
                path: "relative.png".to_string(),
            })
            .await
            .unwrap_err();

        assert!(err.output.contains("path must be absolute"));
    }

    #[tokio::test]
    async fn view_image_requires_image_capable_model() {
        let path = temp_image_path("no-capability.png");
        fs::write(&path, b"png-bytes").await.unwrap();
        let prepared = ViewImageTool
            .prepare(ViewImageInput {
                path: path.display().to_string(),
            })
            .await
            .unwrap();
        let mut settings = image_settings();
        let provider_key = settings.active_provider.clone();
        settings.providers.get_mut(&provider_key).unwrap().models[0].input_modalities = None;
        let mut ctx = ToolExecutionContext::test("view_image");
        ctx.settings = Some(std::sync::Arc::new(settings));

        let result = ViewImageTool.execute_prepared(prepared, ctx).await;

        assert!(result.is_error);
        assert!(result.output.contains("does not declare support"));

        let _ = fs::remove_file(path).await;
    }
}
