//! Inline image rendering (spec §7.6 "Images").
//!
//! Detect the terminal's graphics capability once at startup — kitty, iTerm2 or sixel via
//! `ratatui-image`, halfblock Unicode everywhere a color terminal exists but no protocol
//! does. When an image event's bytes cannot be decoded at all, render a bordered
//! placeholder with an honest reason instead of failing the whole transcript render.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidget, Widget};
use ratatui_image::StatefulImage;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;

/// Detected once per process (spec §7.6: "Detect once at startup, report in `?`").
#[derive(Debug)]
pub struct ImageSupport {
    picker: Picker,
}

impl ImageSupport {
    /// Query the real terminal for a graphics protocol and font metrics. Any failure —
    /// not a tty, an unrecognized terminal, no response before the query timeout — falls
    /// back to the halfblock renderer, which only needs color support.
    pub fn detect() -> Self {
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        Self { picker }
    }

    /// A picker fixed to halfblocks, for tests and non-interactive contexts where querying
    /// stdio would hang or misbehave.
    pub fn halfblocks() -> Self {
        Self {
            picker: Picker::halfblocks(),
        }
    }

    pub fn protocol_type(&self) -> ProtocolType {
        self.picker.protocol_type()
    }

    /// Decode base64 image bytes into a render-ready protocol, or an honest failure the
    /// caller renders as a placeholder. Never panics on malformed wire data.
    pub fn decode(&self, data_base64: &str) -> Result<DecodedImage, DecodeError> {
        let bytes = BASE64_STANDARD
            .decode(data_base64.trim())
            .map_err(|_| DecodeError::Malformed)?;
        let image = image::load_from_memory(&bytes).map_err(|_| DecodeError::Malformed)?;
        let dimensions = (image.width(), image.height());
        let protocol = self.picker.new_resize_protocol(image);
        Ok(DecodedImage {
            protocol,
            dimensions,
        })
    }
}

/// Why an image item has no pixels to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The base64 payload or the image bytes it decoded to could not be parsed.
    Malformed,
}

impl DecodeError {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Malformed => "image could not be decoded",
        }
    }
}

/// A successfully decoded image, ready to render or re-render at any size.
pub struct DecodedImage {
    protocol: StatefulProtocol,
    pub dimensions: (u32, u32),
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

/// Renders a decoded image, or — honestly, per spec §7.6 — a bordered placeholder
/// carrying whatever we do know (dimensions, or just the failure reason) plus the
/// "open externally" action every image item offers regardless of protocol support.
pub fn render_image(area: Rect, buf: &mut Buffer, style: Style, image: &mut Option<DecodedImage>) {
    match image {
        Some(decoded) => {
            StatefulImage::default().render(area, buf, &mut decoded.protocol);
        }
        None => render_placeholder(area, buf, style, None, "image unavailable"),
    }
}

/// The bordered fallback box: dimensions when known, the reason otherwise, and the
/// externally-open hint every image item carries.
pub fn render_placeholder(
    area: Rect,
    buf: &mut Buffer,
    style: Style,
    dimensions: Option<(u32, u32)>,
    reason: &str,
) {
    let block = Block::default().borders(Borders::ALL).style(style);
    let inner = block.inner(area);
    block.render(area, buf);
    let mut lines = vec![Line::from(Span::styled(reason.to_owned(), style))];
    if let Some((width, height)) = dimensions {
        lines.push(Line::styled(format!("{width}\u{d7}{height}"), style));
    }
    lines.push(Line::styled("o  open externally", style));
    Paragraph::new(lines)
        .alignment(Alignment::Center)
        .render(inner, buf);
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;

    use super::*;

    const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

    #[test]
    fn valid_image_bytes_decode_with_their_pixel_dimensions() {
        let support = ImageSupport::halfblocks();
        let decoded = support.decode(TINY_PNG_BASE64).expect("a 1x1 PNG decodes");
        assert_eq!(decoded.dimensions, (1, 1));
    }

    #[test]
    fn malformed_base64_falls_back_honestly() {
        let support = ImageSupport::halfblocks();
        assert_eq!(
            support.decode("not base64!!").unwrap_err(),
            DecodeError::Malformed
        );
    }

    #[test]
    fn truncated_image_bytes_fall_back_honestly() {
        let support = ImageSupport::halfblocks();
        let garbage = BASE64_STANDARD.encode(b"not an image");
        assert_eq!(
            support.decode(&garbage).unwrap_err(),
            DecodeError::Malformed
        );
    }

    #[test]
    fn placeholder_renders_the_open_hint_and_known_dimensions() {
        let area = Rect::new(0, 0, 24, 5);
        let mut buffer = Buffer::empty(area);
        render_placeholder(
            area,
            &mut buffer,
            Style::default(),
            Some((640, 480)),
            "no protocol",
        );
        let content: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(content.contains("no protocol"));
        assert!(content.contains("640\u{d7}480"));
        assert!(content.contains("open externally"));
    }

    #[test]
    fn decoded_image_renders_without_a_real_terminal() {
        let support = ImageSupport::halfblocks();
        let mut decoded = Some(support.decode(TINY_PNG_BASE64).expect("decodes"));
        let area = Rect::new(0, 0, 10, 4);
        let mut buffer = Buffer::empty(area);
        render_image(area, &mut buffer, Style::default(), &mut decoded);
    }
}
