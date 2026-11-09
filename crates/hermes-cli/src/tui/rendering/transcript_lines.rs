// The transcript renderer threads its output buffer, render state and styles
// through each call; bundling them into a struct would be a redesign.
#[allow(clippy::too_many_arguments)]
fn append_transcript_message_lines(
    lines: &mut Vec<Line<'static>>,
    msg: &hermes_core::Message,
    msg_idx: usize,
    rendered_messages: &mut usize,
    state: &mut TuiState,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
    divider: &str,
) {
    // Hide internal orchestration/system payloads from the chat transcript.
    if matches!(msg.role, hermes_core::MessageRole::System) {
        return;
    }
    if *rendered_messages > 0 && matches!(state.view_density, ViewDensity::Detailed) {
        lines.push(Line::from(String::new()));
    }
    *rendered_messages += 1;
    let (glyph, label, label_style, body_style) = role_visuals(msg.role, styles, colors);
    let stamp = if state.show_timestamps {
        let fp = message_fingerprint(msg);
        state
            .message_time_labels
            .entry(fp)
            .or_insert_with(|| Local::now().format("%H:%M:%S").to_string())
            .clone()
    } else {
        String::new()
    };
    let label_text = if stamp.is_empty() {
        label.to_string()
    } else {
        format!("{label}  {stamp}")
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!(" ╭ {} ", glyph),
            label_style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(label_text, label_style.add_modifier(Modifier::BOLD)),
    ]));

    if let Some(content) = msg.content.as_deref() {
        match msg.role {
            hermes_core::MessageRole::Assistant => {
                let assistant_lines = render_assistant_markdown_lines(content, styles, colors);
                lines.extend(collapse_render_lines_with_notice(
                    assistant_lines,
                    max_assistant_render_lines(),
                    colors,
                ));
            }
            hermes_core::MessageRole::Tool => {
                let card_key = format!("tool:{msg_idx}");
                let expanded = state.expanded_tool_cards.contains(&card_key)
                    || state.expanded_tool_cards.contains("__all__");
                let all_lines = format_tool_message_lines(content);
                let shown = if expanded { 20 } else { 4 };
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "    [tool card: {} | {} lines | Ctrl+E toggles]",
                        if expanded { "expanded" } else { "collapsed" },
                        all_lines.len()
                    ),
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.background),
                )]));
                for line in all_lines.iter().take(shown) {
                    for segment in hard_wrap_segments(line, TRANSCRIPT_CONTENT_WRAP_COLS) {
                        lines.push(render_inline_with_code(
                            "    ",
                            &segment,
                            styles.tool_result,
                            Style::default()
                                .fg(colors.accent)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                }
                if all_lines.len() > shown {
                    lines.push(Line::from(vec![Span::styled(
                        format!("    … {} more lines", all_lines.len() - shown),
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background),
                    )]));
                }
            }
            _ => {
                for line in content.lines() {
                    for segment in hard_wrap_segments(line, TRANSCRIPT_CONTENT_WRAP_COLS) {
                        lines.push(render_inline_with_code(
                            "    ",
                            &segment,
                            body_style,
                            Style::default()
                                .fg(colors.accent)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                }
            }
        }
    }

    if msg.role == hermes_core::MessageRole::Assistant {
        if matches!(state.view_density, ViewDensity::Detailed) {
            if let Some(reasoning) = msg
                .reasoning_content
                .as_ref()
                .filter(|s| !s.trim().is_empty())
            {
                lines.push(Line::from(vec![Span::styled(
                    "    🤔 reasoning",
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.background),
                )]));
                for line in reasoning.lines() {
                    lines.push(Line::from(vec![Span::styled(
                        format!("      {}", line.trim_end()),
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background)
                            .add_modifier(Modifier::ITALIC),
                    )]));
                }
            }
        }
        if let Some(tool_calls) = msg.tool_calls.as_ref() {
            for tc in tool_calls {
                let args = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::Null);
                let preview =
                    build_tool_preview_from_value(&tc.function.name, &args, 44).unwrap_or_default();
                let emoji = tool_emoji(&tc.function.name);
                let summary = if preview.is_empty() {
                    format!("{emoji} {}", tc.function.name)
                } else {
                    format!("{emoji} {} {}", tc.function.name, preview)
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        "    ↳ ",
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background),
                    ),
                    Span::styled(summary, styles.tool_call),
                ]));
            }
        }
    }
    if matches!(state.view_density, ViewDensity::Detailed) {
        lines.push(Line::from(vec![Span::styled(
            divider.to_string(),
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background),
        )]));
    }
}

fn build_transcript_lines(
    messages: &[hermes_core::Message],
    state: &mut TuiState,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
    content_width: u16,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rendered_messages = 0usize;
    let divider = transcript_divider(content_width);

    for (msg_idx, msg) in messages.iter().enumerate() {
        append_transcript_message_lines(
            &mut lines,
            msg,
            msg_idx,
            &mut rendered_messages,
            state,
            styles,
            colors,
            &divider,
        );
    }

    // Streaming buffer (partial assistant response)
    if !state.stream_buffer.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::from(String::new()));
        }
        lines.push(Line::from(vec![
            Span::styled(
                " ╭ ● ",
                styles.assistant_response.add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "REFORMED (streaming)",
                styles.assistant_response.add_modifier(Modifier::BOLD),
            ),
        ]));
        let stream_lines = render_assistant_markdown_lines(&state.stream_buffer, styles, colors);
        lines.extend(tail_render_lines_with_notice(
            stream_lines,
            MAX_STREAM_RENDER_LINES,
            colors,
        ));
        lines.push(Line::from(vec![Span::styled(
            "    ▌",
            Style::default()
                .fg(colors.accent)
                .bg(colors.background)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(vec![Span::styled(
            divider,
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background),
        )]));
    }

    if lines.is_empty() {
        let title = Style::default()
            .fg(colors.status_bar_text)
            .bg(colors.background)
            .add_modifier(Modifier::BOLD);
        let mark = Style::default().fg(colors.accent).bg(colors.background);
        lines.push(Line::from(vec![
            Span::styled(" ▤ ", mark),
            Span::styled("Printing press 2.0", title),
        ]));
        lines.push(Line::from(Span::styled(" ──────────────────", mark)));
        lines.push(Line::from(vec![Span::styled(
            " Start chatting.",
            Style::default()
                .fg(colors.foreground)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        )]));
    }
    lines
}

