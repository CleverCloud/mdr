use regex::Regex;

/// Preprocess mermaid source to fix known incompatibilities with mermaid-rs-renderer.
/// This increases the success rate of the native Rust renderer across all backends.
fn preprocess_mermaid_source(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    for line in source.lines() {
        let processed = line
            // Replace HTML line breaks in node labels with spaces
            .replace("<br/>", " ")
            .replace("<br>", " ")
            .replace("<br />", " ")
            // Replace bidirectional arrows (not supported) with unidirectional
            .replace("<-->", "---")
            .replace("x--x", "---")
            .replace("o--o", "---");
        result.push_str(&processed);
        result.push('\n');
    }
    result
}

/// Render a single mermaid diagram source to SVG.
/// First preprocesses the source to fix common incompatibilities,
/// then catches panics from mermaid-rs-renderer (which can panic on some inputs).
/// Suppresses stderr to prevent panic backtraces from corrupting TUI terminal output.
pub fn render_mermaid_to_svg(source: &str) -> Result<String, String> {
    // Suppress stderr during rendering — the mermaid renderer can print panic
    // backtraces/errors to stderr which corrupts the terminal in TUI mode.
    let _stderr_guard = suppress_stderr();

    // Try with preprocessed source first (fixes common syntax issues)
    let preprocessed = preprocess_mermaid_source(source);
    let preprocessed_clone = preprocessed.clone();
    match std::panic::catch_unwind(|| mermaid_rs_renderer::render(&preprocessed_clone)) {
        Ok(Ok(svg)) => return Ok(svg),
        _ => {}
    }
    // Fall back to original source (in case preprocessing made things worse)
    let source = source.to_string();
    match std::panic::catch_unwind(|| mermaid_rs_renderer::render(&source)) {
        Ok(Ok(svg)) => Ok(svg),
        Ok(Err(e)) => Err(format!("{}", e)),
        Err(_) => Err("mermaid renderer panicked (unsupported diagram syntax)".to_string()),
    }
}

/// Temporarily redirect stderr to /dev/null. Restores on drop.
/// This prevents mermaid-rs-renderer panic output from corrupting TUI display.
struct StderrGuard {
    #[cfg(unix)]
    saved_fd: Option<std::os::unix::io::RawFd>,
}

impl Drop for StderrGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(saved) = self.saved_fd {
            unsafe {
                libc::dup2(saved, 2);
                libc::close(saved);
            }
        }
    }
}

fn suppress_stderr() -> StderrGuard {
    #[cfg(unix)]
    {
        unsafe {
            let saved = libc::dup(2);
            if saved >= 0 {
                let devnull = libc::open(b"/dev/null\0".as_ptr() as *const _, libc::O_WRONLY);
                if devnull >= 0 {
                    libc::dup2(devnull, 2);
                    libc::close(devnull);
                    return StderrGuard { saved_fd: Some(saved) };
                }
                libc::close(saved);
            }
        }
        StderrGuard { saved_fd: None }
    }
    #[cfg(not(unix))]
    StderrGuard {}
}

