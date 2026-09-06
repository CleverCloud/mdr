//! Sanitisation of the *document* HTML (#62).
//!
//! `parse_markdown` renders with `unsafe = true`, so raw HTML written in a
//! `.md` file reaches the page untouched — including `<script>`. mdr's own
//! scripts are inline, so the page CSP cannot drop `script-src 'unsafe-inline'`
//! and a document script would run with the same privileges as mdr's own.
//! Everything that comes from the document is therefore filtered here, before
//! mdr wraps it in its template.
//!
//! Only the document body goes through this function. mdr's own template
//! (search, keyboard, Mermaid and highlight.js scripts) is added afterwards and
//! is never sanitised.
//!
//! # Approach and limits
//!
//! This is a small HTML *tokenizer*, not a set of regexes: a regex over tags
//! cannot tell `<a href="a>b">` from a closed tag, and stacking patterns is how
//! filters get bypassed. The tokenizer walks the string once, understands
//! quoted/unquoted attribute values, comments and raw-text elements, and
//! re-emits a tag only from the parts it recognised.
//!
//! Known limits, stated honestly:
//!
//! * It is not a full HTML5 parser. It does not rebuild a tree, so it cannot
//!   defend against mutation-XSS caused by the browser re-nesting a malformed
//!   fragment. Comments and processing instructions are dropped outright, which
//!   removes the most common source of that class of bug.
//! * URL schemes are matched on the first 256 characters of the value after
//!   entity decoding; a scheme cannot be longer, but a hostile value could hide
//!   an encoding this decoder does not know (it handles numeric references and
//!   a short list of named ones).
//! * `data:image/...` is allowed, because mdr inlines every local image that
//!   way. `data:image/svg+xml` therefore survives; in an `<img>` context SVG
//!   scripts do not run, and following such a link is refused by the webview
//!   navigation handler.
//! * `<style>` is kept — Mermaid's inline SVG carries one — so a document can
//!   still restyle the page. It cannot load anything: the CSP forbids every
//!   remote fetch.

/// Elements dropped together with everything they contain.
const DROP_WITH_CONTENT: &[&str] = &["script"];

/// Elements whose tags are dropped while their text content is kept.
const DROP_TAG_ONLY: &[&str] = &[
    "iframe", "object", "embed", "base", "form", "link", "meta", "frame", "frameset", "applet",
];

/// Elements whose content is raw text and must be copied verbatim.
const RAW_TEXT: &[&str] = &["style"];

/// Attributes carrying a URL, checked against the dangerous schemes.
const URL_ATTRS: &[&str] = &[
    "href",
    "src",
    "xlink:href",
    "action",
    "formaction",
    "data",
    "poster",
    "background",
    "cite",
    "longdesc",
];

/// Attributes dropped whatever their value.
const ALWAYS_DROP_ATTRS: &[&str] = &["srcdoc", "ping", "http-equiv"];

/// Strip everything a document must not be able to do: run scripts, navigate
/// the window, or load anything from the network.
pub fn sanitize_document_html(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'<' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'<' {
                i += 1;
            }
            out.push_str(&html[start..i]);
            continue;
        }

        // Comments and `<!doctype>` / `<?pi?>` are dropped: they carry no
        // document content and are a classic parser-confusion vector.
        if html[i..].starts_with("<!--") {
            i = match html[i + 4..].find("-->") {
                Some(off) => i + 4 + off + 3,
                None => bytes.len(),
            };
            continue;
        }
        if matches!(bytes.get(i + 1), Some(b'!') | Some(b'?')) {
            i = match html[i..].find('>') {
                Some(off) => i + off + 1,
                None => bytes.len(),
            };
            continue;
        }

        let Some(tag) = parse_tag(html, i) else {
            // A `<` that starts no tag — including an unterminated `<script`
            // at the very end of the document — is plain text.
            out.push_str("&lt;");
            i += 1;
            continue;
        };

        i = tag.end;
        let lower = tag.name.to_ascii_lowercase();

        if tag.is_end {
            if !DROP_WITH_CONTENT.contains(&lower.as_str())
                && !DROP_TAG_ONLY.contains(&lower.as_str())
            {
                out.push_str("</");
                out.push_str(tag.name);
                out.push('>');
            }
            continue;
        }

        if DROP_WITH_CONTENT.contains(&lower.as_str()) {
            // `<script/>` does not self-close in HTML, so the content is
            // skipped in every case.
            i = skip_raw_text(html, i, &lower).1;
            continue;
        }
        if DROP_TAG_ONLY.contains(&lower.as_str()) {
            continue;
        }

        render_tag(&tag, &mut out);

        if RAW_TEXT.contains(&lower.as_str()) && !tag.self_closing {
            let (content, next) = skip_raw_text(html, i, &lower);
            out.push_str(content);
            out.push_str("</");
            out.push_str(tag.name);
            out.push('>');
            i = next;
        }
    }

    out
}

