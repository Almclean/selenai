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
    r"  ____________________.____     ___________ _______      _____  .___ ",
    r" /   _____/\_   _____/|    |    \_   _____/ \      \    /  _  \ |   |",
    r" \_____  \  |    __)_ |    |     |    __)_  /   |   \  /  /_\  \|   |",
    r" /        \ |        \|    |___  |        \/    |    \/    |    \   |",
    r"/_______  //_______  /|_______ \/_______  /\____|__  /\____|__  /___|",
    r"        \/         \/         \/        \/         \/         \/     ",
    r"                            S E L E N A I V0.01",
];

pub fn render_chat(frame: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();
    let banner_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    for text in SELENAI_BANNER {
        lines.push(Line::from(Span::styled(*text, banner_style)));
    }
    lines.push(Line::default());
    for message in &state.messages {
        lines.push(Line::from(vec![Span::styled(
            message.role.display_name(),
            Style::default()
                .fg(role_color(message.role))
                .add_modifier(Modifier::BOLD),
        )]));
        append_multiline(&mut lines, &message.content);
        lines.push(Line::default());
    }
    if lines.is_empty() {
        lines.push(Line::from("No messages yet. Type below to get started."));
    }

    let mut title = "Conversation".to_string();
    let border_padding = if state.copy_mode { 0 } else { 2 };
    let inner_height = area.height.saturating_sub(border_padding).max(1);
    let inner_width = area.width.saturating_sub(border_padding).max(1);
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

pub fn render_tool_logs(frame: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();
    for entry in &state.tool_logs {
        let status_style = match entry.status {
            ToolStatus::Pending => Style::default().fg(Color::Yellow),
            ToolStatus::Success => Style::default().fg(Color::Green),
            ToolStatus::Error => Style::default().fg(Color::Red),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("[{}]", entry.status.as_str()), status_style),
            Span::raw(" "),
            Span::styled(
                entry.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        if !entry.detail.is_empty() {
            append_multiline(&mut lines, &entry.detail);
        }
        lines.push(Line::default());
    }
    if lines.is_empty() {
        lines.push(Line::from(
            "Tool log will appear here. Try `/lua rust.list_dir(\".\")`.",
        ));
    }

    let block = base_block(
        "Tool activity",
        state.focus == FocusTarget::Tool,
        state.copy_mode,
    );
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((state.tool_scroll, 0))
        .block(block);

    frame.render_widget(paragraph, area);
}

pub fn render_input(frame: &mut Frame, area: Rect, state: &AppState) {
    let mut text = state.input.buffer();
    if text.is_empty() {
        text.push_str("Type a message, or `/lua <code>` to run Lua.");
    }
    let block = base_block("Input", state.focus == FocusTarget::Input, state.copy_mode);
    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);

    if state.focus == FocusTarget::Input {
        let cursor_x = area.x + 1 + state.input.cursor_display_offset();
        let cursor_y = area.y + 1;
        frame.set_cursor(cursor_x, cursor_y);
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
        let mut block = Block::default().borders(Borders::ALL).title(title);
        if focused {
            block = block.border_style(Style::default().fg(Color::Cyan));
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
}
