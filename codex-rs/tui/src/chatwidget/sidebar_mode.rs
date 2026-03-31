use std::sync::Arc;

use ratatui::text::Text;
use ratatui::widgets::Widget;

use super::*;

const SIDEBAR_MAIN_GAP: u16 = 1;

impl ChatWidget {
    pub(crate) fn render_sidebar_mode(
        &self,
        area: Rect,
        buf: &mut Buffer,
        transcript_cells: &[Arc<dyn HistoryCell>],
    ) {
        let (main_area, sidebar_area) = self.split_main_and_sidebar(area);
        self.render_sidebar_main(main_area, buf, transcript_cells);
        if let Some(sidebar_area) = sidebar_area {
            self.render_sidebar(sidebar_area, buf);
        }
        self.last_rendered_width.set(Some(area.width as usize));
    }

    pub(crate) fn sidebar_mode_cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let (main_area, _sidebar_area) = self.split_main_and_sidebar(area);
        let (_transcript_area, bottom_area) = self.sidebar_main_layout(main_area);
        self.bottom_pane.cursor_pos(bottom_area)
    }

    fn render_sidebar_main(
        &self,
        area: Rect,
        buf: &mut Buffer,
        transcript_cells: &[Arc<dyn HistoryCell>],
    ) {
        if area.is_empty() {
            return;
        }

        let (transcript_area, bottom_area) = self.sidebar_main_layout(area);
        if !transcript_area.is_empty() {
            self.render_sidebar_transcript(transcript_area, buf, transcript_cells);
        }
        if !bottom_area.is_empty() {
            self.bottom_pane.render(bottom_area, buf);
        }
    }

    fn sidebar_main_layout(&self, area: Rect) -> (Rect, Rect) {
        if area.is_empty() {
            return (Rect::default(), Rect::default());
        }

        let bottom_height = self.bottom_pane.desired_height(area.width).min(area.height);
        let gap = if bottom_height < area.height {
            SIDEBAR_MAIN_GAP
        } else {
            0
        };
        let transcript_height = area
            .height
            .saturating_sub(bottom_height)
            .saturating_sub(gap);
        let transcript_area = Rect::new(area.x, area.y, area.width, transcript_height);
        let bottom_y = area.y.saturating_add(transcript_height).saturating_add(gap);
        let bottom_area = Rect::new(
            area.x,
            bottom_y,
            area.width,
            area.height
                .saturating_sub(transcript_height)
                .saturating_sub(gap),
        );
        (transcript_area, bottom_area)
    }

    fn render_sidebar_transcript(
        &self,
        area: Rect,
        buf: &mut Buffer,
        transcript_cells: &[Arc<dyn HistoryCell>],
    ) {
        let lines = self.sidebar_transcript_lines(area.width, transcript_cells);
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        let scroll = if area.height == 0 {
            0
        } else {
            paragraph
                .line_count(area.width)
                .saturating_sub(usize::from(area.height))
        };
        paragraph
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(area, buf);
    }

    fn sidebar_transcript_lines(
        &self,
        width: u16,
        transcript_cells: &[Arc<dyn HistoryCell>],
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        for (index, cell) in transcript_cells.iter().enumerate() {
            if index > 0 && !cell.is_stream_continuation() {
                lines.push(Line::from(""));
            }
            lines.extend(cell.transcript_lines(width));
        }

        if let Some(active_lines) = self.active_cell_transcript_lines(width)
            && !active_lines.is_empty()
        {
            if let Some(cell) = self.active_cell.as_ref()
                && !cell.is_stream_continuation()
                && !lines.is_empty()
            {
                lines.push(Line::from(""));
            }
            lines.extend(active_lines);
        }

        lines
    }
}
