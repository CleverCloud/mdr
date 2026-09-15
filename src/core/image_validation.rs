use std::path::Path;

/// Validate that a file's content matches its image extension by checking magic bytes.
/// Returns an error message string if the file appears to be an invalid or mislabeled image.
pub fn validate_image_file(path: &Path) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Only validate known image extensions
    let expected = match ext.as_str() {
        "png" => Some((&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A][..], "PNG")),
        "jpg" | "jpeg" => Some((&[0xFF, 0xD8, 0xFF][..], "JPEG")),
        "gif" => Some((&b"GIF"[..], "GIF")),
        "webp" => Some((&b"RIFF"[..], "WebP")),
        "svg" => return validate_svg(path),
        "bmp" => Some((&b"BM"[..], "BMP")),
        _ => None,
    };

    if let Some((magic, kind)) = expected {
        let data = std::fs::read(path).map_err(|e| format!("cannot read file: {e}"))?;
        if data.len() < magic.len() {
            return Err(format!("file too small to be a valid {kind}"));
        }

        let matches = if kind == "WebP" {
            // RIFF is only the container: `.wav` and `.avi` start with it too.
            // The format itself is named at bytes 8..12, and this used to be
            // checked inside the `!= magic` arm — that is, only when the file
            // did *not* start with RIFF, which cannot happen. The four-byte
            // check passed on its own and any RIFF file named `.webp` went
            // through unexamined.
            data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP"
        } else {
            &data[..magic.len()] == magic
        };

        if !matches {
            return Err(format!(
                "file does not appear to be a valid {kind} (wrong magic bytes)"
            ));
        }
    }

    Ok(())
}

fn validate_svg(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read file: {e}"))?;
    let trimmed = text.trim_start();
    if trimmed.starts_with("<svg")
        || trimmed.starts_with("<?xml")
        || trimmed.starts_with("<!DOCTYPE svg")
    {
        return Ok(());
    }
    if trimmed.starts_with("<!") && trimmed.contains("<svg") {
        // Allow DOCTYPE followed by svg
        return Ok(());
    }
    Err("file is not a valid SVG (possibly an HTML page saved with .svg extension)".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    /// `RIFF` + a size + the format name. Only the last part says WebP.
    fn riff(format: &[u8; 4]) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(format);
        bytes
    }

    #[test]
    fn a_webp_is_recognised_by_its_format_name_not_just_its_container() {
        let (_dir, path) = write("image.webp", &riff(b"WEBP"));
        assert!(validate_image_file(&path).is_ok(), "a real WebP must pass");
    }

    #[test]
    fn another_riff_format_named_webp_is_refused() {
        // The container is shared with WAV and AVI. This used to pass: the
        // format check sat in a branch that could not be reached.
        for format in [b"WAVE", b"AVI "] {
            let (_dir, path) = write("image.webp", &riff(format));
            let err = validate_image_file(&path)
                .expect_err("only WEBP may pass as a WebP, got a pass for {format:?}");
            assert!(
                err.contains("WebP"),
                "the error should name the format: {err}"
            );
        }
    }

    #[test]
    fn a_webp_truncated_before_its_format_name_is_refused() {
        let (_dir, path) = write("image.webp", b"RIFF\0\0\0\0");
        assert!(validate_image_file(&path).is_err());
    }

    use std::io::Write;

    #[test]
    fn valid_png_passes() {
        let tmp = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        tmp.as_file()
            .write_all(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00])
            .unwrap();
        assert!(validate_image_file(tmp.path()).is_ok());
    }

    #[test]
    fn html_saved_as_svg_fails() {
        let tmp = tempfile::NamedTempFile::with_suffix(".svg").unwrap();
        tmp.as_file()
            .write_all(b"<!DOCTYPE html><html></html>")
            .unwrap();
        assert!(validate_image_file(tmp.path()).is_err());
    }

    #[test]
    fn valid_svg_passes() {
        let tmp = tempfile::NamedTempFile::with_suffix(".svg").unwrap();
        tmp.as_file()
            .write_all(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>")
            .unwrap();
        assert!(validate_image_file(tmp.path()).is_ok());
    }

    #[test]
    fn wrong_magic_bytes_fails() {
        let tmp = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        tmp.as_file().write_all(b"NOTAPNG").unwrap();
        assert!(validate_image_file(tmp.path()).is_err());
    }
}
