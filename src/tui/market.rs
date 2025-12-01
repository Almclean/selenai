use ratatui::{
    prelude::*,
    widgets::{Block, Borders, ListItem, Paragraph, Row, Table, List},
};
use crate::types::MarketContext;

pub fn render_market_data(frame: &mut Frame, area: Rect, context: &MarketContext) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Market Data")
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if context.active_ticker.is_none() {
        let p = Paragraph::new("No active ticker.\nUse /context <TICKER> or ask about a stock.")
            .alignment(Alignment::Center)
            .wrap(ratatui::widgets::Wrap { trim: true });
        frame.render_widget(p, inner);
        return;
    }

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // Header
            Constraint::Length(8), // Stats
            Constraint::Min(0),    // News
        ])
        .split(inner);

    // 1. Header
    let ticker = context.active_ticker.as_deref().unwrap_or("???");
    let price = context.price.unwrap_or(0.0);

    let header_text = vec![
        Line::from(vec![
            Span::styled(ticker, Style::default().add_modifier(Modifier::BOLD).fg(Color::Yellow)),
            Span::raw(" "),
            Span::styled(format!("${:.2}", price), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(Span::styled(&context.technical_summary, Style::default().fg(Color::DarkGray))),
    ];

    let header_p = Paragraph::new(header_text)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::BOTTOM));

    frame.render_widget(header_p, vertical[0]);

    // 2. Stats
    let mut rows = Vec::new();
    if let Some(vol) = context.volume {
        rows.push(Row::new(vec!["Volume".to_string(), format!("{}", vol)]));
    }
    if let Some(cap) = &context.market_cap {
        rows.push(Row::new(vec!["Market Cap".to_string(), cap.clone()]));
    }
    if let Some(change) = context.change_percent {
        let color = if change >= 0.0 { Color::Green } else { Color::Red };
        rows.push(Row::new(vec![
            "Change %".to_string(),
            format!("{:.2}%", change)
        ]).style(Style::default().fg(color)));
    }

    // Fill empty if no data
    if rows.is_empty() {
        rows.push(Row::new(vec!["No detailed stats".to_string(), "".to_string()]));
    }

    let table = Table::new(rows, [Constraint::Percentage(40), Constraint::Percentage(60)])
        .block(Block::default().borders(Borders::BOTTOM).title("Key Statistics"));
    frame.render_widget(table, vertical[1]);

    // 3. News
    let news_items: Vec<ListItem> = if context.headlines.is_empty() {
        vec![ListItem::new("No recent news loaded.")]
    } else {
        context.headlines.iter()
            .map(|h| ListItem::new(Line::from(vec![Span::styled("• ", Style::default().fg(Color::Blue)), Span::raw(h)])))
            .collect()
    };

    let news_list = List::new(news_items)
         .block(Block::default().title("News / Analysis"));
    frame.render_widget(news_list, vertical[2]);
}
