use super::*;

pub const CONTENT_SIZE_THRESHOLD: usize = 64 * 1024;

pub struct PreparedBlocks {
    pub values: Vec<serde_json::Value>,
    pub created_files: Vec<PathBuf>,
}

pub struct PreparedUiContent {
    pub value: String,
    pub created_files: Vec<PathBuf>,
}

pub fn prepare_ui_content(
    content: &str,
    thread_dir: &ThreadDir,
) -> Result<PreparedUiContent, StoreError> {
    if content.len() <= CONTENT_SIZE_THRESHOLD {
        return Ok(PreparedUiContent {
            value: content.to_string(),
            created_files: Vec::new(),
        });
    }
    let bytes = content.as_bytes();
    let sha256 = sha256_hex(bytes);
    let relative_path = PathBuf::from("sidecars").join(format!("{sha256}.json"));
    let path = thread_dir.path().join(&relative_path);
    let created_files = if write_atomically_if_absent(thread_dir, &path, bytes)? {
        vec![path]
    } else {
        Vec::new()
    };
    Ok(PreparedUiContent {
        value: serde_json::to_string(&serde_json::json!({
            "type": "sidecar_document",
            "path": path_to_relative_string(&relative_path)?,
            "bytes": bytes.len(),
            "sha256": sha256,
        }))?,
        created_files,
    })
}

pub(crate) fn load_ui_content(stored: &str, thread_dir: &ThreadDir) -> Result<String, StoreError> {
    let Ok(reference) = serde_json::from_str::<serde_json::Value>(stored) else {
        return Ok(stored.to_string());
    };
    if reference.get("type").and_then(serde_json::Value::as_str) != Some("sidecar_document") {
        return Ok(stored.to_string());
    }
    let relative = required_string(&reference, "path")?;
    let bytes = fs::read(safe_relative_path(thread_dir.path(), relative)?)?;
    verify_stored_bytes(&reference, &bytes)?;
    String::from_utf8(bytes)
        .map_err(|error| StoreError::InvalidData(format!("sidecar is not UTF-8: {error}")))
}

pub fn prepare_blocks(
    blocks: &[ContentBlock],
    thread_dir: &ThreadDir,
) -> Result<PreparedBlocks, StoreError> {
    let mut values = Vec::with_capacity(blocks.len());
    let mut created_files = Vec::new();
    for block in blocks {
        if let ContentBlock::Image(image) = block {
            let bytes = BASE64_STANDARD.decode(&image.source.data)?;
            let sha256 = sha256_hex(&bytes);
            let relative_path = asset_relative_path(&sha256, &image.source.media_type)?;
            let path = thread_dir.path().join(&relative_path);
            if write_atomically_if_absent(thread_dir, &path, &bytes)? {
                created_files.push(path);
            }
            values.push(serde_json::json!({
                "type": "asset",
                "path": path_to_relative_string(&relative_path)?,
                "mime_type": image.source.media_type,
                "bytes": bytes.len(),
                "sha256": sha256,
            }));
            continue;
        }

        let encoded = serde_json::to_vec(block)?;
        if should_externalize_block(block, encoded.len()) {
            let sha256 = sha256_hex(&encoded);
            let relative_path = PathBuf::from("sidecars").join(format!("{sha256}.json"));
            let path = thread_dir.path().join(&relative_path);
            if write_atomically_if_absent(thread_dir, &path, &encoded)? {
                created_files.push(path);
            }
            values.push(serde_json::json!({
                "type": "sidecar",
                "path": path_to_relative_string(&relative_path)?,
                "bytes": encoded.len(),
                "sha256": sha256,
            }));
        } else {
            values.push(serde_json::from_slice(&encoded)?);
        }
    }
    Ok(PreparedBlocks {
        values,
        created_files,
    })
}

fn should_externalize_block(block: &ContentBlock, encoded_len: usize) -> bool {
    match block {
        ContentBlock::Text(block) => block.text.len() > CONTENT_SIZE_THRESHOLD,
        ContentBlock::Thinking(block) => block.thinking.len() > CONTENT_SIZE_THRESHOLD,
        ContentBlock::ToolResult(block) => block.content.len() > CONTENT_SIZE_THRESHOLD,
        ContentBlock::ToolUse(_) => encoded_len > CONTENT_SIZE_THRESHOLD,
        ContentBlock::Image(_) => false,
    }
}

