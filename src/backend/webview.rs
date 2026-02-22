use std::path::PathBuf;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

use crate::core::markdown::{parse_markdown, GITHUB_CSS};
use crate::core::toc;

pub fn run(file_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let base_dir = file_path.parent()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
        .unwrap_or_default();
    let markdown_content = std::fs::read_to_string(&file_path)?;
    let html_body = parse_markdown(&markdown_content);
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
/// Handles both `<img src="...">` and `<img alt="..." src="...">` attribute orders.
fn resolve_local_images(html: &str, base_dir: &std::path::Path) -> String {
    use regex::Regex;
    // Match the entire <img ...> tag with src="..." anywhere inside
    let re = Regex::new(r#"<img\s[^>]*?src="([^"]+)"[^>]*?>"#).unwrap();
    re.replace_all(html, |caps: &regex::Captures| {
        let full_tag = &caps[0];
        let src = &caps[1];
        // Skip URLs and existing data URIs
        if src.starts_with("http://") || src.starts_with("https://")
            || src.starts_with("data:") || src.starts_with("file://")
        {
            return full_tag.to_string();
        }
        // URL-decode the src path (comrak may percent-encode spaces etc.)
        let decoded_src = percent_decode(src);
        // Resolve relative path
        let abs_path = base_dir.join(&decoded_src);
        if abs_path.exists() {
            // For SVG files, inline the SVG content directly instead of base64
            let is_svg = abs_path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("svg"))
                .unwrap_or(false);
            if is_svg {
                if let Ok(svg_content) = read_svg_for_inline(&abs_path) {
                    return format!(
                        r#"<div class="inline-svg" style="max-width:100%">{}</div>"#,
                        svg_content
                    );
                }
            }
            // For non-SVG images, use base64 data URI as before
            if let Ok(data_uri) = file_to_data_uri(&abs_path) {
                // Rebuild the tag with the data URI replacing the src
                let re_src = Regex::new(r#"src="[^"]+""#).unwrap();
                return re_src.replace(full_tag, format!("src=\"{}\"", data_uri).as_str()).to_string();
            }
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

/// Read an SVG file and return its content with XML declaration stripped.
fn read_svg_for_inline(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    // Strip XML declaration if present (<?xml ... ?>)
    let svg_content = regex::Regex::new(r"<\?xml[^?]*\?>")
        .unwrap()
        .replace(&content, "")
        .trim()
        .to_string();
    Ok(svg_content)
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
    fn resolve_local_images_inlines_svg() {
        let dir = std::env::temp_dir().join("mdr_test_webview_svg_inline");
        std::fs::create_dir_all(&dir).unwrap();

        let svg_content = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="100" height="100" fill="red"/></svg>"#;
        let svg_path = dir.join("test.svg");
        std::fs::write(&svg_path, svg_content).unwrap();

        let html = r#"<img src="test.svg" alt="test">"#;
        let result = resolve_local_images(html, &dir);

        // SVG should be inlined, not converted to data URI
        assert!(!result.contains("data:image/svg+xml"), "SVG should not use data URI, got: {}", result);
        assert!(result.contains(r#"<div class="inline-svg""#), "SVG should be wrapped in div, got: {}", result);
        assert!(result.contains("<svg"), "SVG content should be inlined, got: {}", result);
        assert!(!result.contains("<img"), "img tag should be replaced, got: {}", result);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_svg_strips_xml_declaration() {
        let dir = std::env::temp_dir().join("mdr_test_webview_svg_xmldecl");
        std::fs::create_dir_all(&dir).unwrap();

        let svg_content = r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="50" height="50"><circle cx="25" cy="25" r="20" fill="blue"/></svg>"#;
        let svg_path = dir.join("decl.svg");
        std::fs::write(&svg_path, svg_content).unwrap();

        let html = r#"<img src="decl.svg" alt="circle">"#;
        let result = resolve_local_images(html, &dir);

        assert!(!result.contains("<?xml"), "XML declaration should be stripped, got: {}", result);
        assert!(result.contains("<svg"), "SVG content should be present, got: {}", result);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_non_svg_uses_data_uri() {
        let dir = std::env::temp_dir().join("mdr_test_webview_png_datauri");
        std::fs::create_dir_all(&dir).unwrap();

        // Create a minimal valid PNG (1x1 pixel)
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
    fn read_svg_for_inline_strips_xml_decl() {
        let dir = std::env::temp_dir().join("mdr_test_read_svg_inline");
        std::fs::create_dir_all(&dir).unwrap();

        let svg_with_decl = r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#;
        let path = dir.join("test.svg");
        std::fs::write(&path, svg_with_decl).unwrap();

        let result = read_svg_for_inline(&path).unwrap();
        assert!(!result.contains("<?xml"), "Should strip XML declaration");
        assert!(result.starts_with("<svg"), "Should start with <svg tag");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