struct Attr<'a> {
    name: &'a str,
    value: Option<&'a str>,
}

struct Tag<'a> {
    name: &'a str,
    is_end: bool,
    self_closing: bool,
    attrs: Vec<Attr<'a>>,
    /// Index just past the closing `>`.
    end: usize,
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.')
}

/// Parse the tag starting at `start` (which must be a `<`).
///
/// Returns `None` when what follows is not a tag, or when the tag is never
/// closed — both are then treated as text by the caller.
fn parse_tag(html: &str, start: usize) -> Option<Tag<'_>> {
    let bytes = html.as_bytes();
    let mut i = start + 1;
    let is_end = bytes.get(i) == Some(&b'/');
    if is_end {
        i += 1;
    }

    if !is_name_start(*bytes.get(i)?) {
        return None;
    }
    let name_start = i;
    while i < bytes.len() && is_name_char(bytes[i]) {
        i += 1;
    }
    let name = &html[name_start..i];

    let mut attrs = Vec::new();
    let mut self_closing = false;

    loop {
        while i < bytes.len() && is_space(bytes[i]) {
            i += 1;
        }
        match bytes.get(i) {
            None => return None, // unterminated tag
            Some(b'>') => {
                i += 1;
                break;
            }
            Some(b'/') => {
                // `/` only self-closes when it is followed by `>`; anywhere
                // else it is a stray character inside the tag.
                if bytes.get(i + 1) == Some(&b'>') {
                    self_closing = true;
                    i += 2;
                    break;
                }
                i += 1;
                continue;
            }
            Some(_) => {}
        }

        let attr_start = i;
        while i < bytes.len() && !is_space(bytes[i]) && !matches!(bytes[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        if i == attr_start {
            // Nothing consumed (a lone `=`, say): skip it to stay in step.
            i += 1;
            continue;
        }
        let attr_name = &html[attr_start..i];

        while i < bytes.len() && is_space(bytes[i]) {
            i += 1;
        }
        let mut value = None;
        if bytes.get(i) == Some(&b'=') {
            i += 1;
            while i < bytes.len() && is_space(bytes[i]) {
                i += 1;
            }
            match bytes.get(i) {
                Some(&q @ (b'"' | b'\'')) => {
                    i += 1;
                    let value_start = i;
                    while i < bytes.len() && bytes[i] != q {
                        i += 1;
                    }
                    if i >= bytes.len() {
                        return None; // unterminated quoted value
                    }
                    value = Some(&html[value_start..i]);
                    i += 1;
                }
                Some(_) => {
                    let value_start = i;
                    while i < bytes.len() && !is_space(bytes[i]) && bytes[i] != b'>' {
                        i += 1;
                    }
                    value = Some(&html[value_start..i]);
                }
                None => return None,
            }
        }

        attrs.push(Attr {
            name: attr_name,
            value,
        });
    }

    Some(Tag {
        name,
        is_end,
        self_closing,
        attrs,
        end: i,
    })
}

/// Content of a raw-text element, and the index just past its end tag.
fn skip_raw_text<'a>(html: &'a str, from: usize, name: &str) -> (&'a str, usize) {
    let needle = format!("</{}", name);
    let haystack = html[from..].to_ascii_lowercase();
    match haystack.find(&needle) {
        Some(off) => {
            let close = from + off;
            let after = match html[close..].find('>') {
                Some(g) => close + g + 1,
                None => html.len(),
            };
            (&html[from..close], after)
        }
        // Never closed: the browser would swallow the rest of the document too.
        None => (&html[from..], html.len()),
    }
}

