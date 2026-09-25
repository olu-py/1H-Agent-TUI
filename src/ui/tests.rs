use super::*;

#[test]
fn palette_commands_align_after_mixed_width_labels() {
    let label_columns = commands::PALETTE_ITEMS
        .iter()
        .map(|item| UnicodeWidthStr::width(item.label))
        .max()
        .unwrap();
    let command_columns = commands::PALETTE_ITEMS
        .iter()
        .filter(|item| item.command.is_some())
        .map(|item| {
            let text = palette_item_text(item, false, label_columns);
            UnicodeWidthStr::width(&text[..text.find('/').unwrap()])
        })
        .collect::<HashSet<_>>();

    assert_eq!(command_columns.len(), 1);
}

#[test]
fn long_visual_line_renders_only_the_visible_style_slices() {
    let text = "中🙂e\u{301}".repeat(6_000);
    let layout = MessageLayout::new(
        vec![Line::from(Span::styled(
            text,
            Style::default().fg(Color::Green),
        ))],
        Rect::new(0, 0, 80, 24),
        0,
    );
    assert!(layout.visual_lines.len() > 100);
    let visual = &layout.visual_lines[layout.visual_lines.len() / 2];
    let rendered = render_visual_line(&layout, visual, None, &UiTheme::default());
    assert_eq!(rendered.spans.len(), 1);

    let line = &layout.lines[visual.logical_line];
    let local_start = visual.start - line.start;
    let first = line.text[local_start..]
        .grapheme_indices(true)
        .nth(1)
        .map(|(offset, _)| visual.start + offset)
        .unwrap();
    let selected = render_visual_line(
        &layout,
        visual,
        Some(OutputSelection {
            anchor: first,
            active: visual.end,
            dragging: false,
        }),
        &UiTheme::default(),
    );
    assert!(selected.spans.len() <= 2);
}

#[test]
fn thinking_summary_folds_to_last_line_and_expands() {
    let thinking = ThinkingDisplay {
        id: "thinking-0".into(),
        content: "第一行\n\n最后一行".into(),
    };
    let target = Some(InteractionTarget::ThinkingSummary("thinking-0".into()));

    let mut lines = Vec::new();
    let mut interactions = Vec::new();
    render_thinking_summary(&thinking, false, None, 40, &mut lines, &mut interactions);
    assert_eq!(lines.len(), 1);
    assert_eq!(interactions, vec![target.clone()]);
    let collapsed = lines[0].to_string();
    assert!(collapsed.contains('▸'));
    assert!(collapsed.contains("最后一行"));
    assert!(!collapsed.contains("第一行"));

    lines.clear();
    interactions.clear();
    render_thinking_summary(&thinking, true, None, 40, &mut lines, &mut interactions);
    assert_eq!(interactions.first(), Some(&target));
    let expanded = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(expanded.contains('▾'));
    assert!(expanded.contains("第一行"));
    assert!(expanded.contains("最后一行"));
}

#[test]
fn settings_rows_group_fields_in_order() {
    let rows = settings_rows();
    let sections: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            SettingsRow::Section(section) => Some(*section),
            SettingsRow::Field(_) | SettingsRow::ContextWindow | SettingsRow::Spacer => None,
        })
        .collect();
    assert_eq!(sections, vec!["基础", "连接", "高级"]);

    let fields: Vec<SettingsField> = rows
        .iter()
        .filter_map(|row| match row {
            SettingsRow::Section(_) | SettingsRow::ContextWindow | SettingsRow::Spacer => None,
            SettingsRow::Field(field) => Some(*field),
        })
        .collect();
    assert_eq!(
        fields,
        vec![
            SettingsField::Preset,
            SettingsField::Protocol,
            SettingsField::Model,
            SettingsField::BaseUrl,
            SettingsField::Thinking,
            SettingsField::ApiKey,
        ]
    );
}

#[test]
fn task_block_inner_is_the_message_layout_viewport() {
    let area = Rect::new(30, 0, 80, 20);
    let block = Block::default().title(" 任务 ").borders(Borders::BOTTOM);
    let viewport = block.inner(area);
    assert_eq!(viewport.x, area.x);
    assert_eq!(viewport.y, area.y + 1);
    assert_eq!(viewport.height, area.height - 2);

    let layout = MessageLayout::new(vec![Line::from("正文")], viewport, 0);
    assert_eq!(layout.viewport, block.inner(area));

    let tiny = Block::default()
        .title(" 任务 ")
        .borders(Borders::BOTTOM)
        .inner(Rect::new(0, 0, 1, 1));
    assert_eq!(tiny.height, 0);
}

