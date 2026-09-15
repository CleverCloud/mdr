//! One set of SVG parsing options, shared by everything that rasterises one.
//!
//! Four places built their own — the three backends and the Mermaid renderer —
//! each with its own copy of the system font database, and each with usvg's
//! defaults. Those defaults include an `xlink:href` resolver that treats the
//! attribute as a path and reads it: `ImageHrefResolver::default_string_resolver`
//! calls `std::fs::read`, and `resources_dir` does not bound it. Validating the
//! drawing mdr was asked for therefore said nothing about the files that drawing
//! points at.
//!
//! Here the string resolver refuses every such reference. The data resolver is
//! kept,
//! so a picture embedded in the SVG as a `data:` URI still draws — that is the
//! author's own bytes, already inside the file mdr checked.

use std::sync::{Arc, OnceLock};

/// The system fonts, loaded once for the whole process rather than once per
/// call site.
fn fontdb() -> &'static Arc<usvg::fontdb::Database> {
    static FONTDB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTDB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
}

/// Parsing options that follow no reference of their own.
///
/// They do read the system fonts, once; what they refuse is a reference the
/// drawing itself names.
pub fn options() -> usvg::Options<'static> {
    let mut options = usvg::Options {
        fontdb: Arc::clone(fontdb()),
        ..Default::default()
    };
    options.image_href_resolver = usvg::ImageHrefResolver {
        // Embedded data still draws.
        resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
        // A reference to anything else does not. mdr resolves the images a
        // document asks for itself, with the project root as the boundary; a
        // path reached through an SVG would go around that.
        resolve_string: Box::new(|_href, _options| None),
    };
    options
}

#[cfg(test)]
mod tests {
    /// A 1x1 transparent PNG, decoded from the base64 below.
    const ONE_PIXEL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x60,
        0x00, 0x02, 0x00, 0x00, 0x05, 0x00, 0x01, 0xE2, 0x26, 0x05, 0x9B, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// An SVG that points at a file outside the document must not read it.
    ///
    /// The file here is real and outside any image root the caller declared, so
    /// a resolver that still read it would produce a tree with an image node in
    /// it.
    #[test]
    fn an_svg_cannot_pull_in_a_file_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret.png");
        // A real 1x1 PNG, so the default resolver would have decoded it and
        // produced an image node — which is what makes this test discriminate.
        std::fs::write(&secret, ONE_PIXEL_PNG).unwrap();

        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image xlink:href="{}" width="10" height="10"/></svg>"#,
            secret.display()
        );
        let tree = usvg::Tree::from_str(&svg, &super::options()).expect("a valid SVG");

        fn has_image(group: &usvg::Group) -> bool {
            group.children().iter().any(|node| match node {
                usvg::Node::Image(_) => true,
                usvg::Node::Group(inner) => has_image(inner),
                _ => false,
            })
        }
        assert!(
            !has_image(tree.root()),
            "the referenced file should not have been read"
        );
    }

    /// A picture embedded in the SVG itself still draws: those are the author's
    /// own bytes, inside the file mdr already checked.
    #[test]
    fn an_embedded_picture_still_draws() {
        // A 1x1 transparent PNG.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image xlink:href="data:image/png;base64,{png}" width="10" height="10"/></svg>"#
        );
        let tree = usvg::Tree::from_str(&svg, &super::options()).expect("a valid SVG");

        fn has_image(group: &usvg::Group) -> bool {
            group.children().iter().any(|node| match node {
                usvg::Node::Image(_) => true,
                usvg::Node::Group(inner) => has_image(inner),
                _ => false,
            })
        }
        assert!(has_image(tree.root()), "embedded data should still draw");
    }
}