fn render_tag(tag: &Tag<'_>, out: &mut String) {
    out.push('<');
    out.push_str(tag.name);
    for attr in &tag.attrs {
        if drops_attribute(&attr.name.to_ascii_lowercase(), attr.value) {
            continue;
        }
        out.push(' ');
        out.push_str(attr.name);
        if let Some(value) = attr.value {
            out.push_str("=\"");
            out.push_str(&value.replace('"', "&quot;"));
            out.push('"');
        }
    }
    // Self-closing markers matter inside inline SVG: without them the HTML
    // parser would nest every following element inside `<path>` & co.
    if tag.self_closing {
        out.push_str(" /");
    }
    out.push('>');
}

fn drops_attribute(lower_name: &str, value: Option<&str>) -> bool {
    // Every event handler, whatever the element: `onclick`, `onerror`, and the
    // long tail nobody enumerates correctly.
    if lower_name.starts_with("on") {
        return true;
    }
    if ALWAYS_DROP_ATTRS.contains(&lower_name) {
        return true;
    }
    let Some(value) = value else {
        return false;
    };
    if lower_name == "srcset" {
        return value
            .split(',')
            .any(|candidate| is_dangerous_url(candidate.split_whitespace().next().unwrap_or("")));
    }
    URL_ATTRS.contains(&lower_name) && is_dangerous_url(value)
}

/// Whether a URL would let the document run code or load an HTML page.
fn is_dangerous_url(value: &str) -> bool {
    // A scheme cannot be long; looking at the head keeps this cheap even when
    // the value is a multi-megabyte inlined image.
    let cut = value
        .char_indices()
        .nth(256)
        .map(|(idx, _)| idx)
        .unwrap_or(value.len());
    let decoded = decode_entities(&value[..cut]);
    // Browsers ignore whitespace and control characters inside a URL, so
    // `java\tscript:` is a working payload unless they are removed first.
    let cleaned: String = decoded
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    let lower = cleaned.to_ascii_lowercase();

    if lower.starts_with("javascript:") || lower.starts_with("vbscript:") {
        return true;
    }
    if lower.starts_with("data:") {
        // Images are inlined as `data:` by mdr itself; nothing else is needed.
        return !lower.starts_with("data:image/");
    }
    false
}

