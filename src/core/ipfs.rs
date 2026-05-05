use anyhow::{Context, bail};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const DEFAULT_PUMP_FUN_IPFS_UPLOAD_URL: &str = "https://pump.fun/api/ipfs";

#[derive(Debug, Clone)]
pub struct PumpFunIpfsUploadRequest<'a> {
    pub name: &'a str,
    pub symbol: &'a str,
    pub description: &'a str,
    pub twitter: Option<&'a str>,
    pub telegram: Option<&'a str>,
    pub website: Option<&'a str>,
    pub show_name: bool,
    pub file_name: &'a str,
    pub file_bytes: &'a [u8],
    pub file_content_type: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PumpFunIpfsUploadResponse {
    pub metadata: PumpFunIpfsMetadata,
    pub metadata_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PumpFunIpfsMetadata {
    pub name: String,
    pub symbol: String,
    pub description: String,
    pub image: String,

    #[serde(default)]
    pub twitter: Option<String>,
    #[serde(default)]
    pub telegram: Option<String>,
    #[serde(default)]
    pub website: Option<String>,

    #[serde(default)]
    pub show_name: Option<bool>,
    #[serde(default)]
    pub created_on: Option<String>,
}

pub fn guess_image_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

pub async fn upload_token_metadata_to_ipfs_via_pump_fun(
    client: &reqwest::Client,
    upload_url: &str,
    request: PumpFunIpfsUploadRequest<'_>,
) -> anyhow::Result<PumpFunIpfsUploadResponse> {
    let upload_url = upload_url.trim();
    if upload_url.is_empty() {
        bail!("missing upload_url");
    }

    let file_part = Part::bytes(request.file_bytes.to_vec())
        .file_name(request.file_name.to_string())
        .mime_str(request.file_content_type)
        .with_context(|| format!("invalid file content-type {}", request.file_content_type))?;

    let mut form = Form::new()
        .part("file", file_part)
        .text("name", request.name.to_string())
        .text("symbol", request.symbol.to_string())
        .text("description", request.description.to_string())
        .text(
            "showName",
            if request.show_name { "true" } else { "false" }.to_string(),
        );

    if let Some(v) = request.twitter.and_then(|v| {
        let t = v.trim();
        (!t.is_empty()).then_some(t)
    }) {
        form = form.text("twitter", v.to_string());
    }
    if let Some(v) = request.telegram.and_then(|v| {
        let t = v.trim();
        (!t.is_empty()).then_some(t)
    }) {
        form = form.text("telegram", v.to_string());
    }
    if let Some(v) = request.website.and_then(|v| {
        let t = v.trim();
        (!t.is_empty()).then_some(t)
    }) {
        form = form.text("website", v.to_string());
    }

    let response = client
        .post(upload_url)
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("POST {upload_url} failed"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("read ipfs upload response body")?;

    if !status.is_success() {
        bail!("ipfs upload failed (status {status}): {body}");
    }

    let parsed: PumpFunIpfsUploadResponse =
        serde_json::from_str(&body).context("decode ipfs upload response json")?;
    Ok(parsed)
}
