use ratatui::{
    Frame,
    prelude::*,
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{
    app::{AppState, FocusTarget},
    types::{Role, ToolStatus},
};

const SELENAI_BANNER: &[&str] = &[
    r"  ███████╗███████╗██╗     ███████╗███╗   ██╗ █████╗ ██╗",
    r"  ██╔════╝██╔════╝██║     ██╔════╝████╗  ██║██╔══██╗██║",
    r"  ███████╗█████╗  ██║     █████╗  ██╔██╗ ██║███████║██║",
    r"  ╚════██║██╔══╝  ██║     ██╔══╝  ██║╚██╗██║██╔══██║██║",
    r"  ███████║███████╗███████╗███████╗██║ ╚████║██║  ██║██║",
    r"  ╚══════╝╚══════╝╚══════╝╚══════╝╚═╝  ╚═══╝╚═╝  ╚═╝╚═╝",
    r"             SYSTEM ONLINE :: V0.01 :: 🚀",
];

pub fn render_chat(frame: &mut Frame, area: Rect, state: &AppState) {
    let border_padding = if state.copy_mode { 0 } else { 2 };
    let inner_height = area.height.saturating_sub(border_padding).max(1);
    let inner_width = area.width.saturating_sub(border_padding).max(1);

    // We render from the bottom up to avoid processing thousands of lines that are off-screen.
    // We need enough lines to cover the scroll offset + the viewport height.
    let required_height = state.chat_scroll.saturating_add(inner_height);
    let mut collected_blocks: Vec<Vec<Line>> = Vec::new();
    let mut current_height: u16 = 0;
    
    // Iterate backwards through messages
    for message in state.messages.iter().rev() {
        let lines = message_to_lines(message);
        let height = estimate_wrapped_height(&lines, inner_width);
        collected_blocks.push(lines);
        current_height = current_height.saturating_add(height);
        
        if current_height >= required_height {
            break;
        }
    }

    // If we haven't filled the screen/scrollback yet, add the banner (if we reached the top)
    if current_height < required_height {
        let banner_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let mut banner_lines = Vec::new();
        for text in SELENAI_BANNER {
            banner_lines.push(Line::from(Span::styled(*text, banner_style)));
        }
        banner_lines.push(Line::default());
        
        // Only add if we really are at the start (checked by loop completion)
        // Actually, if we broke early, current_height >= required. 
        // If we didn't break, we processed all messages.
        collected_blocks.push(banner_lines);
    }

    // Restore order
    collected_blocks.reverse();
    let lines: Vec<Line> = collected_blocks.into_iter().flatten().collect();

    // If empty (no messages, no banner?), add placeholder
    let lines = if lines.is_empty() {
        vec![Line::from("No messages yet. Type below to get started.")]
    } else {
        lines
    };

    let mut title = "Conversation".to_string();
    let total_lines = estimate_wrapped_height(&lines, inner_width);
    let baseline = total_lines.saturating_sub(inner_height);
    let offset_from_bottom = state.chat_scroll.min(baseline);
    let scroll_top = baseline.saturating_sub(offset_from_bottom);

    if total_lines > inner_height {
        let percent = if baseline == 0 {
            100
        } else {
            let ratio = scroll_top as f64 / baseline as f64;
            (ratio * 100.0).round() as u16
        };
        title = format!("Conversation ({percent:>3}%)");
    }
    
    let block = base_block(&title, state.focus == FocusTarget::Chat, state.copy_mode);
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_top, 0))
        .block(block);

    frame.render_widget(paragraph, area);
}

fn message_to_lines(message: &crate::types::Message) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![Span::styled(
        message.role.display_name(),
        Style::default()
            .fg(role_color(message.role))
            .add_modifier(Modifier::BOLD),
    )]));
    append_multiline(&mut lines, &message.content);
    lines.push(Line::default());
    lines
}

