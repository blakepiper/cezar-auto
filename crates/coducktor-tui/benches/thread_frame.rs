//! Measures the live thread path rather than a static transcript: provider events arrive in the
//! same small batches used by the measured production workload, and every batch is followed by a
//! render. Projection and render time are reported separately so regressions keep a useful owner.

use std::time::{Duration, Instant};

use coducktor_contract::{ApiRun, RunEvent, RunRecord, RunStatus};
use coducktor_tui::screens::thread::ThreadUi;
use coducktor_tui::theme::{ColorCapability, Theme, ThemeName};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

// The runtime drains at most 256 receiver items into one frame. The scaling regression test uses
// smaller observed batches; the bench uses the full frame bound so its 30k setup stays runnable.
const FRAME_BATCH_SIZE: usize = 256;
const VIEWPORT: Rect = Rect::new(0, 0, 120, 40);
const EVENT_COUNTS: [usize; 4] = [1_000, 6_000, 12_000, 30_000];

fn run() -> ApiRun {
    ApiRun {
        record: RunRecord {
            id: "frame-bench".to_owned(),
            title: "Profile a live task thread".to_owned(),
            task: "Inspect the implementation, make the focused change, and run the tests."
                .to_owned(),
            status: RunStatus::Running,
            ..RunRecord::default()
        },
        usage: None,
    }
}

fn event(sequence: usize) -> RunEvent {
    let item_index = sequence / 32;
    let offset = sequence % 32;
    let is_tool = item_index.is_multiple_of(2);
    let id = if is_tool {
        format!("tool-{item_index}")
    } else {
        format!("message-{item_index}")
    };
    let (event_type, extra) = match offset {
        0 if is_tool => (
            "item.started",
            json!({"item": {
                "kind": "tool",
                "id": id,
                "name": "Bash",
                "toolKind": "execute",
                "title": "Run cargo test --workspace",
                "status": "running",
                "input": {"command": "cargo test --workspace"}
            }}),
        ),
        0 => (
            "item.started",
            json!({"item": {
                "kind": "message",
                "id": id,
                "role": "assistant",
                "text": "",
                "phase": "commentary"
            }}),
        ),
        31 if is_tool => (
            "item.completed",
            json!({"item": {
                "kind": "tool",
                "id": id,
                "name": "Bash",
                "toolKind": "execute",
                "title": "Run cargo test --workspace",
                "status": "completed",
                "input": {"command": "cargo test --workspace"},
                "output": (0..80).map(|line| format!("test case {line}: ok")).collect::<Vec<_>>().join("\n"),
                "exitCode": 0
            }}),
        ),
        31 => (
            "item.completed",
            json!({"item": {
                "kind": "message",
                "id": id,
                "role": "assistant",
                "text": "### Progress\n\nThe focused change is in place, and the relevant tests are passing.",
                "phase": "commentary"
            }}),
        ),
        _ if is_tool => (
            "item.delta",
            json!({"itemId": id, "field": "output", "delta": "checking target... ok\n"}),
        ),
        _ => (
            "item.delta",
            json!({"itemId": id, "field": "text", "delta": "Streaming a markdown update. "}),
        ),
    };
    RunEvent {
        seq: (sequence + 1) as f64,
        ts: "2026-08-22T00:00:00Z".to_owned(),
        step_id: Some("agent".to_owned()),
        event_type: event_type.to_owned(),
        extra: extra.as_object().cloned().unwrap_or_default(),
    }
}

fn prepare_thread(event_count: usize) -> (ThreadUi, Theme) {
    let events = (0..event_count).map(event).collect::<Vec<_>>();
    let mut thread = ThreadUi::default();
    thread.load(
        "main".to_owned(),
        "frame-bench".to_owned(),
        run(),
        Vec::new(),
        0.0,
        None,
    );
    let theme = Theme::new(ThemeName::Dark, ColorCapability::TrueColor);
    for batch in events.chunks(FRAME_BATCH_SIZE) {
        thread.push_events(batch.iter().cloned().map(|event| (event.seq, event)));
        let mut buffer = Buffer::empty(VIEWPORT);
        thread.transcript.render(&mut buffer, VIEWPORT, &theme);
    }
    (thread, theme)
}

fn next_frame(thread: &mut ThreadUi, theme: &Theme) -> (Duration, Duration) {
    let first = thread.data.as_of_seq as usize;
    let batch = (first..first + FRAME_BATCH_SIZE)
        .map(event)
        .collect::<Vec<_>>();
    let started = Instant::now();
    thread.push_events(batch.into_iter().map(|event| (event.seq, event)));
    let projection = started.elapsed();

    let mut buffer = Buffer::empty(VIEWPORT);
    let started = Instant::now();
    thread.transcript.render(&mut buffer, VIEWPORT, theme);
    let render = started.elapsed();
    std::hint::black_box(buffer);
    (projection, render)
}

fn bench_thread_frames(c: &mut Criterion) {
    let mut group = c.benchmark_group("live_thread_frames");
    group.sample_size(10);
    for event_count in EVENT_COUNTS {
        group.throughput(Throughput::Elements(event_count as u64));
        let (mut projection_thread, projection_theme) = prepare_thread(event_count);
        group.bench_with_input(
            BenchmarkId::new("projection", event_count),
            &event_count,
            |b, _| {
                b.iter_custom(|iterations| {
                    (0..iterations)
                        .map(|_| next_frame(&mut projection_thread, &projection_theme).0)
                        .sum()
                });
            },
        );
        let (mut render_thread, render_theme) = prepare_thread(event_count);
        group.bench_with_input(
            BenchmarkId::new("render", event_count),
            &event_count,
            |b, _| {
                b.iter_custom(|iterations| {
                    (0..iterations)
                        .map(|_| next_frame(&mut render_thread, &render_theme).1)
                        .sum()
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_thread_frames);
criterion_main!(benches);
