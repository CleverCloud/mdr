use std::path::PathBuf;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

use crate::core::markdown::{parse_markdown, GITHUB_CSS};
use crate::core::toc;
use crate::vlog;

pub fn run(file_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    // Canonicalize the file path first so parent() always gives an absolute directory.
    // Without this, a bare filename like "README.md" gives parent() = "" (empty),
    // which breaks relative image resolution when CWD differs from expected.
    let canonical_file = std::fs::canonicalize(&file_path)
        .unwrap_or_else(|_| {
            // If canonicalize fails, try current_dir + file_path
            std::env::current_dir()
                .map(|cwd| cwd.join(&file_path))
                .unwrap_or_else(|_| file_path.clone())
        });
    let base_dir = canonical_file.parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let markdown_content = std::fs::read_to_string(&file_path)?;
    vlog!("webview: file_path={}", file_path.display());
    vlog!("webview: base_dir={}", base_dir.display());
    vlog!("webview: markdown_content length={} bytes", markdown_content.len());
    let html_body = parse_markdown(&markdown_content);
    vlog!("webview: html_body length={} bytes", html_body.len());
    // In verbose mode, dump all <img> tags found in the HTML
    if crate::core::verbose() {
        for cap in regex::Regex::new(r#"<img\s[^>]*?>"#).unwrap().find_iter(&html_body) {
            let tag = cap.as_str();
            if tag.len() > 200 {
                vlog!("webview: found <img> tag: {}...", &tag[..200]);
            } else {
                vlog!("webview: found <img> tag: {}", tag);
            }
        }
    }
    let html_body = resolve_local_images(&html_body, &base_dir);
    let toc_entries = toc::extract_toc(&markdown_content);
    let full_html = build_html(&html_body, &toc_entries);

    let watcher_rx = crate::core::watcher::watch_file(&file_path)?;

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(format!("mdr - {}", file_path.display()))
        .with_inner_size(tao::dpi::LogicalSize::new(1100.0, 900.0))
        .build(&event_loop)?;

    let webview = WebViewBuilder::new()
        .with_html(&full_html)
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // Check for file changes
        if watcher_rx.try_recv().is_ok() {
            while watcher_rx.try_recv().is_ok() {}
            if let Ok(content) = std::fs::read_to_string(&file_path) {
                let new_html = parse_markdown(&content);
                let new_html = resolve_local_images(&new_html, &base_dir);
                let new_toc = toc::extract_toc(&content);
                let toc_html = build_toc_html(&new_toc);

                let body_json = serde_json::to_string(&new_html).unwrap_or_default();
                let toc_json = serde_json::to_string(&toc_html).unwrap_or_default();
                let js = format!(
                    "document.querySelector('.content').innerHTML = {}; document.querySelector('.sidebar ul').innerHTML = {};",
                    body_json, toc_json
                );
                let _ = webview.evaluate_script(&js);
            }
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            _ => {}
        }
    });
}