#[test]
fn session_window_start_keeps_current_visible_and_bounded() {
    assert_eq!(session_window_start(3, 0, 4), 0);
    assert_eq!(session_window_start(10, 0, 4), 0);
    assert_eq!(session_window_start(10, 3, 4), 0);
    assert_eq!(session_window_start(10, 4, 4), 1);
    assert_eq!(session_window_start(10, 9, 4), 6);
    assert_eq!(session_window_start(2, 1, 4), 0);
}

#[test]
fn flatten_session_tree_defaults_collapsed_only_click_expands() {
    let sessions = vec![
        SessionSummary {
            id: "root".into(),
            title: "Root".into(),
            parent_id: None,
            child_status: None,
        },
        SessionSummary {
            id: "other".into(),
            title: "Other".into(),
            parent_id: None,
            child_status: None,
        },
        SessionSummary {
            id: "child".into(),
            title: "Child".into(),
            parent_id: Some("root".into()),
            child_status: None,
        },
    ];
    // Default: nothing explicitly expanded -> everything collapsed.
    let rows = flatten_session_tree(&sessions, &HashSet::new());
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, vec!["root", "other"]);
    assert!(rows[0].has_children && !rows[0].expanded);

    // Explicit (click) expansion shows the children.
    let expanded: HashSet<String> = ["root".into()].into_iter().collect();
    let rows = flatten_session_tree(&sessions, &expanded);
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, vec!["root", "child", "other"]);
    assert_eq!(rows[1].depth, 1);
    assert!(rows[0].expanded);
}

#[test]
fn session_index_at_maps_only_visible_session_rows() {
    let area = Rect::new(0, 0, 30, 12);
    // Panel title row 0 and header rows 1..4 are not sessions.
    assert_eq!(session_index_at(area, 1, 0, 10, 0), None);
    assert_eq!(session_index_at(area, 1, 1, 10, 0), None);
    assert_eq!(session_index_at(area, 1, 4, 10, 0), None);
    // Six session rows are visible (12 - 1 title - 4 header - 1 trailing).
    assert_eq!(session_index_at(area, 1, 5, 10, 0), Some(0));
    assert_eq!(session_index_at(area, 1, 10, 10, 0), Some(5));
    // Trailing blank row and panel border/outside map to None.
    assert_eq!(session_index_at(area, 1, 11, 10, 0), None);
    assert_eq!(session_index_at(area, 29, 5, 10, 0), None);
    assert_eq!(session_index_at(area, 30, 5, 10, 0), None);
}

#[test]
fn input_mode_rect_uses_utf8_width_and_rejects_narrow_inputs() {
    let area = Rect::new(0, 20, 40, 5);
    assert_eq!(
        input_mode_rect(area, AgentMode::Build),
        Some(Rect::new(9, 20, 4, 1))
    );
    let narrow = Rect::new(0, 20, 8, 5);
    assert_eq!(input_mode_rect(narrow, AgentMode::Build), None);
}

#[test]
fn input_cursor_tracks_terminal_columns() {
    assert_eq!(input_viewport("hello", 20), ("hello", 5));
    assert_eq!(
        input_viewport("12345678901234567890", 20),
        ("2345678901234567890", 19)
    );
    assert_eq!(input_viewport("中文a", 20), ("中文a", 5));
    assert_eq!(input_viewport("中文测试abc", 8), ("测试abc", 7));
}

#[test]
fn text_fitting_respects_wide_characters() {
    assert_eq!(fit_text("中文测试", 7), "中文...");
    assert_eq!(fit_text("short", 7), "short");
}

#[test]
fn markdown_blocks_and_inline_styles_are_rendered() {
    let lines = render_markdown(
        "# Heading\n- **bold text** and `code`\n> quote\n```rust\nlet value = 1;\n```",
        Style::default(),
    );
    let text = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(text[0], "Heading");
    assert_eq!(text[1], "• bold text and code");
    assert_eq!(text[2], "│ quote");
    assert_eq!(text[3], "let value = 1;");
    assert!(!text.iter().any(|line| line.contains("[code")));
    assert!(!text.iter().any(|line| line.contains("[end code]")));
    assert!(lines[1].spans.len() >= 4);
}

#[test]
fn fenced_code_preserves_literal_markdown_and_copy_text() {
    for markdown in [
        "```bash\n./1h-agent --workspace /Users/yang/Desktop\n```",
        "~~~bash\n* # [x] <b>中文🙂</b>\n~~~",
        "    one\n      two\n\n    * literal",
        "```\n  indented\n\n`backtick` # heading\n```",
    ] {
        let lines = render_markdown(markdown, Style::default());
        let text = markdown_text(&lines);
        assert!(!text.contains("[code"));
        assert!(!text.contains("[end code]"));
        assert!(!text.contains("code:"));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(Color::Gray))
        }));
    }

    let lines = render_markdown(
        "```bash\n./1h-agent --workspace /Users/yang/Desktop\n```",
        Style::default(),
    );
    let layout = MessageLayout::new(lines, Rect::new(0, 0, 80, 10), 0);
    assert!(
        layout
            .text
            .contains("./1h-agent --workspace /Users/yang/Desktop")
    );
    assert!(!layout.text.contains("bash"));
    assert!(!layout.text.contains("[code"));
}