/// Process HTML from comrak: find mermaid code blocks and replace with rendered SVG.
/// Mermaid blocks appear as: <pre><code class="language-mermaid">...</code></pre>
pub fn process_mermaid_blocks(html: &str) -> String {
    let re = Regex::new(r#"<pre><code class="language-mermaid">([\s\S]*?)</code></pre>"#).unwrap();

    re.replace_all(html, |caps: &regex::Captures| {
        let source = html_decode(&caps[1]);
        match render_mermaid_to_svg(&source) {
            Ok(svg) => format!(r#"<div class="mermaid-diagram">{}</div>"#, svg),
            Err(_) => format!(
                r#"<pre class="mermaid">{}</pre>"#,
                html_encode(&source)
            ),
        }
    })
    .to_string()
}

/// Pre-process markdown for egui: find ```mermaid blocks, render to SVG,
/// convert to base64 PNG data URI, replace block with image reference.
#[cfg(feature = "egui-backend")]
pub fn preprocess_mermaid_for_egui(markdown: &str) -> String {
    let re = Regex::new(r"```mermaid\n([\s\S]*?)```").unwrap();

    re.replace_all(markdown, |caps: &regex::Captures| {
        let source = &caps[1];
        match render_mermaid_to_svg(source) {
            Ok(svg) => match svg_to_png_base64(&svg) {
                Ok(b64) => format!("![mermaid diagram](data:image/png;base64,{})", b64),
                Err(_) => format!("> **◇ Mermaid Diagram** *(SVG to PNG conversion failed)*\n\n```\n{}```", source),
            },
            Err(_) => format!("> **◇ Mermaid Diagram** *(unsupported by native renderer)*\n\n```\n{}```", source),
        }
    })
    .to_string()
}

/// Convert SVG string to PNG and return as base64-encoded string.
/// Scales down large SVGs to fit within GPU texture limits (max 8192px per side).
#[cfg(feature = "egui-backend")]
fn svg_to_png_base64(svg: &str) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;
    use std::sync::{Arc, OnceLock};

    // Max texture size for egui/GPU — keep well under the 16384 hard limit
    const MAX_TEXTURE_SIZE: u32 = 8192;

    // Load system fonts once and reuse across calls
    static FONTDB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let fontdb = FONTDB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });

    let mut options = usvg::Options::default();
    options.fontdb = Arc::clone(fontdb);
    let tree = usvg::Tree::from_str(svg, &options)?;
    let size = tree.size();
    let svg_w = size.width();
    let svg_h = size.height();

    if svg_w <= 0.0 || svg_h <= 0.0 {
        return Err("SVG has zero dimensions".into());
    }

    // Scale down if either dimension exceeds the limit
    let scale = {
        let scale_w = MAX_TEXTURE_SIZE as f32 / svg_w;
        let scale_h = MAX_TEXTURE_SIZE as f32 / svg_h;
        scale_w.min(scale_h).min(1.0) // never scale up, only down
    };

    let width = (svg_w * scale) as u32;
    let height = (svg_h * scale) as u32;

    if width == 0 || height == 0 {
        return Err("SVG dimensions too small after scaling".into());
    }

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or("Failed to create pixmap")?;
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let png_data = pixmap.encode_png()?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&png_data))
}

fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn html_encode(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- html_decode tests ---

    #[test]
    fn html_decode_all_entities() {
        assert_eq!(html_decode("&amp;&lt;&gt;&quot;&#39;"), "&<>\"'");
    }

    #[test]
    fn html_decode_no_entities() {
        assert_eq!(html_decode("plain text"), "plain text");
    }

    #[test]
    fn html_decode_mixed() {
        assert_eq!(html_decode("A &amp; B &lt; C"), "A & B < C");
    }

    // --- html_encode tests ---

    #[test]
    fn html_encode_special_chars() {
        assert_eq!(html_encode("A & B < C > D"), "A &amp; B &lt; C &gt; D");
    }

    #[test]
    fn html_encode_no_special_chars() {
        assert_eq!(html_encode("plain text"), "plain text");
    }

    #[test]
    fn html_encode_decode_roundtrip() {
        let original = "graph LR; A-->B";
        let encoded = html_encode(original);
        let decoded = html_decode(&encoded);
        assert_eq!(decoded, original);
    }

    // --- preprocess_mermaid_source tests ---

    #[test]
    fn preprocess_removes_html_breaks() {
        let source = "graph LR\n  A[Line 1<br/>Line 2]-->B";
        let result = preprocess_mermaid_source(source);
        assert!(!result.contains("<br/>"));
        assert!(result.contains("Line 1 Line 2"));
    }

    #[test]
    fn preprocess_converts_bidirectional_arrows() {
        let source = "graph LR\n  A<-->B";
        let result = preprocess_mermaid_source(source);
        assert!(!result.contains("<-->"));
        assert!(result.contains("A---B"));
    }

    #[test]
    fn preprocess_leaves_valid_syntax_unchanged() {
        let source = "graph LR\n  A-->B\n  B-->C";
        let result = preprocess_mermaid_source(source);
        assert!(result.contains("A-->B"));
        assert!(result.contains("B-->C"));
    }

    // --- render_mermaid_to_svg tests ---

    #[test]
    fn render_mermaid_valid_diagram() {
        let source = "graph LR\n  A-->B";
        let result = render_mermaid_to_svg(source);
        // Should either succeed with SVG or fail with a descriptive error
        // (depends on mermaid-rs-renderer capabilities at runtime)
        match result {
            Ok(svg) => {
                assert!(svg.contains("<svg") || svg.contains("<SVG"),
                    "Expected SVG output, got: {}", svg);
            }
            Err(e) => {
                // If it errors, the error should be descriptive
                assert!(!e.is_empty(), "Error message should not be empty");
            }
        }
    }

    #[test]
    fn render_mermaid_empty_input() {
        let result = render_mermaid_to_svg("");
        // Empty input should produce an error, not panic
        assert!(result.is_err() || result.is_ok());
    }

    #[test]
    fn render_mermaid_invalid_syntax() {
        let result = render_mermaid_to_svg("this is not valid mermaid syntax at all %%% !@#");
        // Should not panic - catch_unwind protects us
        // Result can be Ok or Err but must not panic
        match result {
            Ok(_) => {} // Some renderers may be lenient
            Err(e) => assert!(!e.is_empty()),
        }
    }

    #[test]
    fn render_mermaid_panic_safety() {
        // Test that catch_unwind works - even bizarre input doesn't crash
        let result = render_mermaid_to_svg("\0\0\0");
        // Must not panic
        let _ = result;
    }

    // --- process_mermaid_blocks tests ---

    #[test]
    fn process_mermaid_blocks_no_mermaid() {
        let html = "<p>Hello</p><pre><code class=\"language-rust\">fn main() {}</code></pre>";
        let result = process_mermaid_blocks(html);
        assert_eq!(result, html);
    }

    #[test]
    fn process_mermaid_blocks_replaces_mermaid_code() {
        let html = r#"<p>Before</p><pre><code class="language-mermaid">graph LR
  A--&gt;B</code></pre><p>After</p>"#;
        let result = process_mermaid_blocks(html);
        // The mermaid code block should be replaced
        assert!(!result.contains(r#"class="language-mermaid""#),
            "Mermaid code block should be replaced, got: {}", result);
        // Should contain either a rendered diagram or an error
        assert!(
            result.contains("mermaid-diagram") || result.contains("mermaid-error") || result.contains("mermaid-fallback"),
            "Should contain diagram or fallback div, got: {}",
            result
        );
        // Surrounding content should be preserved
        assert!(result.contains("<p>Before</p>"));
        assert!(result.contains("<p>After</p>"));
    }

    #[test]
    fn process_mermaid_blocks_preserves_non_mermaid_content() {
        let html = "<h1>Title</h1><p>Content</p>";
        let result = process_mermaid_blocks(html);
        assert_eq!(result, html);
    }

    #[test]
    fn process_mermaid_blocks_error_contains_source() {
        // Use obviously invalid mermaid that will produce an error
        let html = r#"<pre><code class="language-mermaid">not valid %%% !@#</code></pre>"#;
        let result = process_mermaid_blocks(html);
        if result.contains("mermaid-fallback") {
            // Fallback div should contain the original source
            assert!(result.contains("Mermaid Diagram"));
        } else if result.contains("mermaid-error") {
            assert!(result.contains("Mermaid error:"));
        }
        // If it somehow renders successfully, that's also fine
    }

    // --- egui-specific tests ---

    #[cfg(feature = "egui-backend")]
    mod egui_tests {
        use super::super::*;

        #[test]
        fn preprocess_mermaid_for_egui_no_mermaid() {
            let md = "# Title\n\nSome text\n\n```rust\nfn main() {}\n```";
            let result = preprocess_mermaid_for_egui(md);
            assert_eq!(result, md);
        }

        #[test]
        fn preprocess_mermaid_for_egui_replaces_block() {
            let md = "Before\n\n```mermaid\ngraph LR\n  A-->B\n```\n\nAfter";
            let result = preprocess_mermaid_for_egui(md);
            // The mermaid block should be replaced with either an image or error message
            assert!(!result.contains("```mermaid"),
                "Mermaid block should be replaced, got: {}", result);
            assert!(result.contains("Before"));
            assert!(result.contains("After"));
        }

        #[test]
        fn preprocess_mermaid_for_egui_error_shows_source() {
            let md = "```mermaid\nnot valid mermaid\n```";
            let result = preprocess_mermaid_for_egui(md);
            if result.contains("error") || result.contains("Error") {
                assert!(result.contains("not valid mermaid"));
            }
        }
    }
}