/// Resolve local image paths to inline base64 data URIs.
/// wry's `with_html()` does not allow loading file:// URLs, so we must embed images directly.
/// SVG files are rasterized to PNG first (to avoid executing embedded scripts/links).
/// Handles both `<img src="...">` and `<img alt="..." src="...">` attribute orders.
fn resolve_local_images(html: &str, base_dir: &std::path::Path) -> String {
    use regex::Regex;
    vlog!("resolve_local_images: base_dir={}", base_dir.display());
    // Match the entire <img ...> tag with src="..." anywhere inside
    let re = Regex::new(r#"<img\s[^>]*?src="([^"]+)"[^>]*?>"#).unwrap();
    re.replace_all(html, |caps: &regex::Captures| {
        let full_tag = &caps[0];
        let src = &caps[1];
        vlog!("  IMG src={:?}", src);
        // Skip URLs and existing data URIs
        if src.starts_with("http://") || src.starts_with("https://")
            || src.starts_with("data:") || src.starts_with("file://")
        {
            vlog!("    → skipped (remote/data URL)");
            return full_tag.to_string();
        }
        // URL-decode the src path (comrak may percent-encode spaces etc.)
        let decoded_src = percent_decode(src);
        // Resolve relative path
        let abs_path = base_dir.join(&decoded_src);
        vlog!("    abs_path={}", abs_path.display());
        vlog!("    exists={}", abs_path.exists());
        if abs_path.exists() {
            let is_svg = abs_path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("svg"))
                .unwrap_or(false);
            vlog!("    is_svg={}", is_svg);
            if is_svg {
                match rasterize_svg_to_png_data_uri(&abs_path) {
                    Ok(png_data_uri) => {
                        vlog!("    → SVG rasterized to PNG ({} bytes)", png_data_uri.len());
                        let re_src = Regex::new(r#"src="[^"]+""#).unwrap();
                        return re_src.replace(full_tag, format!("src=\"{}\"", png_data_uri).as_str()).to_string();
                    }
                    Err(e) => {
                        vlog!("    → SVG rasterization FAILED: {}", e);
                    }
                }
                // Fallback: embed SVG as data URI (scripts won't execute in <img> context)
                match file_to_data_uri(&abs_path) {
                    Ok(data_uri) => {
                        vlog!("    → SVG embedded as data URI ({} bytes)", data_uri.len());
                        let re_src = Regex::new(r#"src="[^"]+""#).unwrap();
                        return re_src.replace(full_tag, format!("src=\"{}\"", data_uri).as_str()).to_string();
                    }
                    Err(e) => {
                        vlog!("    → SVG file_to_data_uri FAILED: {}", e);
                    }
                }
                vlog!("    → SVG: all attempts failed, keeping original tag");
                return full_tag.to_string();
            }
            // For non-SVG images, use base64 data URI
            match file_to_data_uri(&abs_path) {
                Ok(data_uri) => {
                    vlog!("    → embedded as data URI ({} bytes)", data_uri.len());
                    let re_src = Regex::new(r#"src="[^"]+""#).unwrap();
                    return re_src.replace(full_tag, format!("src=\"{}\"", data_uri).as_str()).to_string();
                }
                Err(e) => {
                    vlog!("    → file_to_data_uri FAILED: {}", e);
                }
            }
        } else {
            vlog!("    → file NOT FOUND");
        }
        full_tag.to_string()
    })
    .to_string()
}

/// Decode percent-encoded URL path components (e.g. %20 -> space).
fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
            }
            result.push('%');
            result.push_str(&hex);
        } else {
            result.push(c);
        }
    }
    result
}

/// Convert a local file to a base64 data URI string.
fn file_to_data_uri(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mime = match ext.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    };
    let data = std::fs::read(path)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    Ok(format!("data:{};base64,{}", mime, b64))
}

fn build_toc_html(entries: &[toc::TocEntry]) -> String {
    let mut toc = String::new();
    for entry in entries {
        toc.push_str(&format!(
            "<li class=\"toc-h{}\"><a href=\"#{}\">{}</a></li>",
            entry.level, entry.anchor, entry.text
        ));
    }
    toc
}

/// Mermaid.js embedded at compile time — only injected when the Rust renderer fails.
const MERMAID_JS: &str = include_str!("../../assets/mermaid.min.js");

/// Rasterize an SVG file to PNG and return as a base64 data URI.
/// This is safer than inlining SVG because SVG can contain scripts, links, and styles
/// that would execute in the page context and cause unwanted navigation/requests.
/// Returns Err if the file is not a valid SVG (e.g., an HTML page saved with .svg extension).
fn rasterize_svg_to_png_data_uri(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;
    use std::sync::{Arc, OnceLock};

    let svg_data = std::fs::read_to_string(path)?;

    // Reject files that aren't actually SVG (e.g. HTML pages saved with .svg extension)
    let trimmed = svg_data.trim_start();
    if !trimmed.starts_with('<') || trimmed.starts_with("<!DOCTYPE html") || trimmed.starts_with("<html") {
        if !trimmed.contains("<svg") {
            return Err("File is not a valid SVG (possibly an HTML page)".into());
        }
    }

    // Max pixel dimension to avoid memory issues
    const MAX_DIM: f32 = 8192.0;

    // Reuse font database across calls
    static FONTDB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let fontdb = FONTDB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });

    let mut options = usvg::Options::default();
    options.fontdb = Arc::clone(fontdb);
    let tree = usvg::Tree::from_str(&svg_data, &options)?;
    let size = tree.size();
    let svg_w = size.width();
    let svg_h = size.height();

    if svg_w <= 0.0 || svg_h <= 0.0 {
        return Err("SVG has zero dimensions".into());
    }

    // Scale 2x for retina, but cap at MAX_DIM
    let ideal_scale = 2.0_f32;
    let max_scale_w = MAX_DIM / svg_w;
    let max_scale_h = MAX_DIM / svg_h;
    let scale = ideal_scale.min(max_scale_w).min(max_scale_h);

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
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);
    Ok(format!("data:image/png;base64,{}", b64))
}

