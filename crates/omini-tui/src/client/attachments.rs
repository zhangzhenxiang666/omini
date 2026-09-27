use omini_protocol as protocol;

use crate::client::*;
pub async fn upload_images(
    http: &reqwest::Client,
    base: &str,
    client_id: &str,
    input: &mut crate::client::input::ClientUserInput,
) -> Result<(), String> {
    for image in &input.images {
        let bytes = tokio::fs::read(&image.source_path)
            .await
            .map_err(|error| format!("read image {}: {error}", image.source_path))?;
        let mime_type = image_mime_type(&image.source_path)?;
        let file_name = if image.file_name.is_empty() {
            "attachment".to_string()
        } else {
            image.file_name.clone()
        };
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str(mime_type)
            .map_err(|error| format!("build image upload: {error}"))?;
        let url = format!("{base}/attachments");
        let response = http
            .post(&url)
            .header(CLIENT_ID_HEADER, client_id)
            .multipart(reqwest::multipart::Form::new().part("file", part))
            .send()
            .await
            .map_err(|error| format!("upload image: {error}"))?;
        let uploaded: protocol::AttachmentUploadResponse = decode_response(response, &url).await?;
        input.input.attachment_ids.push(uploaded.attachment_id);
    }
    Ok(())
}

pub fn image_mime_type(path: &str) -> Result<&'static str, String> {
    match std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Ok("image/png"),
        Some("jpg" | "jpeg") => Ok("image/jpeg"),
        Some("webp") => Ok("image/webp"),
        Some("gif") => Ok("image/gif"),
        _ => Err(format!("unsupported image extension: {path}")),
    }
}

pub fn percent_encode(value: &str) -> String {
    // agent id 目前作为 path segment 传输，手写最小 percent-encode 避免引入额外依赖。
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
