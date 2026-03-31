use std::collections::BTreeMap;
use std::path::Path;

use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use tokio::process::Command;
use tokio::time::timeout;

use super::*;

const SIDEBAR_SESSION_TITLE: &str = "Session Title (mock)";
const SIDEBAR_WIDTH: u16 = 38;
const SIDEBAR_GAP: u16 = 1;
const SIDEBAR_MIN_MAIN_WIDTH: u16 = 90;
const SIDEBAR_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const SIDEBAR_GIT_TIMEOUT: Duration = Duration::from_secs(3);
const SIDEBAR_MAX_FILE_ROWS: usize = 8;
const TOKENS_PER_MILLION: f64 = 1_000_000.0;
const INPUT_USD_PER_MILLION: f64 = 1.25;
const CACHED_INPUT_USD_PER_MILLION: f64 = 0.125;
const OUTPUT_USD_PER_MILLION: f64 = 10.0;

impl ChatWidget {
    pub(crate) fn sidebar_enabled(&self) -> bool {
        self.sidebar_enabled
    }

    pub(crate) fn set_sidebar_enabled(&mut self, enabled: bool) {
        if self.sidebar_enabled == enabled {
            return;
        }

        self.sidebar_enabled = enabled;
        if enabled {
            self.request_sidebar_modified_files_refresh();
        }
        self.request_redraw();
    }

    pub(crate) fn sidebar_status_message(&self) -> (String, Option<String>) {
        if !self.sidebar_enabled {
            return ("Sidebar is off.".to_string(), None);
        }

        let hint = self.sidebar_unavailable_hint();
        ("Sidebar is on.".to_string(), hint)
    }

    pub(crate) fn sidebar_toggle_message(&self, enabled: bool) -> (String, Option<String>) {
        if !enabled {
            return ("Sidebar hidden.".to_string(), None);
        }

        (
            "Sidebar shown.".to_string(),
            self.sidebar_unavailable_hint(),
        )
    }

    pub(crate) fn sidebar_mode_active_for_terminal(&self, terminal_width: u16) -> bool {
        self.sidebar_enabled
            && self.thread_id.is_some()
            && Self::sidebar_width_for_terminal(terminal_width).is_some()
    }

    pub(super) fn sidebar_min_terminal_width() -> u16 {
        SIDEBAR_MIN_MAIN_WIDTH
            .saturating_add(SIDEBAR_GAP)
            .saturating_add(SIDEBAR_WIDTH)
    }

    fn sidebar_unavailable_hint(&self) -> Option<String> {
        if self.thread_id.is_none() {
            return Some("Start a session to populate the sidebar.".to_string());
        }

        let terminal_width = self
            .last_rendered_width
            .get()
            .and_then(|width| u16::try_from(width).ok())?;
        if self.sidebar_mode_active_for_terminal(terminal_width) {
            return None;
        }

        Some(format!(
            "Expand the terminal to at least {} columns to pin the sidebar.",
            Self::sidebar_min_terminal_width()
        ))
    }

    pub(super) fn split_main_and_sidebar(&self, area: Rect) -> (Rect, Option<Rect>) {
        if !self.sidebar_mode_active_for_terminal(area.width) {
            return (area, None);
        }

        let sidebar_width = SIDEBAR_WIDTH;
        let reserved = sidebar_width.saturating_add(SIDEBAR_GAP);
        let main_width = area.width.saturating_sub(reserved);
        let main_area = Rect::new(area.x, area.y, main_width, area.height);
        let sidebar_x = area
            .x
            .saturating_add(main_width)
            .saturating_add(SIDEBAR_GAP);
        let sidebar_area = Rect::new(sidebar_x, area.y, sidebar_width, area.height);
        (main_area, Some(sidebar_area))
    }

    pub(super) fn main_content_width_for_terminal(&self, terminal_width: u16) -> u16 {
        terminal_width.saturating_sub(
            Self::sidebar_width_for_terminal(terminal_width)
                .map(|sidebar_width| SIDEBAR_GAP.saturating_add(sidebar_width))
                .unwrap_or_default(),
        )
    }

    pub(super) fn last_rendered_content_width(&self) -> Option<usize> {
        self.last_rendered_width
            .get()
            .and_then(|width| u16::try_from(width).ok())
            .map(|width| usize::from(self.main_content_width_for_terminal(width)))
    }

