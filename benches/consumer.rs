use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use protium_core::{
    commands::AgentMode,
    protocol::{Event, MessageDto},
};
use protium_tui::{
    output::{CachedLayoutSegment, MessageLayout},
    projection::TuiSessionProjection,
};
use ratatui::{
    Terminal,
    backend::{CrosstermBackend, TestBackend},
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

#[derive(Clone)]
struct ByteCounter(Arc<AtomicUsize>);

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.fetch_add(bytes.len(), Ordering::Relaxed);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const MESSAGE_COUNT: usize = 10_000;

fn message_history() -> Vec<MessageDto> {
    (0..MESSAGE_COUNT)
        .map(|index| {
            let content = format!(
                "Message {index}: **fixed markdown payload** with 中文 width and `inline code`."
            );
            if index % 2 == 0 {
                MessageDto::User {
                    id: index as i64,
                    content,
                    created_at: "2026-01-01T00:00:00Z".into(),
                }
            } else {
                MessageDto::Assistant {
                    id: index as i64,
                    content,
                    created_at: "2026-01-01T00:00:00Z".into(),
                }
            }
        })
        .collect()
}

fn styled_history() -> Vec<Line<'static>> {
    (0..MESSAGE_COUNT)
        .map(|index| {
            Line::from(vec![
                Span::styled(
                    format!("Message {index}: "),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled("fixed markdown payload ", Style::default().fg(Color::White)),
                Span::styled(
                    "with 中文 width and inline code",
                    Style::default().fg(Color::Yellow),
                ),
            ])
        })
        .collect()
}

fn bench_projection(c: &mut Criterion) {
    let messages = message_history();
    let mut group = c.benchmark_group("tui_projection");
    group.bench_function("map_10k_messages", |b| {
        b.iter(|| {
            black_box(TuiSessionProjection::message_dto_to_entries(black_box(
                &messages,
            )))
        });
    });
    group.bench_function("replace_and_bound_10k_messages", |b| {
        b.iter_batched(
            || TuiSessionProjection::new("bench-session".into(), AgentMode::Build, Some(128_000)),
            |mut projection| {
                projection.replace_history(TuiSessionProjection::message_dto_to_entries(
                    black_box(&messages),
                ));
                black_box(projection.entries.len())
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_layout(c: &mut Criterion) {
    let lines = styled_history();
    let wide_viewport = Rect::new(0, 0, 120, 40);
    let narrow_viewport = Rect::new(0, 0, 48, 40);
    let layout = MessageLayout::new(lines.clone(), wide_viewport, usize::MAX);
    let mut group = c.benchmark_group("tui_layout");

    group.bench_function("build_10k_unicode_markdown_lines", |b| {
        b.iter_batched(
            || lines.clone(),
            |input| black_box(MessageLayout::new(input, wide_viewport, usize::MAX)),
            BatchSize::LargeInput,
        );
    });
    group.bench_function("reflow_10k_lines_to_narrow_width", |b| {
        b.iter_batched(
            || layout.clone(),
            |input| black_box(input.reflow(narrow_viewport)),
            BatchSize::LargeInput,
        );
    });
    group.bench_function("clip_visible_40_rows_from_10k", |b| {
        let start = layout.scroll;
        let end = (start + wide_viewport.height as usize).min(layout.visual_lines.len());
        b.iter(|| {
            for line in &layout.visual_lines[start..end] {
                black_box(line);
            }
        });
    });
    group.bench_function("toggle_live_thinking_clickable_10k", |b| {
        let mut input = MessageLayout::new_with_live_thinking(
            lines.clone(),
            wide_viewport,
            usize::MAX,
            Some(MESSAGE_COUNT / 2),
        );
        let mut clickable = false;
        b.iter(|| {
            clickable = !clickable;
            let layout = black_box(&mut input);
            layout.set_live_thinking_clickable(black_box(clickable));
            black_box(
                layout.visual_lines[layout.live_thinking_visual_start.unwrap()]
                    .interaction
                    .is_some(),
            )
        });
    });
    group.bench_function("scan_live_thinking_10k_reference", |b| {
        let mut input = MessageLayout::new_with_live_thinking(
            lines.clone(),
            wide_viewport,
            usize::MAX,
            Some(MESSAGE_COUNT / 2),
        );
        let mut clickable = false;
        b.iter(|| {
            clickable = !clickable;
            let layout = black_box(&mut input);
            if let Some(visual) = layout.visual_lines.iter_mut().find(|line| line.synthetic) {
                visual.interaction = black_box(clickable)
                    .then_some(protium_tui::output::InteractionTarget::Thinking);
            }
            black_box(
                layout.visual_lines[layout.live_thinking_visual_start.unwrap()]
                    .interaction
                    .is_some(),
            )
        });
    });
    group.finish();
}

fn bench_stream_and_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("tui_stream");
    for reasoning in [false, true] {
        let event = if reasoning {
            Event::ReasoningDelta {
                delta: "benchmark 中文 thinking payload. ".repeat(2),
            }
        } else {
            Event::TextDelta {
                delta: "benchmark 中文 assistant payload. ".repeat(2),
            }
        };
        group.bench_function(
            if reasoning {
                "reasoning_10k_deltas"
            } else {
                "text_10k_deltas"
            },
            |b| {
                b.iter_batched(
                    || TuiSessionProjection::new("bench".into(), AgentMode::Build, Some(128_000)),
                    |mut projection| {
                        for _ in 0..10_000 {
                            black_box(projection.handle_event(&event));
                        }
                        black_box(projection);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    for (name, count, delta) in [
        (
            "reasoning_1k_no_newlines",
            1000,
            "benchmark 中文 thinking payload. ".repeat(2),
        ),
        (
            "reasoning_10k_with_newlines",
            10_000,
            "benchmark 中文 thinking payload.\n".repeat(2),
        ),
    ] {
        let event = Event::ReasoningDelta { delta };
        group.bench_function(name, |b| {
            b.iter_batched(
                || TuiSessionProjection::new("bench".into(), AgentMode::Build, Some(128_000)),
                |mut projection| {
                    for _ in 0..count {
                        black_box(projection.handle_event(&event));
                    }
                    black_box(projection);
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
    let mut group = c.benchmark_group("ratatui_test_backend");
    let lines = styled_history().into_iter().take(40).collect::<Vec<_>>();
    group.bench_function("render_40_visible_rows", |b| {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        b.iter(|| {
            terminal
                .draw(|frame| frame.render_widget(Paragraph::new(lines.clone()), frame.area()))
                .unwrap();
        });
    });
    group.finish();
}

fn bench_crossterm_ansi_diff(c: &mut Criterion) {
    let byte_count = Arc::new(AtomicUsize::new(0));
    let sink = ByteCounter(Arc::clone(&byte_count));
    let mut terminal = Terminal::new(CrosstermBackend::new(sink)).unwrap();
    terminal.resize(Rect::new(0, 0, 120, 40)).unwrap();
    let mut lines = styled_history().into_iter().take(40).collect::<Vec<_>>();
    let mut frame_index = 0usize;
    let mut frames = 0usize;
    let mut group = c.benchmark_group("crossterm_ansi_diff");
    group.bench_function("streaming_tail_120x40_in_memory_writer", |b| {
        b.iter(|| {
            frame_index = frame_index.wrapping_add(1);
            lines[39] = Line::from(format!("live tail frame {frame_index} 中文"));
            terminal
                .draw(|frame| {
                    frame.render_widget(Paragraph::new(lines.clone()), frame.area());
                })
                .unwrap();
            frames += 1;
            black_box(byte_count.load(Ordering::Relaxed));
        });
    });
    if frames > 0 {
        let total_bytes = byte_count.load(Ordering::Relaxed);
        eprintln!(
            "ANSI_PIPELINE frames={frames} bytes={total_bytes} average_bytes_per_frame={:.2}",
            total_bytes as f64 / frames as f64
        );
    }
    group.finish();
}

fn bench_segment_tail_update(c: &mut Criterion) {
    let viewport = Rect::new(0, 0, 120, 40);
    let mut segments = (0..1000)
        .map(|index| {
            CachedLayoutSegment::new(
                index,
                120,
                0,
                vec![Line::from(format!(
                    "{index} {}",
                    "stable 中文 ".repeat(100)
                ))],
                vec![],
            )
        })
        .collect::<Vec<_>>();
    let tails = ["tail A 中文", "tail B 🙂"]
        .map(|text| CachedLayoutSegment::new(1001, 120, 0, vec![Line::from(text)], vec![]));
    let mut group = c.benchmark_group("segment_tail_update");
    group.bench_function("full_composition_1000_segments", |b| {
        b.iter(|| black_box(MessageLayout::from_segments(&segments, viewport, 0, None)));
    });
    let mut layout = MessageLayout::from_segments(&segments, viewport, 0, None);
    let mut index = 0;
    group.bench_function("incremental_1000_segments", |b| {
        b.iter(|| {
            index ^= 1;
            segments[999] = tails[index].clone();
            layout.update_segments(&segments, viewport, None);
            black_box(&layout);
        });
    });
    group.finish();
}

criterion_group!(
    consumer,
    bench_projection,
    bench_layout,
    bench_stream_and_frame,
    bench_crossterm_ansi_diff,
    bench_segment_tail_update
);
criterion_main!(consumer);
