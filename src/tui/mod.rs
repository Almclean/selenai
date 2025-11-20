mod components;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    prelude::*,
    widgets::Paragraph,
};

use crate::app::{AppState, FocusTarget};

pub fn draw(frame: &mut Frame, state: &AppState) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(3)])
        .split(frame.size());

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(vertical[0]);

    components::render_chat(frame, horizontal[0], state);
    components::render_tool_logs(frame, horizontal[1], state);
    components::render_input(frame, vertical[1], state);

    render_focus_hint(frame, vertical[1], state.focus);
}

fn render_focus_hint(frame: &mut Frame, area: Rect, focus: FocusTarget) {
    let hint = match focus {
        FocusTarget::Chat => "Focus: chat • Tab to move • Up/Down to scroll",
        FocusTarget::Tool => "Focus: tools • Tab to move • Up/Down to scroll",
        FocusTarget::Input => "Focus: input • Enter to submit • /lua <code> to run",
    };

    let info_area = Rect {
        x: area.x,
        y: area.y.saturating_sub(1),
        width: area.width,
        height: 1,
    };

    let paragraph = Paragraph::new(hint).alignment(Alignment::Right);
    frame.render_widget(paragraph, info_area);
}
