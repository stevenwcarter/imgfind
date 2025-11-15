use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub fn nine_block(area: Rect) -> Vec<Rect> {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage(33),
                Constraint::Percentage(34),
                Constraint::Percentage(33),
            ]
            .as_ref(),
        )
        .split(area);

    let mut areas = Vec::new();

    for area in layout.iter() {
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(
                [
                    Constraint::Percentage(33),
                    Constraint::Percentage(34),
                    Constraint::Percentage(33),
                ]
                .as_ref(),
            )
            .split(*area);

        for h_area in horizontal.iter() {
            areas.push(*h_area);
        }
    }

    areas
}
