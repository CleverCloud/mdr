use comrak::{markdown_to_html, Options};
use crate::core::mermaid::process_mermaid_blocks;

/// Convert markdown content to HTML with all GFM extensions enabled.
/// Processes mermaid code blocks into inline SVG diagrams.
/// Adds id attributes to headings for TOC anchor navigation.
pub fn parse_markdown(content: &str) -> String {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.render.unsafe_ = false;

    let html = markdown_to_html(content, &options);
    let html = add_heading_ids(&html);
    process_mermaid_blocks(&html)
}

/// Add id attributes to heading tags for anchor navigation.
fn add_heading_ids(html: &str) -> String {
    use regex::Regex;

    let re = Regex::new(r"<(h[1-6])>(.*?)</h[1-6]>").unwrap();
    re.replace_all(html, |caps: &regex::Captures| {
        let tag = &caps[1];
        let content = &caps[2];
        let plain_text = strip_html_tags(content);
        let id = slugify(&plain_text);
        format!("<{} id=\"{}\">{}</{}>", tag, id, content, tag)
    })
    .to_string()
}

fn strip_html_tags(html: &str) -> String {
    let re = regex::Regex::new(r"<[^>]+>").unwrap();
    re.replace_all(html, "").to_string()
}

fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else if c == ' ' { '-' } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("")
}

/// CSS for GitHub-like markdown rendering with dark/light theme support.
pub const GITHUB_CSS: &str = r#"
@media (prefers-color-scheme: dark) {
    :root { --bg: #0d1117; --fg: #e6edf3; --code-bg: #161b22; --border: #30363d; --link: #58a6ff; --blockquote: #8b949e; --sidebar-bg: #010409; --sidebar-hover: #161b22; --sidebar-active: #1f6feb33; }
}
@media (prefers-color-scheme: light) {
    :root { --bg: #ffffff; --fg: #1f2328; --code-bg: #f6f8fa; --border: #d0d7de; --link: #0969da; --blockquote: #656d76; --sidebar-bg: #f6f8fa; --sidebar-hover: #eaeef2; --sidebar-active: #ddf4ff; }
}
* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; height: 100%; }
body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif;
    font-size: 16px;
    line-height: 1.6;
    color: var(--fg);
    background: var(--bg);
    display: flex;
}
.sidebar {
    width: 250px;
    min-width: 250px;
    height: 100vh;
    position: fixed;
    top: 0;
    left: 0;
    background: var(--sidebar-bg);
    border-right: 1px solid var(--border);
    overflow-y: auto;
    padding: 16px 0;
    font-size: 14px;
}
.sidebar-title {
    font-weight: 600;
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--blockquote);
    padding: 8px 16px;
    margin: 0;
}
.sidebar ul { list-style: none; margin: 0; padding: 0; }
.sidebar li a {
    display: block;
    padding: 4px 16px;
    color: var(--fg);
    text-decoration: none;
    border-left: 3px solid transparent;
    transition: background 0.15s, border-color 0.15s;
}
.sidebar li a:hover { background: var(--sidebar-hover); }
.sidebar li a.active { background: var(--sidebar-active); border-left-color: var(--link); color: var(--link); }
.sidebar li.toc-h2 a { padding-left: 24px; }
.sidebar li.toc-h3 a { padding-left: 36px; font-size: 13px; }
.sidebar li.toc-h4 a { padding-left: 48px; font-size: 13px; color: var(--blockquote); }
.sidebar li.toc-h5 a, .sidebar li.toc-h6 a { padding-left: 56px; font-size: 12px; color: var(--blockquote); }
.content {
    margin-left: 250px;
    max-width: 900px;
    padding: 32px 24px;
    flex: 1;
}
h1, h2, h3, h4, h5, h6 { margin-top: 24px; margin-bottom: 16px; font-weight: 600; line-height: 1.25; }
h1 { font-size: 2em; padding-bottom: 0.3em; border-bottom: 1px solid var(--border); }
h2 { font-size: 1.5em; padding-bottom: 0.3em; border-bottom: 1px solid var(--border); }
code {
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    font-size: 85%;
    background: var(--code-bg);
    padding: 0.2em 0.4em;
    border-radius: 6px;
}
pre {
    background: var(--code-bg);
    padding: 16px;
    border-radius: 6px;
    overflow-x: auto;
    line-height: 1.45;
}
pre code { background: transparent; padding: 0; font-size: 85%; }
table { border-collapse: collapse; width: 100%; margin: 16px 0; }
th, td { border: 1px solid var(--border); padding: 6px 13px; }
th { font-weight: 600; background: var(--code-bg); }
blockquote {
    color: var(--blockquote);
    border-left: 4px solid var(--border);
    padding: 0 16px;
    margin: 16px 0;
}
a { color: var(--link); text-decoration: none; }
a:hover { text-decoration: underline; }
hr { border: none; border-top: 1px solid var(--border); margin: 24px 0; }
img { max-width: 100%; }
ul, ol { padding-left: 2em; }
input[type="checkbox"] { margin-right: 0.5em; }
.mermaid-diagram { text-align: center; margin: 16px 0; }
.mermaid-diagram svg { max-width: 100%; height: auto; }
.mermaid-error {
    border: 2px solid #f85149;
    border-radius: 6px;
    padding: 16px;
    margin: 16px 0;
    background: var(--code-bg);
}
.mermaid-error strong { color: #f85149; }
"#;
