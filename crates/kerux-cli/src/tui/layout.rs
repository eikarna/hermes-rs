use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub(crate) const CONSTRAINED_WIDTH: u16 = 65;
pub(crate) const CONSTRAINED_HEIGHT: u16 = 20;
pub(crate) const DESKTOP_WIDTH: u16 = 120;
pub(crate) const DESKTOP_HEIGHT: u16 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LandingCompactLayout {
    pub title: Rect,
    pub prompt: Rect,
    pub controls: Rect,
    pub status: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LandingConstrainedLayout {
    pub title: Rect,
    pub prompt: Rect,
    pub help: Rect,
    pub status: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DesktopWorkspaceLayout {
    pub header: Rect,
    pub conversation: Rect,
    pub panel: Rect,
    pub reasoning: Rect,
    pub activity: Rect,
    pub footer: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediumWorkspaceLayout {
    pub header: Rect,
    pub conversation: Rect,
    pub tabs: Rect,
    pub panel: Rect,
    pub reasoning: Rect,
    pub footer: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompactWorkspaceLayout {
    pub header: Rect,
    pub tabs: Rect,
    pub content: Rect,
    pub footer: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConstrainedWorkspaceLayout {
    pub header: Rect,
    pub content: Rect,
    pub footer: Rect,
    pub popup: Rect,
}

pub(crate) fn is_constrained(area: Rect) -> bool {
    area.width < CONSTRAINED_WIDTH || area.height < CONSTRAINED_HEIGHT
}

pub(crate) fn build_landing_compact_layout(
    area: Rect,
    is_portrait_like: bool,
) -> LandingCompactLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if is_portrait_like {
            [
                Constraint::Max(4),
                Constraint::Min(7),
                Constraint::Max(4),
                Constraint::Min(1),
                Constraint::Max(2),
            ]
        } else {
            [
                Constraint::Max(4),
                Constraint::Min(7),
                Constraint::Max(3),
                Constraint::Min(1),
                Constraint::Max(2),
            ]
        })
        .split(area);

    LandingCompactLayout {
        title: outer[0],
        prompt: outer[1],
        controls: outer[2],
        status: outer[4],
    }
}

pub(crate) fn build_landing_constrained_layout(area: Rect) -> LandingConstrainedLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Max(2),
            Constraint::Min(4),
            Constraint::Max(2),
            Constraint::Max(1),
        ])
        .split(area);

    LandingConstrainedLayout {
        title: outer[0],
        prompt: outer[1],
        help: outer[2],
        status: outer[3],
    }
}

pub(crate) fn build_desktop_layout(area: Rect) -> DesktopWorkspaceLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(outer[1]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(42),
            Constraint::Percentage(33),
            Constraint::Percentage(25),
        ])
        .split(body[1]);

    DesktopWorkspaceLayout {
        header: outer[0],
        conversation: body[0],
        panel: right[0],
        reasoning: right[1],
        activity: right[2],
        footer: outer[2],
    }
}

pub(crate) fn build_medium_layout(area: Rect) -> MediumWorkspaceLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Ratio(1, 2),
            Constraint::Length(3),
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 6),
            Constraint::Length(4),
        ])
        .split(area);

    MediumWorkspaceLayout {
        header: outer[0],
        conversation: outer[1],
        tabs: outer[2],
        panel: outer[3],
        reasoning: outer[4],
        footer: outer[5],
    }
}

pub(crate) fn build_compact_layout(area: Rect) -> CompactWorkspaceLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(4),
        ])
        .split(area);

    CompactWorkspaceLayout {
        header: outer[0],
        tabs: outer[1],
        content: outer[2],
        footer: outer[3],
    }
}

pub(crate) fn build_constrained_layout(area: Rect) -> ConstrainedWorkspaceLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Max(1), Constraint::Min(3), Constraint::Max(2)])
        .split(area);

    ConstrainedWorkspaceLayout {
        header: outer[0],
        content: outer[1],
        footer: outer[2],
        popup: centered_rect_percent(outer[1], 92, 88, 80, 16),
    }
}

pub(crate) fn centered_rect_percent(
    area: Rect,
    width_percent: u16,
    height_percent: u16,
    max_width: u16,
    max_height: u16,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }

    let width = area
        .width
        .saturating_mul(width_percent.min(100))
        .saturating_div(100)
        .min(max_width)
        .max(1)
        .min(area.width.max(1));
    let height = area
        .height
        .saturating_mul(height_percent.min(100))
        .saturating_div(100)
        .min(max_height)
        .max(1)
        .min(area.height.max(1));

    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landing_compact_layout_keeps_status_last() {
        let layout = build_landing_compact_layout(Rect::new(0, 0, 40, 28), true);

        assert_eq!(layout.title, Rect::new(0, 0, 40, 4));
        assert_eq!(layout.prompt, Rect::new(0, 4, 40, 9));
        assert_eq!(layout.controls, Rect::new(0, 13, 40, 4));
        assert_eq!(layout.status, Rect::new(0, 26, 40, 2));
    }

    #[test]
    fn landing_constrained_layout_keeps_all_regions_visible() {
        let layout = build_landing_constrained_layout(Rect::new(0, 0, 40, 15));

        assert_eq!(layout.title, Rect::new(0, 0, 40, 2));
        assert_eq!(layout.prompt, Rect::new(0, 2, 40, 10));
        assert_eq!(layout.help, Rect::new(0, 12, 40, 2));
        assert_eq!(layout.status, Rect::new(0, 14, 40, 1));
    }
}
