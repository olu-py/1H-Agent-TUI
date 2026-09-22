use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use protium_core::{commands::AgentMode, protocol::MessageDto};
use protium_tui::{output::MessageLayout, projection::TuiSessionProjection};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
};

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
    group.finish();
}

criterion_group!(consumer, bench_projection, bench_layout);
criterion_main!(consumer);