#[test]
fn unclosed_streaming_code_is_visible_without_end_marker() {
    let lines = render_markdown(
        "```rust\nfn main() {\n  println!(\"中文🙂 * # [x]\");",
        Style::default(),
    );
    let text = markdown_text(&lines);
    assert!(text.contains("fn main()"));
    assert!(text.contains("中文🙂 * # [x]"));
    assert!(!text.contains("[code"));
    assert!(!text.contains("[end code]"));
}

#[test]
fn code_block_keeps_empty_lines_and_nested_indent() {
    let lines = render_markdown(
        "```text\nroot\n\n    child\n        grandchild\n```",
        Style::default(),
    );
    let text = markdown_text(&lines);
    assert!(text.contains("root\n\n    child\n        grandchild"));
}

fn markdown_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn markdown_renders_nested_lists_tasks_tables_and_footnotes() {
    let lines = render_markdown(
        "> outer quote\n>\n> > nested **text**\n\n1. first\n   - nested\n   - [x] done\n\n| 名称 | 值 |\n| :--- | ---: |\n| 中文 | 🙂 |\n\n引用[^1]\n\n[^1]: 脚注内容",
        Style::default(),
    );
    let text = markdown_text(&lines);
    assert!(text.contains("│ outer quote"));
    assert!(text.contains("│ │ nested text"));
    assert!(text.contains("1. first"));
    assert!(text.contains("  • nested"));
    assert!(text.contains("  • [x] done"));
    assert!(text.contains("| 名称"));
    assert!(text.contains("| ---"));
    assert!(text.contains("| 中文"));
    assert!(text.contains("引用[^1]"));
    assert!(text.contains("[^1]: 脚注内容"));
}

#[test]
fn markdown_renders_gfm_alert_labels_and_nested_quotes() {
    let lines = render_markdown(
        "> [!NOTE]\n> note body\n\n> [!TIP]\n> tip body\n\n> [!IMPORTANT]\n> important body\n\n> [!WARNING]\n> warning body\n\n> [!CAUTION]\n> caution body\n\n> ordinary\n> > [!NOTE]\n> > nested note\n\nafter",
        Style::default(),
    );
    let text = markdown_text(&lines);
    for label in ["NOTE", "TIP", "IMPORTANT", "WARNING", "CAUTION"] {
        assert!(
            text.contains(&format!("│ {label}")),
            "missing {label} in {text}"
        );
    }
    assert!(text.contains("│ note body"));
    assert!(text.contains("│ │ NOTE"));
    assert!(text.contains("│ │ nested note"));
    assert!(text.contains("after"));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content == "WARNING" && span.style.fg == Some(Color::Yellow))
    }));
}

#[test]
fn markdown_table_uses_alignment_padding_and_visible_delimiters() {
    let lines = render_markdown(
        "| Left | Center | Right |\n| :--- | :---: | ---: |\n| a | b | c |",
        Style::default(),
    );
    let text = markdown_text(&lines);
    assert!(text.contains("| :---- | :------: | -----: |"));
    assert!(text.contains("| a    |   b    |     c |"));
}

#[test]
fn markdown_renders_links_images_nested_styles_and_html_as_text() {
    let lines = render_markdown(
        r#"**粗 *体*** ~~删~~ `代码` [链接](https://example.test/a) ![图片](https://example.test/i.png) <span>HTML</span> \*字面星号\* 中文🙂"#,
        Style::default(),
    );
    let text = markdown_text(&lines);
    assert!(text.contains("粗 体"));
    assert!(text.contains("删"));
    assert!(text.contains("代码"));
    assert!(text.contains("链接 (https://example.test/a)"));
    assert!(text.contains("图片 (https://example.test/i.png)"));
    assert!(text.contains("<span>HTML</span>"));
    assert!(text.contains("*字面星号* 中文🙂"));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.style.add_modifier(Modifier::BOLD) == span.style)
    }));
}

#[test]
fn markdown_handles_code_variants_soft_breaks_and_incomplete_streams() {
    let lines = render_markdown(
        "before\nsoft break\n\n    indented code\n\n~~~text\nfenced\n~~~\n\n**incomplete",
        Style::default(),
    );
    let text = markdown_text(&lines);
    assert!(text.contains("before"));
    assert!(text.contains("soft break"));
    assert!(text.contains("indented code"));
    assert!(text.contains("fenced"));
    assert!(!text.contains("[code"));
    assert!(!text.contains("[end code]"));
    assert!(text.contains("incomplete"));

    let hard_break = markdown_text(&render_markdown("one  \ntwo", Style::default()));
    assert_eq!(hard_break, "one\ntwo");
    let paragraph_gap = markdown_text(&render_markdown("first\n\nsecond", Style::default()));
    assert_eq!(paragraph_gap, "first\n\nsecond");
}

