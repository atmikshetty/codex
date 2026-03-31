use color_eyre::eyre::Result;

use super::App;
use crate::tui;

impl App {
    pub(crate) fn sync_sidebar_mode(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let terminal_width = tui.terminal.size()?.width;
        let should_activate = self
            .chat_widget
            .sidebar_mode_active_for_terminal(terminal_width);
        if self.overlay.is_some() {
            if should_activate {
                self.sidebar_mode_active = true;
            }
            return Ok(());
        }
        if should_activate == self.sidebar_mode_active {
            return Ok(());
        }

        self.sidebar_mode_active = should_activate;

        if should_activate {
            let _ = tui.enter_alt_screen();
        } else {
            let _ = tui.leave_alt_screen();
            self.clear_terminal_ui(tui, /*redraw_header*/ false)?;
            self.render_transcript_once(tui);
        }

        tui.frame_requester().schedule_frame();
        Ok(())
    }
}
