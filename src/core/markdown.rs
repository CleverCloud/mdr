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
    options.render.unsafe_ = true;

    let html = markdown_to_html(content, &options);
    let html = add_heading_ids(&html);
    process_mermaid_blocks(&html)
}

/// Add id attributes to heading tags for anchor navigation.
fn add_heading_ids(html: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"<(h[1-6])>(.*?)</h[1-6]>").unwrap());
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
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"<[^>]+>").unwrap());
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- add_heading_ids tests ---

    #[test]
    fn heading_ids_added_to_h1() {
        let html = "<h1>Hello World</h1>";
        let result = add_heading_ids(html);
        assert!(result.contains(r#"<h1 id="hello-world">Hello World</h1>"#));
    }

    #[test]
    fn heading_ids_added_to_multiple_levels() {
        let html = "<h1>Title</h1><h2>Section</h2><h3>Sub</h3>";
        let result = add_heading_ids(html);
        assert!(result.contains(r#"<h1 id="title">"#));
        assert!(result.contains(r#"<h2 id="section">"#));
        assert!(result.contains(r#"<h3 id="sub">"#));
    }

    #[test]
    fn heading_ids_strip_inner_html_tags() {
        let html = "<h2>Hello <code>world</code></h2>";
        let result = add_heading_ids(html);
        assert!(result.contains(r#"id="hello-world""#));
        // Inner HTML is preserved in content
        assert!(result.contains("<code>world</code>"));
    }

    #[test]
    fn heading_ids_no_headings_unchanged() {
        let html = "<p>Just a paragraph</p>";
        let result = add_heading_ids(html);
        assert_eq!(result, html);
    }

    // --- strip_html_tags tests ---

    #[test]
    fn strip_html_tags_removes_tags() {
        assert_eq!(strip_html_tags("<b>bold</b>"), "bold");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<a href=\"#\">link</a>"), "link");
    }

    // --- parse_markdown integration tests ---

    #[test]
    fn parse_markdown_basic_paragraph() {
        let result = parse_markdown("Hello world");
        assert!(result.contains("Hello world"));
        assert!(result.contains("<p>"));
    }

    #[test]
    fn parse_markdown_heading_gets_id() {
        let result = parse_markdown("# My Title");
        assert!(result.contains(r#"id="my-title""#));
        assert!(result.contains("My Title"));
    }

    #[test]
    fn parse_markdown_multiple_headings_get_ids() {
        let result = parse_markdown("# First\n## Second\n### Third");
        assert!(result.contains(r#"id="first""#));
        assert!(result.contains(r#"id="second""#));
        assert!(result.contains(r#"id="third""#));
    }

    #[test]
    fn parse_markdown_table() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let result = parse_markdown(md);
        assert!(result.contains("<table>"));
        assert!(result.contains("<th>"));
        assert!(result.contains("<td>"));
    }

    #[test]
    fn parse_markdown_tasklist() {
        let md = "- [x] Done\n- [ ] Todo";
        let result = parse_markdown(md);
        assert!(result.contains("checkbox"));
    }

    #[test]
    fn parse_markdown_strikethrough() {
        let md = "This is ~~deleted~~ text.";
        let result = parse_markdown(md);
        assert!(result.contains("<del>"));
        assert!(result.contains("deleted"));
    }

    #[test]
    fn parse_markdown_mermaid_block_is_processed() {
        // A mermaid code block should be processed (either rendered or show error)
        let md = "```mermaid\ngraph LR\n  A-->B\n```";
        let result = parse_markdown(md);
        // The mermaid block should not remain as a raw code block with language-mermaid class
        // It should either be a rendered SVG diagram or a mermaid-error div
        assert!(
            result.contains("mermaid-diagram") || result.contains("mermaid-error") || result.contains("mermaid-fallback"),
            "Mermaid block should be processed, got: {}",
            result
        );
    }

    #[test]
    fn parse_markdown_empty_input() {
        let result = parse_markdown("");
        // Empty input should produce empty or minimal HTML
        assert!(result.is_empty() || result.trim().is_empty());
    }

    #[test]
    fn parse_markdown_code_block_not_mermaid() {
        let md = "```rust\nfn main() {}\n```";
        let result = parse_markdown(md);
        assert!(result.contains("<code"));
        assert!(!result.contains("mermaid-diagram"));
    }

    // --- raw HTML image tests (bug: local images not showing) ---

    #[test]
    fn parse_markdown_raw_html_img_preserved() {
        // Business docs often use raw HTML <img> tags for sizing
        let md = r#"<img src="chart.png" alt="Revenue chart" width="600" />"#;
        let result = parse_markdown(md);
        assert!(result.contains("<img"), "Raw HTML <img> tags should be preserved, got: {}", result);
        assert!(result.contains("chart.png"), "Image src should be preserved, got: {}", result);
    }

    #[test]
    fn parse_markdown_raw_html_img_with_attributes() {
        let md = r#"<p align="center"><img src="logo.png" alt="logo" width="200"/></p>"#;
        let result = parse_markdown(md);
        assert!(result.contains("<img"), "Centered HTML image should be preserved, got: {}", result);
        assert!(result.contains("logo.png"), "Image src should be preserved, got: {}", result);
    }

    #[test]
    fn parse_markdown_markdown_image_syntax_works() {
        // Standard markdown images should always work
        let md = "![alt text](image.png)";
        let result = parse_markdown(md);
        assert!(result.contains("<img"), "Markdown image should produce <img>, got: {}", result);
        assert!(result.contains("image.png"), "Image src should be present, got: {}", result);
    }
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
.mermaid-fallback {
    border: 1px solid var(--border);
    border-radius: 6px;
    margin: 16px 0;
    background: var(--code-bg);
    overflow: hidden;
}
.mermaid-fallback-header {
    padding: 8px 16px;
    font-size: 13px;
    font-weight: 600;
    color: var(--blockquote);
    border-bottom: 1px solid var(--border);
    background: var(--sidebar-bg);
}
.mermaid-icon { margin-right: 6px; }
.mermaid-fallback pre { margin: 0; border-radius: 0; }
.mermaid-fallback code { font-size: 13px; color: var(--fg); }
/* Search */
.search-bar {
    position: fixed;
    bottom: 0;
    left: 250px;
    right: 0;
    background: var(--code-bg);
    border-top: 1px solid var(--border);
    padding: 8px 16px;
    display: flex;
    align-items: center;
    gap: 8px;
    z-index: 1000;
    font-size: 14px;
}
.search-bar input {
    flex: 1;
    max-width: 400px;
    padding: 4px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font-size: 14px;
    outline: none;
}
.search-bar input:focus { border-color: var(--link); }
.search-bar .search-info { color: var(--blockquote); white-space: nowrap; }
.search-bar button {
    padding: 4px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--code-bg);
    color: var(--fg);
    cursor: pointer;
    font-size: 13px;
}
.search-bar button:hover { background: var(--sidebar-hover); }
.search-bar .close-btn { margin-left: auto; }
mark.search-highlight { background: #ffd33d55; color: inherit; border-radius: 2px; }
mark.search-highlight.current { background: #ffd33d; color: #000; }
"#;
