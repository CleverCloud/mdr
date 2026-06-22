use comrak::nodes::NodeValue;
use comrak::{parse_document, Arena, Options};

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
            entries.push(TocEntry {
                level,
                text,
                anchor,
            });
        }
    }

    entries
}

/// Collect all text content from a node and its children.
fn collect_text<'a>(
    node: &'a comrak::arena_tree::Node<'a, std::cell::RefCell<comrak::nodes::Ast>>,
) -> String {
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
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else if c == ' ' {
                '-'
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- slugify tests ---

    #[test]
    fn slugify_simple_text() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn slugify_preserves_hyphens_and_underscores() {
        assert_eq!(slugify("my-heading_here"), "my-heading_here");
    }

    #[test]
    fn slugify_strips_special_characters() {
        assert_eq!(slugify("Hello, World! (2024)"), "hello-world-2024");
    }

    #[test]
    fn slugify_multiple_spaces_become_multiple_hyphens() {
        // Each space maps to a hyphen; hyphens are kept as-is (alphanumeric-like),
        // so multiple spaces produce multiple hyphens.
        assert_eq!(slugify("hello   world"), "hello---world");
    }

    #[test]
    fn slugify_empty_string() {
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn slugify_only_special_chars() {
        assert_eq!(slugify("!@#$%"), "");
    }

    #[test]
    fn slugify_unicode_alphanumeric() {
        // Unicode alphanumeric chars are preserved (lowercased)
        let result = slugify("Café Résumé");
        assert!(result.contains("café"));
        assert!(result.contains("résumé"));
    }

    #[test]
    fn slugify_numbers() {
        assert_eq!(slugify("Chapter 1"), "chapter-1");
    }

    // --- extract_toc tests ---

    #[test]
    fn extract_toc_empty_input() {
        let entries = extract_toc("");
        assert!(entries.is_empty());
    }

    #[test]
    fn extract_toc_no_headings() {
        let entries = extract_toc("Just some paragraph text.\n\nAnother paragraph.");
        assert!(entries.is_empty());
    }

    #[test]
    fn extract_toc_single_h1() {
        let entries = extract_toc("# Hello World");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, 1);
        assert_eq!(entries[0].text, "Hello World");
        assert_eq!(entries[0].anchor, "hello-world");
    }

    #[test]
    fn extract_toc_multiple_levels() {
        let md = "# Title\n## Section\n### Subsection\n#### Deep";
        let entries = extract_toc(md);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].level, 1);
        assert_eq!(entries[1].level, 2);
        assert_eq!(entries[2].level, 3);
        assert_eq!(entries[3].level, 4);
    }

    #[test]
    fn extract_toc_heading_with_inline_code() {
        let md = "# The `main` function";
        let entries = extract_toc(md);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "The main function");
    }

    #[test]
    fn extract_toc_heading_with_special_chars() {
        let md = "## Hello, World! (2024)";
        let entries = extract_toc(md);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].anchor, "hello-world-2024");
    }

    #[test]
    fn extract_toc_mixed_content_and_headings() {
        let md = "Some intro text.\n\n# First\n\nParagraph here.\n\n## Second\n\nMore text.";
        let entries = extract_toc(md);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "First");
        assert_eq!(entries[1].text, "Second");
    }

    #[test]
    fn extract_toc_h5_and_h6() {
        let md = "##### Level 5\n###### Level 6";
        let entries = extract_toc(md);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].level, 5);
        assert_eq!(entries[1].level, 6);
    }

    #[test]
    fn extract_toc_preserves_order() {
        let md = "## B\n# A\n### C";
        let entries = extract_toc(md);
        assert_eq!(entries[0].text, "B");
        assert_eq!(entries[1].text, "A");
        assert_eq!(entries[2].text, "C");
    }
}