fn build_html(body: &str, toc_entries: &[toc::TocEntry]) -> String {
    let toc_html = build_toc_html(toc_entries);
    // Only include mermaid.js if there are fallback blocks that need JS rendering
    let mermaid_script = if body.contains(r#"class="mermaid""#) {
        format!(
            r#"<script>{}</script>
<script>mermaid.initialize({{ startOnLoad: true, theme: (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) ? 'dark' : 'default' }});</script>"#,
            MERMAID_JS
        )
    } else {
        String::new()
    };

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>{css}</style>
</head>
<body>
<nav class="sidebar">
<p class="sidebar-title">Table of Contents</p>
<ul>{toc}</ul>
</nav>
<div class="content">
{body}
</div>
<script>
document.querySelector('.sidebar').addEventListener('click', function(e) {{
    if (e.target.tagName === 'A') {{
        e.preventDefault();
        var id = e.target.getAttribute('href').substring(1);
        var el = document.getElementById(id);
        if (el) {{
            el.scrollIntoView({{ behavior: 'smooth', block: 'start' }});
            document.querySelectorAll('.sidebar a').forEach(a => a.classList.remove('active'));
            e.target.classList.add('active');
        }}
    }}
}});
</script>
<div class="search-bar" id="searchBar" style="display:none;">
    <input type="text" id="searchInput" placeholder="Search..." />
    <span class="search-info" id="searchInfo">0/0</span>
    <button onclick="searchNav(-1)">&#9650;</button>
    <button onclick="searchNav(1)">&#9660;</button>
    <button class="close-btn" onclick="closeSearch()">Esc</button>
</div>
<script>
(function() {{
    var matches = [];
    var currentIdx = -1;

    function clearHighlights() {{
        document.querySelectorAll('mark.search-highlight').forEach(function(m) {{
            var parent = m.parentNode;
            parent.replaceChild(document.createTextNode(m.textContent), m);
            parent.normalize();
        }});
        matches = [];
        currentIdx = -1;
    }}

    function highlightMatches(query) {{
        clearHighlights();
        if (!query) {{ updateInfo(); return; }}
        var walker = document.createTreeWalker(
            document.querySelector('.content'),
            NodeFilter.SHOW_TEXT, null, false
        );
        var textNodes = [];
        while (walker.nextNode()) textNodes.push(walker.currentNode);

        var queryLower = query.toLowerCase();
        for (var i = textNodes.length - 1; i >= 0; i--) {{
            var node = textNodes[i];
            var text = node.textContent;
            var textLower = text.toLowerCase();
            var idx = textLower.lastIndexOf(queryLower);
            while (idx >= 0) {{
                var range = document.createRange();
                range.setStart(node, idx);
                range.setEnd(node, idx + query.length);
                var mark = document.createElement('mark');
                mark.className = 'search-highlight';
                range.surroundContents(mark);
                node = mark.previousSibling || node.parentNode.firstChild;
                idx = idx > 0 ? node.textContent.toLowerCase().lastIndexOf(queryLower, idx - 1) : -1;
            }}
        }}
        matches = document.querySelectorAll('mark.search-highlight');
        if (matches.length > 0) {{ currentIdx = 0; goToCurrent(); }}
        updateInfo();
    }}

    function goToCurrent() {{
        document.querySelectorAll('mark.search-highlight.current').forEach(function(m) {{ m.classList.remove('current'); }});
        if (matches.length > 0 && currentIdx >= 0) {{
            matches[currentIdx].classList.add('current');
            matches[currentIdx].scrollIntoView({{ behavior: 'smooth', block: 'center' }});
        }}
    }}

    function updateInfo() {{
        var info = document.getElementById('searchInfo');
        if (matches.length === 0) {{ info.textContent = '0/0'; }}
        else {{ info.textContent = (currentIdx + 1) + '/' + matches.length; }}
    }}

    window.searchNav = function(dir) {{
        if (matches.length === 0) return;
        currentIdx = (currentIdx + dir + matches.length) % matches.length;
        goToCurrent();
        updateInfo();
    }};

    window.closeSearch = function() {{
        document.getElementById('searchBar').style.display = 'none';
        clearHighlights();
        updateInfo();
    }};

    document.addEventListener('keydown', function(e) {{
        if ((e.ctrlKey || e.metaKey) && e.key === 'f') {{
            e.preventDefault();
            var bar = document.getElementById('searchBar');
            bar.style.display = 'flex';
            var input = document.getElementById('searchInput');
            input.focus();
            input.select();
        }}
        if (e.key === 'Escape') {{
            window.closeSearch();
        }}
        if (e.key === 'Enter' && document.activeElement === document.getElementById('searchInput')) {{
            e.preventDefault();
            if (e.shiftKey) {{ window.searchNav(-1); }}
            else {{ window.searchNav(1); }}
        }}
    }});

    document.getElementById('searchInput').addEventListener('input', function() {{
        highlightMatches(this.value);
    }});
}})();
</script>
{mermaid_script}
</body>
</html>"#,
        css = GITHUB_CSS,
        toc = toc_html,
        body = body,
        mermaid_script = mermaid_script
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_local_images_svg_rasterized_to_png() {
        let dir = std::env::temp_dir().join("mdr_test_webview_svg_raster");
        std::fs::create_dir_all(&dir).unwrap();

        let svg_content = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="100" height="100" fill="red"/></svg>"#;
        std::fs::write(dir.join("test.svg"), svg_content).unwrap();

        let html = r#"<img src="test.svg" alt="test">"#;
        let result = resolve_local_images(html, &dir);

        // SVG should be rasterized to PNG data URI (not inlined as raw SVG)
        assert!(result.contains("data:image/png;base64,"), "SVG should be rasterized to PNG, got: {}", result);
        assert!(!result.contains("<svg"), "Raw SVG should NOT be inlined (security), got: {}", result);
        assert!(result.contains("<img"), "Should remain an <img> tag with PNG data URI");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_svg_with_links_is_safe() {
        // SVGs with <a> tags must NOT be inlined (they cause navigation)
        let dir = std::env::temp_dir().join("mdr_test_webview_svg_links");
        std::fs::create_dir_all(&dir).unwrap();

        let svg_with_links = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
<a href="https://example.com"><rect width="100" height="100" fill="blue"/></a></svg>"#;
        std::fs::write(dir.join("logo.svg"), svg_with_links).unwrap();

        let html = r#"<img src="logo.svg" alt="logo">"#;
        let result = resolve_local_images(html, &dir);

        // Must NOT contain raw SVG with links
        assert!(!result.contains("href=\"https://example.com\""),
            "SVG links must not leak into page, got: {}", result);
        assert!(result.contains("data:image/png;base64,"),
            "Should be rasterized to safe PNG, got: {}", result);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_non_svg_uses_data_uri() {
        let dir = std::env::temp_dir().join("mdr_test_webview_png_datauri");
        std::fs::create_dir_all(&dir).unwrap();

        let png_path = dir.join("test.png");
        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(&png_path).unwrap();

        let html = r#"<img src="test.png" alt="pixel">"#;
        let result = resolve_local_images(html, &dir);

        assert!(result.contains("data:image/png;base64,"), "PNG should use data URI, got: {}", result);
        assert!(result.contains("<img"), "img tag should be preserved for PNG, got: {}", result);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_preserves_remote_urls() {
        let dir = std::env::temp_dir();
        let html = r#"<img src="https://example.com/image.svg" alt="remote">"#;
        let result = resolve_local_images(html, &dir);
        assert_eq!(result, html, "Remote URLs should be preserved unchanged");
    }

    #[test]
    fn resolve_local_images_subdirectory_paths() {
        // Simulate the real-world scenario: images in subdirectories
        let dir = std::env::temp_dir().join("mdr_test_webview_subdir");
        let img_dir = dir.join("assets").join("screenshots");
        std::fs::create_dir_all(&img_dir).unwrap();

        // Create a real PNG file in subdirectory
        let png_path = img_dir.join("chart.png");
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(&png_path).unwrap();

        // This is what comrak generates from ![alt](assets/screenshots/chart.png)
        let html = r#"<img src="assets/screenshots/chart.png" alt="Revenue chart" />"#;
        let result = resolve_local_images(html, &dir);

        assert!(result.contains("data:image/png;base64,"),
            "PNG in subdirectory should be resolved to data URI, got: {}",
            &result[..result.len().min(200)]);
        assert!(result.contains("<img"), "Should still be an img tag");
        assert!(result.contains("alt=\"Revenue chart\""), "Alt text should be preserved");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_empty_base_dir() {
        // When file_path.parent() is empty (bare filename), base_dir is ""
        // This should still work for files that exist relative to CWD
        let dir = std::env::temp_dir().join("mdr_test_webview_empty_base");
        std::fs::create_dir_all(&dir).unwrap();

        let png_path = dir.join("test.png");
        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([0, 255, 0, 255]));
        img.save(&png_path).unwrap();

        // With proper base_dir, it should work
        let html = r#"<img src="test.png" alt="test" />"#;
        let result = resolve_local_images(html, &dir);
        assert!(result.contains("data:image/png;base64,"),
            "Should resolve with proper base_dir, got: {}", &result[..result.len().min(200)]);

        // With empty base_dir, the file won't be found (unless CWD happens to match)
        let empty = std::path::PathBuf::from("");
        let result2 = resolve_local_images(html, &empty);
        // This will likely NOT find the file since CWD != dir
        // The tag should be returned unchanged
        assert!(result2.contains("src=\"test.png\"") || result2.contains("data:image/png;base64,"),
            "With empty base_dir, should either find file or return original, got: {}",
            &result2[..result2.len().min(200)]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_comrak_output_format() {
        // Test with the exact HTML format comrak produces from markdown images
        let dir = std::env::temp_dir().join("mdr_test_webview_comrak_format");
        let screenshots_dir = dir.join("assets").join("screenshots");
        std::fs::create_dir_all(&screenshots_dir).unwrap();

        let png_path = screenshots_dir.join("revenue.png");
        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([0, 0, 255, 255]));
        img.save(&png_path).unwrap();

        // Comrak generates self-closing tags with alt attribute
        let html = r#"<p><img src="assets/screenshots/revenue.png" alt="Monthly Revenue Growth — Jan 2023 to Feb 2026" /></p>"#;
        let result = resolve_local_images(html, &dir);

        assert!(result.contains("data:image/png;base64,"),
            "Comrak-style img tag should be resolved, got: {}", &result[..result.len().min(300)]);
        assert!(result.contains("alt=\"Monthly Revenue Growth"), "Alt text with special chars should be preserved");
        assert!(result.contains("<p>") && result.contains("</p>"), "Surrounding <p> tags should be preserved");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_multiple_images_in_html() {
        // Test multiple images in a single HTML string
        let dir = std::env::temp_dir().join("mdr_test_webview_multi_img");
        std::fs::create_dir_all(&dir).unwrap();

        for name in &["a.png", "b.png"] {
            let path = dir.join(name);
            let mut img = image::RgbaImage::new(1, 1);
            img.put_pixel(0, 0, image::Rgba([128, 128, 128, 255]));
            img.save(&path).unwrap();
        }

        let html = r#"<p><img src="a.png" alt="A" /></p><p><img src="b.png" alt="B" /></p>"#;
        let result = resolve_local_images(html, &dir);

        // Both images should be resolved
        let count = result.matches("data:image/png;base64,").count();
        assert_eq!(count, 2, "Both images should be resolved to data URIs, got {} matches in: {}", count, &result[..result.len().min(300)]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rasterize_svg_to_png_data_uri_basic() {
        let dir = std::env::temp_dir().join("mdr_test_rasterize_svg");
        std::fs::create_dir_all(&dir).unwrap();

        let svg = r#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="50" height="50"><circle cx="25" cy="25" r="20" fill="blue"/></svg>"#;
        let path = dir.join("test.svg");
        std::fs::write(&path, svg).unwrap();

        let result = rasterize_svg_to_png_data_uri(&path).unwrap();
        assert!(result.starts_with("data:image/png;base64,"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
