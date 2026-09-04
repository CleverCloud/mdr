//! Where mdr is allowed to read images from.

use std::path::{Path, PathBuf};

/// The directory tree images may be loaded from, for a document living in
/// `base_dir`.
///
/// Restricting images to the directory of the Markdown file itself breaks the
/// very common `docs/page.md` → `![](../images/logo.png)` layout (#61), so the
/// root is widened to the enclosing project: the nearest ancestor holding a
/// `.git` directory, or a few other unmistakable project markers. Without any
/// marker the root stays the document's own directory, which is the previous,
/// conservative behaviour.
///
/// The walk never leaves the filesystem, and never returns the user's home
/// directory or the filesystem root, so a stray `../../../etc/passwd` still
/// cannot be read.
pub fn image_root(base_dir: &Path) -> PathBuf {
    let start = base_dir
        .canonicalize()
        .unwrap_or_else(|_| base_dir.to_path_buf());

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|h| h.canonicalize().ok());

    for ancestor in start.ancestors() {
        // Never hand out the home directory or anything above it as a root.
        if home.as_deref() == Some(ancestor) {
            break;
        }
        if ancestor.parent().is_none() {
            break;
        }
        if is_project_root(ancestor) {
            return ancestor.to_path_buf();
        }
    }

    start
}

fn is_project_root(dir: &Path) -> bool {
    const MARKERS: &[&str] = &[".git", ".hg", ".svn", ".jj"];
    MARKERS.iter().any(|m| dir.join(m).exists())
}

/// Whether `candidate` may be read as an image for a document in `base_dir`.
///
/// Both paths are canonicalised by the caller-visible behaviour of
/// [`image_root`], so symlinks pointing outside the project are refused too.
pub fn is_within_image_root(candidate: &Path, base_dir: &Path) -> bool {
    let root = image_root(base_dir);
    match candidate.canonicalize() {
        Ok(canonical) => canonical.starts_with(&root),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_project_marker_the_root_is_the_document_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = tmp.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        assert_eq!(image_root(&docs), docs.canonicalize().unwrap());
    }

    #[test]
    fn a_git_directory_widens_the_root_to_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        std::fs::create_dir_all(proj.join("docs")).unwrap();
        assert_eq!(image_root(&proj.join("docs")), proj.canonicalize().unwrap());
    }

    #[test]
    fn a_parent_directory_image_is_allowed_inside_a_project() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        std::fs::create_dir_all(proj.join("docs")).unwrap();
        std::fs::create_dir_all(proj.join("images")).unwrap();
        let logo = proj.join("images/logo.png");
        std::fs::write(&logo, b"\x89PNG\r\n\x1a\n").unwrap();

        assert!(is_within_image_root(&logo, &proj.join("docs")));
    }

    #[test]
    fn an_image_outside_the_project_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        std::fs::create_dir_all(proj.join("docs")).unwrap();
        let outside = tmp.path().join("secret.png");
        std::fs::write(&outside, b"\x89PNG\r\n\x1a\n").unwrap();

        assert!(!is_within_image_root(&outside, &proj.join("docs")));
    }

    #[test]
    fn a_missing_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_within_image_root(
            &tmp.path().join("nope.png"),
            tmp.path()
        ));
    }
}
