//! Narrow system-clipboard support for the composer.
//!
//! Terminal bracketed-paste events remain the normal text path. `Ctrl+V` uses this module so an
//! image clipboard can be encoded as PNG; when there is no image, clipboard text is returned.

use std::borrow::Cow;
use std::io::Cursor;

/// Content read from the system clipboard, with images preferred over text like Codex's TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    ImagePng(Vec<u8>),
    Text(String),
}

/// Read one composer paste from the native clipboard.
#[cfg(not(target_os = "android"))]
pub fn read() -> Result<ClipboardContent, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("clipboard unavailable: {error}"))?;

    // Finder, Explorer, and some Linux file managers expose a copied image as a file list rather
    // than raw pixels. Codex prefers that representation too.
    if let Some(image) = clipboard
        .get()
        .file_list()
        .unwrap_or_default()
        .into_iter()
        .find_map(|path| image::open(path).ok())
    {
        return encode_dynamic_image(image);
    }

    match clipboard.get_image() {
        Ok(image) => encode_png(image.width, image.height, image.bytes),
        Err(image_error) => {
            clipboard
                .get_text()
                .map(ClipboardContent::Text)
                .map_err(|text_error| {
                    format!("clipboard has no image or text: {image_error}; {text_error}")
                })
        }
    }
}

#[cfg(not(target_os = "android"))]
fn encode_dynamic_image(image: image::DynamicImage) -> Result<ClipboardContent, String> {
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| format!("could not encode clipboard image: {error}"))?;
    Ok(ClipboardContent::ImagePng(png))
}

#[cfg(not(target_os = "android"))]
fn encode_png(
    width: usize,
    height: usize,
    bytes: Cow<'_, [u8]>,
) -> Result<ClipboardContent, String> {
    let width = u32::try_from(width).map_err(|_| "clipboard image is too wide".to_owned())?;
    let height = u32::try_from(height).map_err(|_| "clipboard image is too tall".to_owned())?;
    let rgba = image::RgbaImage::from_raw(width, height, bytes.into_owned())
        .ok_or_else(|| "clipboard image has an invalid RGBA buffer".to_owned())?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| format!("could not encode clipboard image: {error}"))?;
    Ok(ClipboardContent::ImagePng(png))
}

#[cfg(target_os = "android")]
pub fn read() -> Result<ClipboardContent, String> {
    Err("clipboard paste is unsupported on Android".to_owned())
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;

    #[test]
    fn rgba_clipboard_pixels_are_encoded_as_a_real_png() {
        let ClipboardContent::ImagePng(png) =
            encode_png(1, 1, Cow::Borrowed(&[255, 0, 0, 255])).unwrap()
        else {
            panic!("an image is returned");
        };
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1, 1));
    }
}
