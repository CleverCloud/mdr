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
        let data = std::fs::read(path).map_err(|e| format!("cannot read file: {}", e))?;
        if data.len() < magic.len() {
            return Err(format!("file too small to be a valid {}", kind));
        }
        if &data[..magic.len()] != magic {
            // WebP is RIFF + 4 bytes + WEBP
            if kind == "WebP" && data.len() >= 12 {
                if &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
                    return Ok(());
                }
            }
            return Err(format!(
                "file does not appear to be a valid {} (wrong magic bytes)",
                kind
            ));
        }
    }

    Ok(())
}

fn validate_svg(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read file: {}", e))?;
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
