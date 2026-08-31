use super::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use omini_domain::config::InputModality;
use omini_domain::events::{PermissionPreview, ReadPermissionPreview};
use omini_domain::message::ContentBlock;
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
        if !ctx.settings.supports_input_modality(InputModality::Image) {
            return ToolResult::error(format!(
                "view_image requires image input, but current model '{}' does not declare support for image input",
                ctx.settings.active_model().model_id
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
