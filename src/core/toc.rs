use comrak::{parse_document, Arena, Options};
use comrak::nodes::NodeValue;

#[derive(Debug, Clone)]
pub struct TocEntry {
    pub level: u8,
    pub text: String,
    pub anchor: String,
}

/// Extract table of contents entries from markdown content.
pub fn extract_toc(content: &str) -> Vec<TocEntry> {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;

    let root = parse_document(&arena, content, &options);
    let mut entries = Vec::new();

    for node in root.descendants() {
        if let NodeValue::Heading(heading) = &node.data.borrow().value {
            let level = heading.level;
            let text = collect_text(node);
            let anchor = slugify(&text);
            entries.push(TocEntry { level, text, anchor });
        }
    }

    entries
}

/// Collect all text content from a node and its children.
fn collect_text<'a>(node: &'a comrak::arena_tree::Node<'a, std::cell::RefCell<comrak::nodes::Ast>>) -> String {
    let mut text = String::new();
    for child in node.descendants() {
        if let NodeValue::Text(ref t) = child.data.borrow().value {
            text.push_str(t);
        }
        if let NodeValue::Code(ref c) = child.data.borrow().value {
            text.push_str(&c.literal);
        }
    }
    text
}

/// Convert a heading text to a URL-friendly slug.
fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else if c == ' ' { '-' } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("")
}
