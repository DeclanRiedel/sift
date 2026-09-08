//! Bounded first-frame previews for binary result cells.
use std::{io::Cursor, sync::Arc};

pub(crate) fn hex_page(bytes: &[u8], page: usize) -> String {
    let start = page.min(bytes.len().saturating_sub(1) / 4096) * 4096;
    let end = (start + 4096).min(bytes.len());
    let mut dump = format!("{} bytes · offsets {start}–{end}\n\n", bytes.len());
    for (offset, chunk) in bytes[start..end].chunks(16).enumerate() {
        let hex = chunk
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let ascii = chunk
            .iter()
            .map(|byte| {
                if byte.is_ascii_graphic() || *byte == b' ' {
                    char::from(*byte)
                } else {
                    '.'
                }
            })
            .collect::<String>();
        dump.push_str(&format!(
            "{:08x}  {hex:<47}  |{ascii}|\n",
            start + offset * 16
        ));
    }
    dump
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Arc<gpui::RenderImage>, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Image preview limited to 16 MiB; hex remains available".into());
    }
    let format = image::guess_format(bytes)
        .map_err(|_| "Binary value has no supported image preview".to_string())?;
    if !matches!(
        format,
        image::ImageFormat::Png
            | image::ImageFormat::Jpeg
            | image::ImageFormat::Gif
            | image::ImageFormat::WebP
    ) {
        return Err("Preview supports PNG, JPEG, GIF, and WebP".into());
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut pixels = reader
        .decode()
        .map_err(|_| "Invalid image or preview exceeds 4096×4096 / 64 MiB limit".to_string())?
        .into_rgba8();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
        pixels,
    )])))
}

pub(crate) struct CachedPreview {
    pub bytes: Arc<Vec<u8>>,
    pub result: Option<Result<Arc<gpui::RenderImage>, String>>,
    pub task: Option<gpui::Task<()>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_pages_cover_tail_and_clamp_empty_or_out_of_range_pages() {
        let mut bytes = vec![0; 4096];
        bytes.extend_from_slice(b"tail");
        assert!(!hex_page(&bytes, 0).contains("tail"));
        assert!(hex_page(&bytes, usize::MAX).contains("00001000  74 61 69 6c"));
        assert!(hex_page(&[], usize::MAX).contains("0 bytes"));
    }

    #[test]
    fn decodes_pixels_and_rejects_invalid_or_oversized_input() {
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([255, 0, 0, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        let rendered = decode(png.get_ref()).unwrap();
        assert_eq!(rendered.as_bytes(0).unwrap(), &[0, 0, 255, 255]);
        assert!(decode(b"\x89PNG\r\n\x1a\ninvalid").is_err());
        assert!(decode(&vec![0; 16 * 1024 * 1024 + 1]).is_err());
    }
}
