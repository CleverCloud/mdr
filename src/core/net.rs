//! Fetching remote images so the graphical backends can display them (#60).
//!
//! Both graphical backends inline images as `data:` URIs; remote images are
//! downloaded here and inlined the same way, which keeps the strict
//! `img-src data:` CSP of the webview backend intact.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Refuse anything larger than this, so a hostile document cannot make mdr
/// download a disk image.
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// Whether a URL is one this module can fetch.
pub fn is_remote_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Download a remote image and return it as a `data:` URI.
///
/// Returns `None` when mdr is running offline, when the download fails, or when
/// the response is not an image. Results — including failures — are cached for
/// the life of the process, so a live-reload cycle does not re-download every
/// badge of a README.
pub fn remote_image_data_uri(url: &str) -> Option<String> {
    if !is_remote_url(url) || crate::core::offline() {
        return None;
    }

    if let Some(cached) = cache().lock().ok().and_then(|c| c.get(url).cloned()) {
        return cached;
    }

    let result = fetch(url);
    if result.is_none() {
        crate::vlog!("remote image not loaded: {}", url);
    }
    if let Ok(mut c) = cache().lock() {
        c.insert(url.to_string(), result.clone());
    }
    result
}

fn fetch(url: &str) -> Option<String> {
    use std::io::Read;

    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .into()
    });

    let response = agent.get(url).call().ok()?;
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_string());

    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES || bytes.is_empty() {
        return None;
    }

    let mime = content_type
        .filter(|t| t.starts_with("image/"))
        .or_else(|| mime_from_extension(url).map(str::to_string))?;

    Some(data_uri(&mime, &bytes))
}

fn mime_from_extension(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        _ => None,
    }
}

/// Build a `data:` URI from a MIME type and the raw bytes.
pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine;
    format!(
        "data:{};base64,{}",
        mime,
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_remote_urls() {
        assert!(is_remote_url("http://example.com/a.png"));
        assert!(is_remote_url("https://example.com/a.png"));
        assert!(!is_remote_url("images/a.png"));
        assert!(!is_remote_url("data:image/png;base64,AAAA"));
        assert!(!is_remote_url("file:///etc/passwd"));
    }

    #[test]
    fn mime_is_guessed_from_the_extension_ignoring_query_strings() {
        assert_eq!(mime_from_extension("https://a/b.png"), Some("image/png"));
        assert_eq!(
            mime_from_extension("https://img.shields.io/badge/x.svg?style=flat"),
            Some("image/svg+xml")
        );
        assert_eq!(mime_from_extension("https://a/badge"), None);
    }

    #[test]
    fn data_uri_is_well_formed() {
        assert_eq!(data_uri("image/png", b"ab"), "data:image/png;base64,YWI=");
    }

    #[test]
    fn offline_mode_refuses_to_fetch() {
        // The default is online; flip it for the duration of the assertion so
        // the test never touches the network either way.
        crate::core::set_offline(true);
        assert_eq!(remote_image_data_uri("https://example.com/a.png"), None);
        crate::core::set_offline(false);
    }

    #[test]
    fn non_remote_urls_are_never_fetched() {
        assert_eq!(remote_image_data_uri("images/a.png"), None);
    }
}
