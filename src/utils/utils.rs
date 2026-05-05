use anyhow::Context;
use base64::{Engine as _, engine::general_purpose::STANDARD as base64_std};

pub fn decode_b64(b64: &str) -> anyhow::Result<Vec<u8>> {
    let bytes = base64_std
        .decode(b64)
        .with_context(|| "base64 decode failed")?;
    if bytes.len() < 8 {
        anyhow::bail!("too short: {}", bytes.len());
    }
    Ok(bytes)
}
