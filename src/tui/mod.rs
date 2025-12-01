mod components;
mod market;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    prelude::*,
    widgets::{Block, Borders, Paragraph, Tabs},
};

use crate::app::{AppState, FocusTarget, RightPanelTab};

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

    let right_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(horizontal[1]);

    render_tabs(frame, right_layout[0], state);

    match state.active_tab {
        RightPanelTab::ToolLogs => components::render_tool_logs(frame, right_layout[1], state),
        RightPanelTab::MarketData => market::render_market_data(frame, right_layout[1], &state.market_context),
    }

    components::render_input(frame, vertical[1], state);

    render_focus_hint(frame, vertical[1], state.focus);
}

fn render_tabs(frame: &mut Frame, area: Rect, state: &AppState) {
    let titles = vec![RightPanelTab::ToolLogs.title(), RightPanelTab::MarketData.title()];
    let selected = match state.active_tab {
        RightPanelTab::ToolLogs => 0,
        RightPanelTab::MarketData => 1,
    };

    let block = if state.focus == FocusTarget::Tool {
        Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan))
    } else {
        Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray))
    };

    let tabs = Tabs::new(titles)
        .block(block)
        .select(selected)
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

    frame.render_widget(tabs, area);
}

fn render_focus_hint(frame: &mut Frame, area: Rect, focus: FocusTarget) {
    let hint = match focus {
        FocusTarget::Chat => "Focus: chat • Tab to move • Up/Down to scroll",
        FocusTarget::Tool => "Focus: tools • Tab to move • Up/Down to scroll",
        FocusTarget::Input => "Focus: input • /review • /config • @macro • /lua",
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