pub(crate) fn load_blocks(
    stored: &[serde_json::Value],
    thread_dir: &ThreadDir,
) -> Result<Vec<ContentBlock>, StoreError> {
    stored
        .iter()
        .map(
            |value| match value.get("type").and_then(serde_json::Value::as_str) {
                Some("asset") => {
                    let relative = required_string(value, "path")?;
                    let path = safe_relative_path(thread_dir.path(), relative)?;
                    let bytes = fs::read(path)?;
                    verify_stored_bytes(value, &bytes)?;
                    Ok(ContentBlock::from_base64_image(
                        required_string(value, "mime_type")?.to_string(),
                        BASE64_STANDARD.encode(bytes),
                    ))
                }
                Some("sidecar") => {
                    let relative = required_string(value, "path")?;
                    let path = safe_relative_path(thread_dir.path(), relative)?;
                    let bytes = fs::read(path)?;
                    verify_stored_bytes(value, &bytes)?;
                    Ok(serde_json::from_slice(&bytes)?)
                }
                _ => Ok(serde_json::from_value(value.clone())?),
            },
        )
        .collect()
}

pub(crate) fn persist_asset(
    thread_dir: &ThreadDir,
    bytes: &[u8],
    mime_type: &str,
) -> Result<(String, String), StoreError> {
    let sha256 = sha256_hex(bytes);
    let relative_path = asset_relative_path(&sha256, mime_type)?;
    let path = thread_dir.path().join(&relative_path);
    write_atomically_if_absent(thread_dir, &path, bytes)?;
    Ok((sha256, path_to_relative_string(&relative_path)?))
}

pub(crate) fn asset_path(
    thread_dir: &ThreadDir,
    sha256: &str,
    mime_type: &str,
) -> Result<PathBuf, StoreError> {
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidData("invalid attachment id".to_string()));
    }
    Ok(thread_dir
        .path()
        .join(asset_relative_path(sha256, mime_type)?))
}

fn asset_relative_path(sha256: &str, mime_type: &str) -> Result<PathBuf, StoreError> {
    let extension = match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => {
            return Err(StoreError::InvalidData(format!(
                "unsupported attachment MIME type {mime_type}"
            )));
        }
    };
    Ok(PathBuf::from("assets").join(format!("{sha256}.{extension}")))
}

fn write_atomically_if_absent(
    thread_dir: &ThreadDir,
    destination: &Path,
    bytes: &[u8],
) -> Result<bool, StoreError> {
    if destination.exists() {
        return Ok(false);
    }
    fs::create_dir_all(thread_dir.staging_dir())?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = thread_dir
        .staging_dir()
        .join(format!("{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> Result<(), std::io::Error> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp_path, destination)?;
        if let Some(parent) = destination.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error.into());
    }
    Ok(true)
}

fn required_string<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str, StoreError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| StoreError::InvalidData(format!("missing {field}")))
}

fn verify_stored_bytes(value: &serde_json::Value, bytes: &[u8]) -> Result<(), StoreError> {
    let expected_bytes = value
        .get("bytes")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| StoreError::InvalidData("missing bytes".to_string()))?;
    let expected_hash = required_string(value, "sha256")?;
    if expected_bytes != bytes.len() as u64 || expected_hash != sha256_hex(bytes) {
        return Err(StoreError::InvalidData(
            "sidecar or asset integrity check failed".to_string(),
        ));
    }
    Ok(())
}

fn safe_relative_path(root: &Path, relative: &str) -> Result<PathBuf, StoreError> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(StoreError::InvalidData(
            "persisted path is not a safe relative path".to_string(),
        ));
    }
    Ok(root.join(relative))
}

fn path_to_relative_string(path: &Path) -> Result<String, StoreError> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| StoreError::InvalidData("non-UTF-8 persisted path".to_string()))
}

// TODO: 需要考虑这里的性能问题
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub fn finish_prepared_write(
    result: Result<(), sqlx::Error>,
    created_files: &[PathBuf],
) -> Result<(), StoreError> {
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            cleanup_created_files(created_files);
            Err(error.into())
        }
    }
}

pub fn cleanup_created_files(paths: &[PathBuf]) {
    for path in paths {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), %error, "failed to clean unreferenced file");
        }
    }
}