    pub(super) fn request_sidebar_modified_files_refresh(&mut self) {
        if cfg!(test) {
            return;
        }

        let Some(terminal_width) = self
            .last_rendered_width
            .get()
            .and_then(|width| u16::try_from(width).ok())
        else {
            return;
        };
        if !self.sidebar_mode_active_for_terminal(terminal_width) {
            return;
        }

        let cwd = self.sidebar_cwd().to_path_buf();
        self.sync_sidebar_modified_files_state(&cwd);
        if self.sidebar_modified_files_pending {
            return;
        }
        if self
            .sidebar_modified_files_last_refresh
            .is_some_and(|last| last.elapsed() < SIDEBAR_REFRESH_INTERVAL)
        {
            return;
        }

        self.sidebar_modified_files_pending = true;
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let files = collect_sidebar_modified_files(cwd.clone()).await;
            tx.send(AppEvent::SidebarModifiedFilesUpdated { cwd, files });
        });
    }

    pub(crate) fn set_sidebar_modified_files(
        &mut self,
        cwd: PathBuf,
        files: Vec<SidebarModifiedFile>,
    ) {
        if self.sidebar_modified_files_cwd.as_ref() != Some(&cwd) {
            self.sidebar_modified_files_pending = false;
            return;
        }

        self.sidebar_modified_files = files;
        self.sidebar_modified_files_pending = false;
        self.sidebar_modified_files_last_refresh = Some(Instant::now());
        self.request_redraw();
    }

    pub(super) fn render_sidebar(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().dim());
        let inner = block.inner(area);
        block.render(area, buf);

        let content_area = Rect::new(
            inner.x.saturating_add(1),
            inner.y.saturating_add(1),
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(2),
        );
        if content_area.is_empty() {
            return;
        }

        let lines = self.sidebar_lines(content_area.width as usize, content_area.height as usize);
        Paragraph::new(lines).render(content_area, buf);
    }

    fn sidebar_lines(&self, width: usize, max_height: usize) -> Vec<Line<'static>> {
        let usage = self.status_line_total_usage();
        let context_tokens = usage.tokens_in_context_window().max(0);
        let used_percent = self.status_line_context_used_percent().unwrap_or(0);
        let spent = estimated_sidebar_cost_usd(&usage);
        let session_title = truncate_text(SIDEBAR_SESSION_TITLE, width);

        let mut lines = vec![
            Line::from(vec![Span::from(session_title).bold()]),
            Line::from(""),
            Line::from(vec!["Context".bold()]),
            Line::from(vec![
                Span::from(format!("{} tokens", format_tokens_compact(context_tokens))).dim(),
            ]),
            Line::from(vec![Span::from(format!("{used_percent}% used")).dim()]),
            Line::from(vec![Span::from(format!("${spent:.2} spent")).dim()]),
            Line::from(""),
            Line::from(vec!["LSP".bold()]),
            Line::from(vec![
                Span::from(truncate_text("LSPs will activate as files are read", width)).dim(),
            ]),
            Line::from(""),
            Line::from(vec!["Modified Files".bold()]),
        ];

        if max_height <= lines.len() {
            return lines;
        }

        if self.sidebar_modified_files.is_empty() {
            lines.push(Line::from(vec![Span::from("No modified files yet").dim()]));
            return lines;
        }

        let remaining_rows = max_height.saturating_sub(lines.len());
        if remaining_rows == 0 {
            return lines;
        }

        let max_file_rows = remaining_rows.min(SIDEBAR_MAX_FILE_ROWS);
        let mut visible_rows = max_file_rows.min(self.sidebar_modified_files.len());
        if self.sidebar_modified_files.len() > visible_rows && visible_rows > 0 {
            visible_rows = visible_rows.saturating_sub(1);
        }

        for file in self.sidebar_modified_files.iter().take(visible_rows) {
            lines.push(self.sidebar_modified_file_line(file, width));
        }

        let hidden_count = self
            .sidebar_modified_files
            .len()
            .saturating_sub(visible_rows);
        if hidden_count > 0 {
            lines.push(Line::from(vec![
                Span::from(format!("+{hidden_count} more files")).dim(),
            ]));
        }

        lines
    }

    fn sidebar_modified_file_line(
        &self,
        file: &SidebarModifiedFile,
        width: usize,
    ) -> Line<'static> {
        let additions = (file.additions > 0).then(|| format!("+{}", file.additions));
        let deletions = (file.deletions > 0).then(|| format!("-{}", file.deletions));

        let mut suffix_width = 0usize;
        if let Some(additions) = additions.as_ref() {
            suffix_width += additions.chars().count().saturating_add(1);
        }
        if let Some(deletions) = deletions.as_ref() {
            suffix_width += deletions.chars().count().saturating_add(1);
        }

        let path_width = width.saturating_sub(suffix_width).max(1);
        let path = truncate_text(&file.path, path_width);

        let mut spans = vec![Span::from(path).dim()];
        if let Some(additions) = additions {
            spans.push(" ".into());
            spans.push(additions.green());
        }
        if let Some(deletions) = deletions {
            spans.push(" ".into());
            spans.push(deletions.red());
        }

        Line::from(spans)
    }

    fn sync_sidebar_modified_files_state(&mut self, cwd: &Path) {
        if self
            .sidebar_modified_files_cwd
            .as_ref()
            .is_some_and(|path| path == cwd)
        {
            return;
        }

        self.sidebar_modified_files_cwd = Some(cwd.to_path_buf());
        self.sidebar_modified_files.clear();
        self.sidebar_modified_files_pending = false;
        self.sidebar_modified_files_last_refresh = None;
    }

    fn sidebar_cwd(&self) -> &Path {
        self.current_cwd
            .as_deref()
            .unwrap_or(self.config.cwd.as_path())
    }

    fn sidebar_width_for_terminal(terminal_width: u16) -> Option<u16> {
        (terminal_width >= Self::sidebar_min_terminal_width()).then_some(SIDEBAR_WIDTH)
    }
}

