mod components;

use ratatui::{
    Frame,
    prelude::*,
    widgets::Paragraph,
};
use taffy::{
    prelude::{TaffyTree, NodeId, AvailableSpace, Size, length, percent, auto},
    style::{Style as TaffyStyle, FlexDirection},
};

use crate::app::{AppState, FocusTarget};

pub fn draw(frame: &mut Frame, state: &AppState) {
    let (chat_area, tool_area, input_area) = calculate_layout(frame.size());

    components::render_chat(frame, chat_area, state);
    components::render_tool_logs(frame, tool_area, state);
    components::render_input(frame, input_area, state);

    render_focus_hint(frame, input_area, state.focus);
}

fn calculate_layout(area: Rect) -> (Rect, Rect, Rect) {
    let mut tree: TaffyTree<()> = TaffyTree::new();

    // Input style: height 3, width 100%
    let input_style = TaffyStyle {
        size: Size { width: percent(1.0), height: length(3.0) },
        ..Default::default()
    };

    // Top container style: flex grow 1, flex direction row
    let top_container_style = TaffyStyle {
        flex_grow: 1.0,
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0), height: auto() },
        ..Default::default()
    };

    // Chat style: width 60%
    let chat_style = TaffyStyle {
        size: Size { width: percent(0.6), height: percent(1.0) },
        ..Default::default()
    };

    // Tools style: width 40%
    let tools_style = TaffyStyle {
        size: Size { width: percent(0.4), height: percent(1.0) },
        ..Default::default()
    };

    // Root style: column, width/height of area
    let root_style = TaffyStyle {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(area.width as f32), height: length(area.height as f32) },
        ..Default::default()
    };

    // Create nodes
    let chat_node = tree.new_leaf(chat_style).unwrap();
    let tools_node = tree.new_leaf(tools_style).unwrap();
    let top_node = tree.new_with_children(top_container_style, &[chat_node, tools_node]).unwrap();
    let input_node = tree.new_leaf(input_style).unwrap();

    let root_node = tree.new_with_children(root_style, &[top_node, input_node]).unwrap();

    // Compute layout
    tree.compute_layout(
        root_node,
        Size {
            width: AvailableSpace::Definite(area.width as f32),
            height: AvailableSpace::Definite(area.height as f32),
        },
    )
    .unwrap();

    // Helper to extract global position
    let get_rect = |node: NodeId, parent_x: f32, parent_y: f32| -> Rect {
        let layout = tree.layout(node).unwrap();
        Rect {
            x: (parent_x + layout.location.x) as u16,
            y: (parent_y + layout.location.y) as u16,
            width: layout.size.width as u16,
            height: layout.size.height as u16,
        }
    };

    let top_layout = tree.layout(top_node).unwrap();
    // top node relative to root
    let top_x = area.x as f32 + top_layout.location.x;
    let top_y = area.y as f32 + top_layout.location.y;

    let chat_rect = get_rect(chat_node, top_x, top_y);
    let tool_rect = get_rect(tools_node, top_x, top_y);

    let input_rect = get_rect(input_node, area.x as f32, area.y as f32); // input is child of root

    (chat_rect, tool_rect, input_rect)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_calculation_is_correct() {
        let area = Rect::new(0, 0, 200, 50);
        let (chat, tool, input) = calculate_layout(area);

        // Input fixed height 3 at bottom
        assert_eq!(input.height, 3, "Input height");
        assert_eq!(input.width, 200, "Input width");
        assert_eq!(input.y, 47, "Input y position"); // 50 - 3

        // Chat 60% of remaining 47 height. 60% of 200 = 120.
        assert_eq!(chat.width, 120, "Chat width");
        assert_eq!(chat.height, 47, "Chat height");
        assert_eq!(chat.x, 0, "Chat x position");

        // Tool 40% of remaining 47 height. 40% of 200 = 80.
        assert_eq!(tool.width, 80, "Tool width");
        assert_eq!(tool.height, 47, "Tool height");
        assert_eq!(tool.x, 120, "Tool x position");
    }
}
