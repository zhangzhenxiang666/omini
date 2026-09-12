use crate::daemon::GlobalDaemonManager;
use crate::routes::{ApiError, api_error, core_error, ensure_controller, require_daemon_thread};
use axum::Json;
use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, ETAG, HeaderName};
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode};
use omini_protocol as protocol;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

pub(crate) const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

pub async fn upload_attachment(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<protocol::AttachmentUploadResponse>), ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_controller(&thread, &headers).await?;

    let mut upload = None;
    while let Some(mut field) = multipart.next_field().await.map_err(invalid_multipart)? {
        if field.name() != Some("file") || upload.is_some() {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "invalid_attachment_upload",
                "multipart upload must contain exactly one 'file' field",
            ));
        }
        let name = field.file_name().unwrap_or("attachment").to_string();
        let mime_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_ascii_lowercase();
        let staging_dir = thread.thread_dir().staging_dir();
        tokio::fs::create_dir_all(&staging_dir)
            .await
            .map_err(|error| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "attachment_storage_failed",
                    format!("failed to create attachment staging directory: {error}"),
                )
            })?;
        let path = staging_dir.join(format!("{}.upload", uuid::Uuid::new_v4()));
        let mut staged = StagedUpload::new(path);
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged.path)
            .await
            .map_err(storage_error)?;
        let mut size = 0usize;
        let mut magic = Vec::with_capacity(12);
        let mut hasher = Sha256::new();
        while let Some(chunk) = field.chunk().await.map_err(invalid_multipart)? {
            size = checked_upload_size(size, chunk.len())?;
            let needed = 12usize.saturating_sub(magic.len()).min(chunk.len());
            magic.extend_from_slice(&chunk[..needed]);
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(storage_error)?;
        }
        file.sync_all().await.map_err(storage_error)?;
        drop(file);
        validate_image(&mime_type, &magic)?;
        staged.size = size as u64;
        staged.sha256 = format!("{:x}", hasher.finalize());
        upload = Some((name, mime_type, staged));
    }

    let Some((name, mime_type, staged)) = upload else {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_attachment_upload",
            "multipart upload is missing the 'file' field",
        ));
    };
    let attachment = thread
        .persist_staged_attachment(
            &staged.path,
            staged.size,
            staged.sha256.clone(),
            &mime_type,
            &name,
        )
        .await
        .map_err(core_error)?;
    Ok((
        StatusCode::CREATED,
        Json(protocol::AttachmentUploadResponse {
            attachment_id: attachment.id,
        }),
    ))
}

pub async fn get_attachment(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id, attachment_id)): Path<(String, String, String)>,
) -> Result<Response<Body>, ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    let Some(attachment) = thread
        .get_attachment(&attachment_id)
        .await
        .map_err(core_error)?
    else {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "attachment_not_found",
            "Attachment does not exist in this thread",
        ));
    };
    let bytes = thread
        .load_attachment_bytes(&attachment)
        .map_err(core_error)?;
    let file_name = safe_header_filename(&attachment.original_name);
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    let response_headers = response.headers_mut();
    response_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&attachment.mime_type).map_err(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_attachment_metadata",
                "Stored attachment MIME type is invalid",
            )
        })?,
    );
    response_headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&attachment.size.to_string()).expect("integer is a valid header"),
    );
    response_headers.insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("inline; filename=\"{file_name}\""))
            .expect("sanitized filename is a valid header"),
    );
    response_headers.insert(
        ETAG,
        HeaderValue::from_str(&format!("\"{}\"", attachment.sha256))
            .expect("sha256 is a valid header"),
    );
    response_headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

fn invalid_multipart(error: axum::extract::multipart::MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachment_too_large",
            "attachment exceeds the 20 MiB limit",
        );
    }
    api_error(
        StatusCode::BAD_REQUEST,
        "invalid_attachment_upload",
        error.to_string(),
    )
}

fn storage_error(error: std::io::Error) -> ApiError {
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "attachment_storage_failed",
        error.to_string(),
    )
}

fn checked_upload_size(current: usize, chunk: usize) -> Result<usize, ApiError> {
    let next = current.saturating_add(chunk);
    if next > MAX_ATTACHMENT_BYTES {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachment_too_large",
            "attachment exceeds the 20 MiB limit",
        ));
    }
    Ok(next)
}

struct StagedUpload {
    path: std::path::PathBuf,
    size: u64,
    sha256: String,
}

impl StagedUpload {
    fn new(path: std::path::PathBuf) -> Self {
        Self {
            path,
            size: 0,
            sha256: String::new(),
        }
    }
}

impl Drop for StagedUpload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn validate_image(mime_type: &str, bytes: &[u8]) -> Result<(), ApiError> {
    let valid = match mime_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => false,
    };
    if !valid {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "unsupported_attachment_type",
            "attachment must be a PNG, JPEG, GIF, or WebP image matching its MIME type",
        ));
    }
    Ok(())
}

fn safe_header_filename(name: &str) -> String {
    let value = name
        .chars()
        .map(|character| match character {
            '/' | '\\' | '"' | '\r' | '\n' => '_',
            character if character.is_control() => '_',
            character if character.is_ascii() => character,
            _ => '_',
        })
        .collect::<String>();
    if value.is_empty() {
        "attachment".to_string()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_magic_must_match_the_declared_mime_type() {
        for (mime_type, bytes) in [
            ("image/png", b"\x89PNG\r\n\x1a\n".as_slice()),
            ("image/jpeg", &[0xff, 0xd8, 0xff][..]),
            ("image/gif", b"GIF89a".as_slice()),
            ("image/webp", b"RIFF\0\0\0\0WEBP".as_slice()),
        ] {
            validate_image(mime_type, bytes).expect("matching image signature should pass");
        }
        assert!(validate_image("image/jpeg", b"\x89PNG\r\n\x1a\n").is_err());
        assert!(validate_image("application/octet-stream", b"GIF89a").is_err());
    }

    #[test]
    fn upload_limit_and_download_filename_are_stable() {
        assert_eq!(MAX_ATTACHMENT_BYTES, 20 * 1024 * 1024);
        assert_eq!(
            checked_upload_size(0, MAX_ATTACHMENT_BYTES).expect("exact limit should pass"),
            MAX_ATTACHMENT_BYTES
        );
        assert!(checked_upload_size(MAX_ATTACHMENT_BYTES, 1).is_err());
        assert_eq!(safe_header_filename("../图\".png"), "..___.png");
    }
}