fn estimated_sidebar_cost_usd(usage: &TokenUsage) -> f64 {
    let input = usage.non_cached_input().max(0) as f64;
    let cached_input = usage.cached_input().max(0) as f64;
    let output = usage.output_tokens.max(0) as f64;
    (input / TOKENS_PER_MILLION * INPUT_USD_PER_MILLION)
        + (cached_input / TOKENS_PER_MILLION * CACHED_INPUT_USD_PER_MILLION)
        + (output / TOKENS_PER_MILLION * OUTPUT_USD_PER_MILLION)
}

async fn collect_sidebar_modified_files(cwd: PathBuf) -> Vec<SidebarModifiedFile> {
    if !inside_git_repo(&cwd).await {
        return Vec::new();
    }

    let (unstaged, staged, untracked) = tokio::join!(
        run_git_capture_stdout(
            &cwd,
            &["diff", "--numstat", "--"],
            /*allow_diff_exit*/ true
        ),
        run_git_capture_stdout(
            &cwd,
            &["diff", "--numstat", "--cached", "--"],
            /*allow_diff_exit*/ true,
        ),
        run_git_capture_stdout(
            &cwd,
            &["ls-files", "--others", "--exclude-standard"],
            /*allow_diff_exit*/ false,
        ),
    );

    let mut totals: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    if let Some(unstaged) = unstaged {
        parse_numstat_into_totals(&unstaged, &mut totals);
    }
    if let Some(staged) = staged {
        parse_numstat_into_totals(&staged, &mut totals);
    }
    if let Some(untracked) = untracked {
        for path in untracked
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            totals.entry(path.to_string()).or_insert((0, 0));
        }
    }

    totals
        .into_iter()
        .map(|(path, (additions, deletions))| SidebarModifiedFile {
            path,
            additions,
            deletions,
        })
        .collect()
}

fn parse_numstat_into_totals(output: &str, totals: &mut BTreeMap<String, (usize, usize)>) {
    for line in output.lines() {
        let mut parts = line.split('\t');
        let Some(additions_raw) = parts.next() else {
            continue;
        };
        let Some(deletions_raw) = parts.next() else {
            continue;
        };
        let Some(path_first) = parts.next() else {
            continue;
        };
        let path = std::iter::once(path_first)
            .chain(parts)
            .last()
            .unwrap_or(path_first);
        let additions = additions_raw.parse::<usize>().unwrap_or(0);
        let deletions = deletions_raw.parse::<usize>().unwrap_or(0);
        let entry = totals.entry(path.to_string()).or_insert((0, 0));
        entry.0 += additions;
        entry.1 += deletions;
    }
}

async fn inside_git_repo(cwd: &Path) -> bool {
    run_git_capture_stdout(
        cwd,
        &["rev-parse", "--is-inside-work-tree"],
        /*allow_diff_exit*/ false,
    )
    .await
    .is_some_and(|stdout| stdout.trim() == "true")
}

async fn run_git_capture_stdout(
    cwd: &Path,
    args: &[&str],
    allow_diff_exit: bool,
) -> Option<String> {
    let output = timeout(
        SIDEBAR_GIT_TIMEOUT,
        Command::new("git").args(args).current_dir(cwd).output(),
    )
    .await
    .ok()?
    .ok()?;

    let success = output.status.success() || (allow_diff_exit && output.status.code() == Some(1));
    if !success {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}