/// Decode the character references a browser would decode in an attribute
/// value. Numeric references are handled in full; named ones are limited to
/// those useful to hide a scheme.
fn decode_entities(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'&' {
                i += 1;
            }
            out.push_str(&value[start..i]);
            continue;
        }
        let mut j = i + 1;
        let decoded = if bytes.get(j) == Some(&b'#') {
            j += 1;
            let hex = matches!(bytes.get(j), Some(b'x') | Some(b'X'));
            if hex {
                j += 1;
            }
            let digits_start = j;
            while j < bytes.len()
                && ((hex && bytes[j].is_ascii_hexdigit()) || (!hex && bytes[j].is_ascii_digit()))
            {
                j += 1;
            }
            let digits = &value[digits_start..j];
            if digits.is_empty() {
                None
            } else {
                u32::from_str_radix(digits, if hex { 16 } else { 10 })
                    .ok()
                    .and_then(char::from_u32)
            }
        } else {
            let name_start = j;
            while j < bytes.len() && bytes[j].is_ascii_alphanumeric() && j - name_start < 32 {
                j += 1;
            }
            named_entity(&value[name_start..j])
        };

        match decoded {
            Some(c) => {
                out.push(c);
                // The trailing `;` is optional in attribute values.
                if bytes.get(j) == Some(&b';') {
                    j += 1;
                }
                i = j;
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

fn named_entity(name: &str) -> Option<char> {
    match name.to_ascii_lowercase().as_str() {
        "colon" => Some(':'),
        "tab" => Some('\t'),
        "newline" => Some('\n'),
        "lpar" => Some('('),
        "rpar" => Some(')'),
        "sol" => Some('/'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "nbsp" => Some('\u{a0}'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- scripts ---

    #[test]
    fn a_script_element_and_its_content_are_removed() {
        let out = sanitize_document_html(r#"<p>a</p><script>alert(1)</script><p>b</p>"#);
        assert!(!out.contains("script"), "{out}");
        assert!(!out.contains("alert(1)"), "{out}");
        assert!(
            out.contains("<p>a</p>") && out.contains("<p>b</p>"),
            "{out}"
        );
    }

    #[test]
    fn script_detection_ignores_case_and_attributes() {
        let out = sanitize_document_html(r#"<ScRiPt TYPE="text/javascript">alert(1)</ScRiPt>ok"#);
        assert!(!out.to_lowercase().contains("script"), "{out}");
        assert!(!out.contains("alert"), "{out}");
        assert!(out.contains("ok"), "{out}");
    }

    #[test]
    fn an_unclosed_script_swallows_nothing_and_leaks_nothing() {
        let out = sanitize_document_html("<p>text</p><script>alert(1)");
        assert!(!out.contains("alert(1)"), "{out}");
        assert!(out.contains("<p>text</p>"), "{out}");
    }

    #[test]
    fn a_bare_lower_than_at_the_end_of_the_document_is_escaped() {
        let out = sanitize_document_html("a < b and <script");
        assert!(!out.contains("<script"), "{out}");
        assert!(out.contains("a &lt; b"), "{out}");
    }

    #[test]
    fn a_script_hidden_inside_inline_svg_is_removed() {
        let out = sanitize_document_html(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script><circle r="1"/></svg>"#,
        );
        assert!(!out.contains("alert(1)"), "{out}");
        assert!(out.contains("<circle r=\"1\" />"), "{out}");
    }

    // --- event handlers ---

    #[test]
    fn event_handler_attributes_are_removed() {
        let cases = [
            r#"<img src="a.png" onerror="alert(1)">"#,
            r#"<img src="a.png" ONERROR=alert(1)>"#,
            "<img src=\"a.png\"\n  onerror\n  =\n  'alert(1)'>",
            r#"<div onclick="alert(1)">x</div>"#,
            r#"<body onload=alert(1)>"#,
        ];
        for case in cases {
            let out = sanitize_document_html(case);
            assert!(
                !out.to_lowercase().contains("onerror")
                    && !out.to_lowercase().contains("onclick")
                    && !out.to_lowercase().contains("onload"),
                "{case} → {out}"
            );
            assert!(!out.contains("alert(1)"), "{case} → {out}");
        }
    }

    #[test]
    fn harmless_attributes_survive_next_to_a_removed_handler() {
        let out = sanitize_document_html(r#"<img src="a.png" onerror="x" alt="a picture">"#);
        assert!(out.contains(r#"src="a.png""#), "{out}");
        assert!(out.contains(r#"alt="a picture""#), "{out}");
    }

    // --- URLs ---

    #[test]
    fn javascript_urls_are_neutralised() {
        let cases = [
            r#"<a href="javascript:alert(1)">x</a>"#,
            r#"<a href="JaVaScRiPt:alert(1)">x</a>"#,
            r#"<a href=" javascript:alert(1)">x</a>"#,
            "<a href=\"java\tscript:alert(1)\">x</a>",
            r#"<a href="&#106;avascript:alert(1)">x</a>"#,
            r#"<a href="java&Tab;script:alert(1)">x</a>"#,
            r#"<a href="&#x6a;avascript&colon;alert(1)">x</a>"#,
            r#"<a href=javascript:alert(1)>x</a>"#,
        ];
        for case in cases {
            let out = sanitize_document_html(case);
            assert!(!out.contains("href"), "{case} → {out}");
            assert!(out.contains(">x</a>"), "{case} → {out}");
        }
    }

    #[test]
    fn vbscript_and_html_data_urls_are_neutralised() {
        for case in [
            r#"<a href="vbscript:msgbox(1)">x</a>"#,
            r#"<a href="data:text/html;base64,PHNjcmlwdD4=">x</a>"#,
            r#"<iframe src="data:text/html,<script>alert(1)</script>"></iframe>"#,
        ] {
            let out = sanitize_document_html(case);
            assert!(!out.contains("vbscript"), "{case} → {out}");
            assert!(!out.contains("data:text/html"), "{case} → {out}");
        }
    }

    #[test]
    fn xlink_href_is_checked_too() {
        let out = sanitize_document_html(
            r#"<svg><a xlink:href="javascript:alert(1)"><text>x</text></a></svg>"#,
        );
        assert!(!out.contains("javascript"), "{out}");
        assert!(out.contains("<text>x</text>"), "{out}");
    }

    #[test]
    fn ordinary_and_inlined_urls_are_left_alone() {
        let html = r#"<a href="https://example.com/a?b=1&amp;c=2">x</a>"#;
        assert!(sanitize_document_html(html).contains("https://example.com/a?b=1&amp;c=2"));

        let img = r#"<img src="data:image/png;base64,iVBORw0KGgo=" alt="a" />"#;
        assert!(sanitize_document_html(img).contains("data:image/png;base64,iVBORw0KGgo="));

        let anchor = r##"<a href="#section-1">x</a>"##;
        assert!(sanitize_document_html(anchor).contains(r##"href="#section-1""##));

        let relative = r#"<a href="other.md">x</a>"#;
        assert!(sanitize_document_html(relative).contains(r#"href="other.md""#));
    }

    #[test]
    fn srcdoc_never_survives() {
        let out = sanitize_document_html(r#"<div srcdoc="<script>alert(1)</script>">x</div>"#);
        assert!(!out.contains("srcdoc"), "{out}");
    }

    // --- dropped elements ---

    #[test]
    fn navigating_and_embedding_elements_are_dropped_but_their_text_is_kept() {
        let out = sanitize_document_html(
            r#"<base href="http://evil/"><form action="http://evil/"><p>keep me</p></form><link rel="stylesheet" href="http://evil/x.css"><meta http-equiv="refresh" content="0;url=http://evil/"><object data="x.swf"></object><embed src="x.swf"><iframe src="http://evil/"></iframe>"#,
        );
        for forbidden in [
            "<base", "<form", "</form", "<link", "<meta", "<object", "<embed", "<iframe",
        ] {
            assert!(!out.contains(forbidden), "{forbidden} survived: {out}");
        }
        assert!(out.contains("<p>keep me</p>"), "{out}");
        assert!(!out.contains("evil"), "{out}");
    }

    #[test]
    fn comments_are_dropped() {
        let out = sanitize_document_html("<p>a</p><!-- <script>alert(1)</script> --><p>b</p>");
        assert!(!out.contains("alert(1)"), "{out}");
        assert!(
            out.contains("<p>a</p>") && out.contains("<p>b</p>"),
            "{out}"
        );
    }

    // --- what must keep working ---

    #[test]
    fn ordinary_markdown_output_is_preserved() {
        let html = concat!(
            r#"<h1 id="title">Title</h1><p>Some <em>text</em> &amp; entities.</p>"#,
            r#"<table><tr><td>a</td></tr></table>"#,
            r#"<pre><code class="language-rust">fn main() {}</code></pre>"#,
            r#"<ul><li><input type="checkbox" checked="" disabled="" /> done</li></ul>"#,
        );
        let out = sanitize_document_html(html);
        assert!(out.contains(r#"<h1 id="title">Title</h1>"#), "{out}");
        assert!(out.contains("<em>text</em>"), "{out}");
        assert!(out.contains("&amp; entities."), "{out}");
        assert!(out.contains(r#"<code class="language-rust">"#), "{out}");
        assert!(out.contains("<td>a</td>"), "{out}");
        assert!(out.contains(r#"<input type="checkbox""#), "{out}");
    }

    #[test]
    fn inline_svg_keeps_its_structure_and_self_closing_tags() {
        let svg = concat!(
            r#"<div class="mermaid-diagram"><svg xmlns="http://www.w3.org/2000/svg" "#,
            r#"viewBox="0 0 100 50" width="100" height="50">"#,
            r#"<style>.node{fill:#fff}</style>"#,
            r#"<defs><marker id="arrow"><path d="M 0,0 L 10,5 L 0,10 z"/></marker></defs>"#,
            r#"<g class="node"><rect x="1" y="2" width="8" height="4"/>"#,
            r#"<text x="5" y="5">A</text></g></svg></div>"#,
        );
        let out = sanitize_document_html(svg);
        assert!(out.contains(r#"viewBox="0 0 100 50""#), "{out}");
        assert!(
            out.contains(r#"<path d="M 0,0 L 10,5 L 0,10 z" />"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<rect x="1" y="2" width="8" height="4" />"#),
            "{out}"
        );
        assert!(out.contains("<text x=\"5\" y=\"5\">A</text>"), "{out}");
        assert!(out.contains(".node{fill:#fff}"), "{out}");
        assert!(out.contains("</svg></div>"), "{out}");
    }

    #[test]
    fn a_style_element_is_copied_verbatim() {
        let out = sanitize_document_html("<style>a::after{content:'<'}</style><p>x</p>");
        assert!(
            out.contains("<style>a::after{content:'<'}</style>"),
            "{out}"
        );
        assert!(out.contains("<p>x</p>"), "{out}");
    }

    #[test]
    fn a_script_after_a_style_is_still_removed() {
        let out = sanitize_document_html("<style>body{}</style><script>alert(1)</script>ok");
        assert!(!out.contains("alert(1)"), "{out}");
        assert!(out.contains("ok"), "{out}");
    }

    #[test]
    fn a_quoted_greater_than_does_not_end_the_tag() {
        let out = sanitize_document_html(r#"<a href="/a>b" title="x>y">link</a>"#);
        assert!(out.contains(r#"href="/a>b""#), "{out}");
        assert!(out.contains(">link</a>"), "{out}");
    }

    #[test]
    fn sanitising_twice_changes_nothing_more() {
        let html = concat!(
            r#"<p>a</p><script>alert(1)</script><img src="a.png" onerror="x" />"#,
            r#"<svg><path d="M0 0"/></svg><a href="https://example.com">l</a>"#,
        );
        let once = sanitize_document_html(html);
        assert_eq!(sanitize_document_html(&once), once);
    }

    #[test]
    fn a_real_mermaid_diagram_survives() {
        // Not a hand-written fixture: the actual output of the renderer, so a
        // change in the SVG it emits would be caught here.
        let md = "```mermaid\ngraph LR\n  A[Start] --> B{Choice}\n  B --> C[End]\n```";
        let rendered = crate::core::markdown::parse_markdown(md);
        let out = sanitize_document_html(&rendered);

        assert!(out.contains(r#"<div class="mermaid-diagram">"#), "{out}");
        if rendered.contains("<svg") {
            assert!(
                out.contains(r#"<svg xmlns="http://www.w3.org/2000/svg""#),
                "{out}"
            );
            assert!(out.contains("</svg></div>"), "{out}");
            assert!(out.contains("viewBox="), "{out}");
            assert!(out.contains("<path "), "{out}");
            // Self-closing markers matter: without them the HTML parser nests
            // every following element inside `<path>`.
            assert!(out.contains(" />"), "self-closing markers lost: {out}");
        } else {
            // The native renderer declined; the JS fallback must survive too.
            assert!(out.contains(r#"class="mermaid""#), "{out}");
        }
    }

    #[test]
    fn the_documents_of_this_project_come_through_undamaged() {
        // End-to-end guard: real Markdown, rendered the way mdr renders it.
        // Only the tag counts are compared, so this cannot fail for a cosmetic
        // difference — but it does catch a filter eating legitimate markup.
        for name in ["README.md", "SPEC.md", "CHANGELOG.md", "test.md"] {
            let Ok(md) = std::fs::read_to_string(name) else {
                continue;
            };
            let before = crate::core::markdown::parse_markdown(&md);
            let after = sanitize_document_html(&before);
            for tag in ["<a ", "<img", "<h1", "<h2", "<code", "<li", "<table", "<em"] {
                let b = before.matches(tag).count();
                let a = after.matches(tag).count();
                assert_eq!(b, a, "{name}: {tag} went from {b} to {a}");
            }
        }
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(sanitize_document_html(""), "");
    }
}
