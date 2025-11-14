mod components;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    prelude::*,
    widgets::Paragraph,
};

use crate::app::{AppState, FocusTarget};

pub fn draw(frame: &mut Frame, state: &AppState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(7),
            Constraint::Length(3),
        ])
        .split(frame.size());

    components::render_chat(frame, layout[0], state);
    components::render_tool_logs(frame, layout[1], state);
    components::render_input(frame, layout[2], state);

    render_focus_hint(frame, layout[2], state.focus);
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