#[test]
fn markdown_lines_feed_message_layout_without_losing_copy_text() {
    let lines = render_markdown(
        r#"| A | B |
| --- | --- |
| **中文** | *🙂* |
| 长文本 | [链接](https://example.test) |
| 组合 é | Emoji 👩‍💻 |"#,
        Style::default(),
    );
    let layout = MessageLayout::new(lines, Rect::new(0, 0, 12, 10), 0);
    let selection = OutputSelection {
        anchor: 0,
        active: layout.text.len(),
        dragging: false,
    };
    assert_eq!(layout.selected_text(selection), Some(layout.text.as_str()));
    assert!(layout.text.contains("中文"));
    assert!(layout.text.contains("🙂"));
    assert!(layout.text.contains("长文本"));
    assert!(layout.text.contains("https://example.test"));
    assert!(layout.text.contains("e\u{301}"));
    assert!(layout.text.contains("👩‍💻"));
}

#[test]
fn tool_output_is_merged_translated_and_structured() {
    let tool = ToolDisplay {
        call_id: "call-1".into(),
        name: "file_read".into(),
        arguments: serde_json::json!({"path": "a.txt"}),
        status: ToolDisplayStatus::Completed,
        result: Some("line one\nline two".into()),
    };
    let mut lines = Vec::new();
    let mut interactions = Vec::new();
    render_tool(&tool, true, 80, &mut lines, &mut interactions);
    let text = markdown_text(&lines);
    assert!(text.contains("文件读取"));
    assert!(text.contains("路径：a.txt"));
    assert!(text.contains("line two"));
    assert_eq!(
        interactions[0],
        Some(InteractionTarget::Tool("call-1".into()))
    );
}

#[test]
fn all_builtin_tool_names_are_translated_and_unknown_names_stay_readable() {
    let expected = [
        ("file_list", "文件列表"),
        ("file_stat", "文件信息"),
        ("file_read", "文件读取"),
        ("file_search", "文件搜索"),
        ("file_glob", "文件查找"),
        ("repo_map", "符号大纲"),
        ("file_mkdir", "新建目录"),
        ("file_write", "文件修改"),
        ("file_edit", "文件编辑"),
        ("file_copy", "文件复制"),
        ("file_move", "文件移动"),
        ("file_delete", "文件删除"),
        ("web_search", "网络搜索"),
        ("web_fetch", "网页读取"),
        ("market_quote", "实时行情"),
        ("terminal_exec", "命令执行"),
        ("terminal_shell", "Shell 命令"),
        ("agent_spawn", "子 Agent"),
        ("git", "Git 操作"),
        ("git_diff", "差异查看"),
        ("browser_open", "打开网页"),
        ("browser_snapshot", "页面快照"),
        ("browser_click", "页面点击"),
        ("browser_type", "页面输入"),
        ("browser_press", "页面按键"),
    ];
    for (name, translated) in expected {
        assert_eq!(tool_display_name(name), translated);
    }
    assert_eq!(tool_display_name("custom_reader"), "custom reader");
    assert_eq!(
        tool_display_name("mcp:server:remote_tool"),
        "外部工具：remote tool"
    );
}

#[test]
fn diff_lines_receive_semantic_colors() {
    let lines = render_diff("@@ -1 +1 @@\n-old\n+new");
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1].spans[0].style.fg, Some(Color::Red));
    assert_eq!(lines[2].spans[0].style.fg, Some(Color::Green));
}

#[test]
fn approval_payload_is_human_readable_and_redacts_secrets() {
    let call = crate::provider::ToolCall {
        id: "call_1".into(),
        name: "terminal_shell".into(),
        arguments: serde_json::json!({
            "command": "git status",
            "api_key": "secret-value"
        }),
    };
    let text = approval_lines(&call, "needs approval")
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("Shell 命令"));
    assert!(text.contains("HIGH"));
    assert!(text.contains("[redacted]"));
    assert!(!text.contains("secret-value"));
    assert!(!text.contains("\"command\""));
}

#[test]
fn live_thinking_wraps_all_content_on_grapheme_boundaries() {
    assert_eq!(
        wrap_grapheme_lines("「正在检查项目结构」", 10),
        vec!["「正在检查", "项目结构」"]
    );
    assert_eq!(
        wrap_grapheme_lines("abc👩‍💻e\u{301}尾", 5),
        vec!["abc👩‍💻", "e\u{301}尾"]
    );
    assert_eq!(
        wrap_grapheme_lines("one\n\ntwo", 20),
        vec!["one", "", "two"]
    );
}
