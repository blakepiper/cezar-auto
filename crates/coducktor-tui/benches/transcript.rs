//! Guards the A7 accept criterion (spec §10, §13): a 5,000-item transcript must scroll
//! at ≥30 fps, i.e. each render must fit inside the 33ms frame budget `main.rs` runs to.
//!
//! `bench_scroll_steady_state` is the realistic case: the height cache is already warm
//! (as it is on every frame after the first at a given width) and only the viewport
//! window's items need painting, but `total_height` still walks every cached entry to
//! find the scroll bounds — this is what would regress if that summation, not just
//! per-item painting, stopped being cheap at 5,000 items.
//!
//! `bench_first_render_cold_cache` measures the one frame that isn't steady state: a
//! freshly opened thread, where every visible item's height is computed for the first
//! time. It is not held to the same 33ms bound — the spec's budget describes ongoing
//! scrolling, not the initial paint — but is tracked to catch a pathological blow-up.

use std::hint::black_box;

use coducktor_protocol::{MessageRole, ToolStatus};
use coducktor_tui::image::ImageSupport;
use coducktor_tui::theme::{ColorCapability, Theme, ThemeName};
use coducktor_tui::widgets::transcript::{
    ImageItem, MessageItem, NoteItem, NoteTone, ReasoningItem, ToolItem, Transcript, TranscriptItem,
};
use criterion::{Criterion, criterion_group, criterion_main};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const ITEM_COUNT: usize = 5_000;
const VIEWPORT: Rect = Rect::new(0, 0, 120, 40);
// `ImageItem::decode` needs base64 bytes; a real (if tiny) PNG keeps the bench honest
// about what a decoded image item actually costs to paint (halfblocks output), rather
// than exercising only the cheaper placeholder path.
const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

fn build_transcript() -> Transcript {
    let mut transcript = Transcript::new();
    let support = ImageSupport::halfblocks();
    for index in 0..ITEM_COUNT {
        let item = match index % 5 {
            0 => TranscriptItem::Message(MessageItem::new(
                format!("m{index}"),
                MessageRole::Assistant,
                format!("Turn {index}: applied a small fix and re-ran the suite."),
            )),
            1 => TranscriptItem::Reasoning(ReasoningItem::new(
                format!("r{index}"),
                format!("Considering approach {index} before touching the file."),
            )),
            2 => {
                let mut tool = ToolItem::new(
                    format!("t{index}"),
                    "Bash",
                    Some(
                        &serde_json::json!({"command": "cargo test", "description": "run the suite"}),
                    ),
                    ToolStatus::Completed,
                );
                tool.output = Some("running 4 tests\ntest result: ok. 4 passed".to_owned());
                tool.exit_code = Some(0);
                TranscriptItem::Tool(tool)
            }
            3 => TranscriptItem::Note(NoteItem::new(
                format!("n{index}"),
                format!("checkpoint {index} saved"),
                NoteTone::Dim,
            )),
            _ => TranscriptItem::Image(ImageItem::decode(
                format!("i{index}"),
                &support,
                TINY_PNG_BASE64,
            )),
        };
        transcript.push(item);
    }
    transcript
}

fn theme() -> Theme {
    Theme::new(ThemeName::Dark, ColorCapability::TrueColor)
}

fn bench_scroll_steady_state(c: &mut Criterion) {
    let mut transcript = build_transcript();
    let theme = theme();
    let mut buf = Buffer::empty(VIEWPORT);
    // Warm the height cache the same way the real app would after its first frame.
    transcript.render(&mut buf, VIEWPORT, &theme);

    c.bench_function("transcript_scroll_steady_state_5000_items", |b| {
        b.iter(|| {
            transcript.scroll_by(black_box(3));
            let mut buf = Buffer::empty(VIEWPORT);
            transcript.render(&mut buf, VIEWPORT, &theme);
            black_box(&buf);
        });
    });
}

fn bench_first_render_cold_cache(c: &mut Criterion) {
    let theme = theme();
    c.bench_function("transcript_first_render_cold_cache_5000_items", |b| {
        b.iter(|| {
            let mut transcript = build_transcript();
            let mut buf = Buffer::empty(VIEWPORT);
            transcript.render(&mut buf, VIEWPORT, &theme);
            black_box(&buf);
        });
    });
}

criterion_group!(
    benches,
    bench_scroll_steady_state,
    bench_first_render_cold_cache
);
criterion_main!(benches);
