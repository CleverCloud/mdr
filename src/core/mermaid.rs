use regex::Regex;

/// Render a single mermaid diagram source to SVG.
/// Catches panics from mermaid-rs-renderer (which can panic on some inputs).
pub fn render_mermaid_to_svg(source: &str) -> Result<String, String> {
    let source = source.to_string();
    match std::panic::catch_unwind(|| mermaid_rs_renderer::render(&source)) {
        Ok(Ok(svg)) => Ok(svg),
        Ok(Err(e)) => Err(format!("{}", e)),
        Err(_) => Err("mermaid renderer panicked (unsupported diagram syntax)".to_string()),
    }
}

/// Process HTML from comrak: find mermaid code blocks and replace with rendered SVG.
/// Mermaid blocks appear as: <pre><code class="language-mermaid">...</code></pre>
pub fn process_mermaid_blocks(html: &str) -> String {
    let re = Regex::new(r#"<pre><code class="language-mermaid">([\s\S]*?)</code></pre>"#).unwrap();

    re.replace_all(html, |caps: &regex::Captures| {
        let source = html_decode(&caps[1]);
        match render_mermaid_to_svg(&source) {
            Ok(svg) => format!(r#"<div class="mermaid-diagram">{}</div>"#, svg),
            Err(err) => format!(
                r#"<div class="mermaid-error"><strong>Mermaid error:</strong> {}<pre><code>{}</code></pre></div>"#,
                html_encode(&err),
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
                Err(_) => format!("**Mermaid render error**: could not convert SVG to PNG\n\n```\n{}```", source),
            },
            Err(err) => format!("**Mermaid error**: {}\n\n```\n{}```", err, source),
        }
    })
    .to_string()
}

/// Convert SVG string to PNG and return as base64-encoded string.
#[cfg(feature = "egui-backend")]
fn svg_to_png_base64(svg: &str) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;
    use std::sync::{Arc, OnceLock};

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
    let width = size.width() as u32;
    let height = size.height() as u32;

    if width == 0 || height == 0 {
        return Err("SVG has zero dimensions".into());
    }

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or("Failed to create pixmap")?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

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