pub fn render_tool_logs(frame: &mut Frame, area: Rect, state: &AppState) {
    let border_padding = if state.copy_mode { 0 } else { 2 };
    let inner_height = area.height.saturating_sub(border_padding).max(1);
    let inner_width = area.width.saturating_sub(border_padding).max(1);

    let required_height = state.tool_scroll.saturating_add(inner_height);
    let mut collected_blocks: Vec<Vec<Line>> = Vec::new();
    let mut current_height: u16 = 0;

    // Iterate backwards through logs
    for entry in state.tool_logs.iter().rev() {
        let lines = tool_entry_to_lines(entry);
        let height = estimate_wrapped_height(&lines, inner_width);
        collected_blocks.push(lines);
        current_height = current_height.saturating_add(height);

        if current_height >= required_height {
            break;
        }
    }

    if current_height < required_height && state.tool_logs.is_empty() {
        collected_blocks.push(vec![Line::from(
            "Tool log will appear here. Try `/lua rust.list_dir(\".\")`.",
        )]);
    }

    collected_blocks.reverse();
    let lines: Vec<Line> = collected_blocks.into_iter().flatten().collect();

    let mut title = "Tool Activity".to_string();
    let total_lines = estimate_wrapped_height(&lines, inner_width);
    let baseline = total_lines.saturating_sub(inner_height);
    let offset_from_bottom = state.tool_scroll.min(baseline);
    let scroll_top = baseline.saturating_sub(offset_from_bottom);

    if total_lines > inner_height {
        let percent = if baseline == 0 {
            100
        } else {
            let ratio = scroll_top as f64 / baseline as f64;
            (ratio * 100.0).round() as u16
        };
        title = format!("Tool Activity ({percent:>3}%)");
    }

    let block = base_block(
        &title,
        state.focus == FocusTarget::Tool,
        state.copy_mode,
    );

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_top, 0))
        .block(block);

    frame.render_widget(paragraph, area);
}

fn tool_entry_to_lines(entry: &crate::types::ToolLogEntry) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let (icon, style) = match entry.status {
        ToolStatus::Pending => ("⏳", Style::default().fg(Color::Yellow)),
        ToolStatus::Success => ("✅", Style::default().fg(Color::Green)),
        ToolStatus::Error => ("❌", Style::default().fg(Color::Red)),
    };
    
    lines.push(Line::from(vec![
        Span::styled(format!("{icon} "), style),
        Span::styled(
            entry.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));

    if !entry.detail.is_empty() {
        for line_str in entry.detail.lines() {
            let style = if line_str.starts_with("+++") || line_str.starts_with("---") {
                Style::default().add_modifier(Modifier::BOLD)
            } else if line_str.starts_with('+') {
                Style::default().fg(Color::Green)
            } else if line_str.starts_with('-') {
                Style::default().fg(Color::Red)
            } else if line_str.starts_with("@@") {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };

            lines.push(Line::styled(line_str.to_string(), style));
        }
    }
    lines.push(Line::default());
    lines
}

pub fn render_input(frame: &mut Frame, area: Rect, state: &AppState) {
    let border_padding = if state.copy_mode { 0 } else { 2 };
    let inner_width = area.width.saturating_sub(border_padding).max(1);
    
    let mut text = state.input.buffer();
    let show_placeholder = text.is_empty();
    if show_placeholder {
        text.push_str("Type a message, or `/lua <code>` to run Lua.");
    }
    
    let block = base_block("Input", state.focus == FocusTarget::Input, state.copy_mode);
    
    // Horizontal scrolling logic
    let cursor_visual_x = state.input.cursor_display_offset();
    let mut scroll_x = 0;
    
    if !show_placeholder && state.focus == FocusTarget::Input {
        // Ensure cursor is visible.
        // If cursor is at 10, and width is 5. Scroll should be at least 6 (10-5+1).
        // We leave a small margin on the right?
        if cursor_visual_x >= inner_width {
            scroll_x = cursor_visual_x.saturating_sub(inner_width).saturating_add(1);
        }
    }

    let paragraph = Paragraph::new(text)
        .block(block)
        .scroll((0, scroll_x));
        
    frame.render_widget(paragraph, area);

    if state.focus == FocusTarget::Input {
        // Cursor position is relative to the area, minus the scroll.
        // x = area.x + 1 (border) + cursor_visual_x - scroll_x
        // We need to clamp or ensure it's inside.
        let relative_cursor_x = cursor_visual_x.saturating_sub(scroll_x);
        
        // If the cursor is mathematically "visible", we draw it.
        if relative_cursor_x < inner_width {
             let cursor_x = area.x + 1 + relative_cursor_x;
             let cursor_y = area.y + 1;
             frame.set_cursor(cursor_x, cursor_y);
        }
    }
}

fn role_color(role: Role) -> Color {
    match role {
        Role::User => Color::Cyan,
        Role::Assistant => Color::Magenta,
        Role::Tool => Color::Yellow,
    }
}

fn base_block<'a>(title: &'a str, focused: bool, copy_mode: bool) -> Block<'a> {
    if copy_mode {
        Block::default().title(title)
    } else {
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(title);
        if focused {
            block = block
                .border_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .title_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
        } else {
            block = block.border_style(Style::default().fg(Color::DarkGray));
        }
        block
    }
}

