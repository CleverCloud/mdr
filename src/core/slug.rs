//! Heading anchor generation, shared by the HTML renderer and the table of
//! contents so that both always produce exactly the same anchors.

/// Convert a heading text to a URL-friendly slug.
///
/// This is the raw slug, before de-duplication. Callers that walk a whole
/// document should use [`SlugGenerator`] instead, so repeated headings get
/// distinct anchors.
pub fn slugify(text: &str) -> String {
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

/// Hands out unique anchors for the headings of a single document, in document
/// order. Repeated heading texts get a `-1`, `-2`, … suffix, as GitHub does.
///
/// Both the HTML renderer and the TOC extractor walk the headings in the same
/// order, so feeding each of them its own generator yields identical anchors.
#[derive(Default)]
pub struct SlugGenerator {
    seen: std::collections::HashMap<String, usize>,
}

impl SlugGenerator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a unique anchor for `text`.
    pub fn generate(&mut self, text: &str) -> String {
        let base = slugify(text);
        // A heading whose text slugifies to nothing still needs an anchor to be
        // reachable; `section` mirrors what the TOC displays for it.
        let base = if base.is_empty() {
            "section".to_string()
        } else {
            base
        };

        let Some(&last) = self.seen.get(&base) else {
            self.seen.insert(base.clone(), 0);
            return base;
        };

        // Keep bumping until we land on an anchor no heading has taken: a
        // document may legitimately contain both `Setup` twice and an explicit
        // `Setup 1`.
        let mut count = last;
        loop {
            count += 1;
            let candidate = format!("{}-{}", base, count);
            if !self.seen.contains_key(&candidate) {
                self.seen.insert(base, count);
                self.seen.insert(candidate.clone(), 0);
                return candidate;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_simple_text() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn slugify_strips_special_characters() {
        assert_eq!(slugify("Hello, World! (2024)"), "hello-world-2024");
    }

    #[test]
    fn first_occurrence_keeps_the_plain_slug() {
        let mut gen = SlugGenerator::new();
        assert_eq!(gen.generate("Setup"), "setup");
    }

    #[test]
    fn duplicate_headings_get_distinct_anchors() {
        let mut gen = SlugGenerator::new();
        assert_eq!(gen.generate("Setup"), "setup");
        assert_eq!(gen.generate("Setup"), "setup-1");
        assert_eq!(gen.generate("Setup"), "setup-2");
    }

    #[test]
    fn suffix_collision_with_an_explicit_heading_is_avoided() {
        let mut gen = SlugGenerator::new();
        assert_eq!(gen.generate("Setup"), "setup");
        assert_eq!(gen.generate("Setup 1"), "setup-1");
        // `setup-1` is taken by the explicit heading, so the duplicate skips it.
        assert_eq!(gen.generate("Setup"), "setup-2");
    }

    #[test]
    fn headings_with_no_slugifiable_text_still_get_anchors() {
        let mut gen = SlugGenerator::new();
        assert_eq!(gen.generate("!!!"), "section");
        assert_eq!(gen.generate("???"), "section-1");
    }

    #[test]
    fn different_headings_do_not_interfere() {
        let mut gen = SlugGenerator::new();
        assert_eq!(gen.generate("Install"), "install");
        assert_eq!(gen.generate("Usage"), "usage");
        assert_eq!(gen.generate("Install"), "install-1");
    }
}