fn append_multiline(lines: &mut Vec<Line>, text: &str) {
    let mut segments = text.split('\n').peekable();
    while let Some(line) = segments.next() {
        lines.push(Line::from(line.to_string()));
        if segments.peek().is_none() && line.is_empty() {
            break;
        }
    }
}

fn estimate_wrapped_height(lines: &[Line], width: u16) -> u16 {
    if width == 0 {
        return lines.len() as u16;
    }
    let usable_width = width as usize;
    let mut total: u32 = 0;
    for line in lines {
        let height = estimate_line_height(line, usable_width) as u32;
        total = total.saturating_add(height);
    }
    total.min(u16::MAX as u32) as u16
}

fn estimate_line_height(line: &Line, width: usize) -> u16 {
    if width == 0 {
        return 0;
    }
    let line_width = line.width();
    if line_width == 0 {
        return 1;
    }
    let rows = line_width.div_ceil(width);
    rows.min(u16::MAX as usize) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_multiline_splits_text() {
        let mut lines = Vec::new();
        append_multiline(&mut lines, "one\ntwo\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], Line::from("one"));
        assert_eq!(lines[1], Line::from("two"));
    }

    #[test]
    fn estimate_wrapped_height_accounts_for_width() {
        let lines = vec![Line::from("abcdef")];
        assert_eq!(estimate_wrapped_height(&lines, 3), 2);
        assert_eq!(estimate_wrapped_height(&lines, 10), 1);
    }

    #[test]
    fn tool_entry_to_lines_formats_correctly() {
        let entry = crate::types::ToolLogEntry {
            id: 1,
            title: "Test Tool".to_string(),
            status: ToolStatus::Success,
            detail: "Details here".to_string(),
        };
        let lines = tool_entry_to_lines(&entry);
        assert!(!lines.is_empty());
        assert!(lines[0].spans.iter().any(|s| s.content == "[ok]"));
        assert!(lines[0].spans.iter().any(|s| s.content == "Test Tool"));
        assert_eq!(lines[1], Line::from("Details here"));
    }

    #[test]
    fn tool_entry_to_lines_handles_multiline_detail() {
        let entry = crate::types::ToolLogEntry {
            id: 2,
            title: "Multi".to_string(),
            status: ToolStatus::Pending,
            detail: "Line 1\nLine 2".to_string(),
        };
        let lines = tool_entry_to_lines(&entry);
        // Line 0: Header
        // Line 1: "Line 1"
        // Line 2: "Line 2"
        // Line 3: Empty spacing line
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1], Line::from("Line 1"));
        assert_eq!(lines[2], Line::from("Line 2"));
    }
}
