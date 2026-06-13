use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind, EnableMouseCapture, DisableMouseCapture};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::prelude::*;
use ratatui::widgets::*;

use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};

use crate::core::toc::{self, TocEntry};

/// Represents a single line element in the rendered content.
/// Lines can be either text (rendered as ratatui Lines) or images (rendered as StatefulImage).
enum ContentElement {
    /// A text line. When `wrap` is true, the line is word-wrapped to the available
    /// content width at render time (paragraphs, list items, blockquotes, headings).
    /// When false, the line is rendered as-is and truncated if it exceeds the width
    /// (code block lines, table rows, horizontal rules, heading underlines, placeholders).
    TextLine {
        line: Line<'static>,
        wrap: bool,
    },
    /// An image element that spans a number of rows in the terminal.
    /// Stores the stateful protocol, alt text (for fallback), and the desired height in rows.
    Image {
        protocol: StatefulProtocol,
        _alt: String,
        height: u16,
    },
}

impl ContentElement {
    /// Returns the number of terminal rows this element occupies at the given content width.
    /// For non-wrapping text and images, this is independent of width.
    /// For wrapping text, this counts the wrapped sub-lines.
    fn row_height(&self, width: u16) -> u16 {
        match self {
            ContentElement::TextLine { wrap: false, .. } => 1,
            ContentElement::TextLine { line, wrap: true } => wrapped_line_count(line, width),
            ContentElement::Image { height, .. } => *height,
        }
    }

    /// Returns the concatenated text of a text element, or an empty string for images.
    /// Used for search matching and TOC heading lookup.
    fn text(&self) -> String {
        match self {
            ContentElement::TextLine { line, .. } => {
                line.spans.iter().map(|s| s.content.as_ref()).collect()
            }
            ContentElement::Image { .. } => String::new(),
        }
    }
}


/// Characters prepended to wrap-continuation sub-lines so soft wraps are
/// visually distinct from real indentation.
const WRAP_LINEBREAKCHARS: &str = "++++";
const WRAP_LINEBREAKCHARS_WIDTH: u16 = 4;

/// Per-row widths used by the wrap algorithm: full width for the first row,
/// minus `WRAP_LINEBREAKCHARS_WIDTH` for continuations. Falls back to equal
/// widths (no prefix) when the terminal is too narrow to spare the columns.
fn wrap_widths(width: u16) -> (u16, u16) {
    if width > WRAP_LINEBREAKCHARS_WIDTH + 1 {
        (width, width - WRAP_LINEBREAKCHARS_WIDTH)
    } else {
        (width, width)
    }
}

/// Number of terminal rows a line will occupy when word-wrapped to `width`.
fn wrapped_line_count(line: &Line<'_>, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let (first, cont) = wrap_widths(width);
    split_wrap_offsets(&text, first, cont).len().max(1) as u16
}

/// Greedy word-wrap. Returns char-index ranges that each fit in the active
/// row width: `first_width` for the first row, `cont_width` for continuations.
/// Long single words hard-break at the boundary. Always returns ≥ 1 range.
fn split_wrap_offsets(text: &str, first_width: u16, cont_width: u16) -> Vec<(usize, usize)> {
    let first_max = (first_width as usize).max(1);
    let cont_max = (cont_width as usize).max(1);
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![(0, 0)];
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut row_start: usize = 0;
    let mut col: usize = 0;
    let mut last_break: Option<usize> = None; // char index of last whitespace within current row
    let mut i: usize = 0;
    let mut max = first_max;
    while i < chars.len() {
        let ch = chars[i];
        if col == max {
            // Need to wrap before consuming this char.
            let break_at = last_break.unwrap_or(i);
            ranges.push((row_start, break_at));
            // Skip a single whitespace at the break point (it was the separator).
            let mut next = break_at;
            if next < chars.len() && next < i && chars[next].is_whitespace() {
                next += 1;
            }
            row_start = next;
            i = next;
            col = 0;
            last_break = None;
            max = cont_max;
            continue;
        }
        if ch == ' ' || ch == '\t' {
            last_break = Some(i);
        }
        col += 1;
        i += 1;
    }
    ranges.push((row_start, chars.len()));
    ranges
}

/// Wrap a styled `Line` into a sequence of `Line`s, each fitting within `width`
/// columns. Span styling is preserved within sub-lines; whitespace at sub-line
/// boundaries is consumed (as ratatui's `Wrap { trim: false }` does for hard
/// breaks at space characters). Continuation sub-lines are prefixed with
/// `WRAP_LINEBREAKCHARS` (dimmed) so soft wraps are visually distinct.
fn wrap_line_into_sublines(line: &Line<'_>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::from("")];
    }
    let full_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let (first_width, cont_width) = wrap_widths(width);
    let offsets = split_wrap_offsets(&full_text, first_width, cont_width);
    if offsets.is_empty() {
        return vec![Line::from("")];
    }
    let linebreakchars_active = cont_width < first_width;
    let linebreakchars_style = Style::default().add_modifier(Modifier::DIM);

    // Build a flat (char_index, char, style) view of the original line so we can
    // slice it into sub-lines while preserving per-span styling.
    let chars_all: Vec<char> = full_text.chars().collect();
    let mut char_style: Vec<Style> = Vec::with_capacity(chars_all.len());
    for span in &line.spans {
        let style = span.style;
        for _ in span.content.chars() {
            char_style.push(style);
        }
    }
    debug_assert_eq!(char_style.len(), chars_all.len());

    let mut sublines: Vec<Line<'static>> = Vec::with_capacity(offsets.len());
    for (sub_idx, (start, end)) in offsets.into_iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if sub_idx > 0 && linebreakchars_active {
            spans.push(Span::styled(
                WRAP_LINEBREAKCHARS.to_string(),
                linebreakchars_style,
            ));
        }
        if start >= end {
            sublines.push(Line::from(spans));
            continue;
        }
        // Group consecutive chars of the same style into spans.
        let mut run_start = start;
        let mut run_style = char_style[start];
        for j in (start + 1)..end {
            if char_style[j] != run_style {
                let s: String = chars_all[run_start..j].iter().collect();
                spans.push(Span::styled(s, run_style));
                run_start = j;
                run_style = char_style[j];
            }
        }
        let s: String = chars_all[run_start..end].iter().collect();
        spans.push(Span::styled(s, run_style));
        sublines.push(Line::from(spans));
    }
    sublines
}

pub fn run(file_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(&file_path)?;
    let toc_entries = toc::extract_toc(&content);

    // Bail out if stdout is not a TTY. On Unix, enable_raw_mode() errors on a
    // pipe so the loop never starts; on Windows it succeeds and the event poll
    // would spin forever (which previously hung CI for 6h).
    if !io::stdout().is_terminal() {
        return Err("tui backend requires a terminal (stdout is not a TTY)".into());
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Initialize the image picker for protocol detection.
    // from_query_stdio should be called after entering the alternate screen.
    let picker = Picker::from_query_stdio().ok();

    let rendered = build_content_elements(&content, &file_path, &picker);
    let watcher_rx = crate::core::watcher::watch_file(&file_path)?;

    let mut app = TuiApp {
        content,
        rendered,
        toc_entries,
        file_path,
        watcher_rx,
        picker,
        scroll_offset: 0,
        toc_selected: 0,
        focus_toc: false,
        should_quit: false,
        search_mode: false,
        search_query: String::new(),
        search_matches: Vec::new(),
        current_match_idx: 0,
        content_width: 0,
        cum_rows: Vec::new(),
        // Sentinel: a width that no real layout will ever use, so the first
        // `ensure_metrics` call always recomputes.
        metrics_width: u16::MAX,
    };

    // Main loop
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        // Check for file changes
        if app.watcher_rx.try_recv().is_ok() {
            while app.watcher_rx.try_recv().is_ok() {}
            if let Ok(new_content) = std::fs::read_to_string(&app.file_path) {
                app.toc_entries = toc::extract_toc(&new_content);
                app.rendered = build_content_elements(&new_content, &app.file_path, &app.picker);
                app.content = new_content;
            }
        }

        // Poll events with 100ms timeout for file watching
        if event::poll(std::time::Duration::from_millis(100))? {
            let ev = event::read()?;
            // Resize events are handled implicitly: `ui()` recomputes the
            // wrapped row total at the new width and re-clamps `scroll_offset`,
            // so we don't need to do anything special here.
            // Handle mouse scroll
            if let Event::Mouse(mouse) = &ev {
                match mouse.kind {
                    MouseEventKind::ScrollDown => {
                        app.scroll_offset = app.scroll_offset.saturating_add(3);
                    }
                    MouseEventKind::ScrollUp => {
                        app.scroll_offset = app.scroll_offset.saturating_sub(3);
                    }
                    _ => {}
                }
            }
            if let Event::Key(key) = ev {
                if app.search_mode {
                    match key.code {
                        KeyCode::Esc => {
                            app.search_mode = false;
                            app.search_query.clear();
                            app.search_matches.clear();
                            app.current_match_idx = 0;
                        }
                        KeyCode::Enter => {
                            if !app.search_matches.is_empty() {
                                app.current_match_idx = (app.current_match_idx + 1) % app.search_matches.len();
                                let m = app.search_matches[app.current_match_idx];
                                app.scroll_offset = app.match_row(&m);
                            }
                        }
                        KeyCode::Backspace => {
                            app.search_query.pop();
                            update_search_matches(&mut app);
                        }
                        KeyCode::Char(c) => {
                            app.search_query.push(c);
                            update_search_matches(&mut app);
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.should_quit = true;
                        }
                        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.search_mode = true;
                        }
                        KeyCode::Char('/') => {
                            app.search_mode = true;
                        }
                        KeyCode::Char('n') => {
                            if !app.search_matches.is_empty() {
                                app.current_match_idx = (app.current_match_idx + 1) % app.search_matches.len();
                                let m = app.search_matches[app.current_match_idx];
                                app.scroll_offset = app.match_row(&m);
                            }
                        }
                        KeyCode::Char('N') => {
                            if !app.search_matches.is_empty() {
                                app.current_match_idx = if app.current_match_idx == 0 {
                                    app.search_matches.len() - 1
                                } else {
                                    app.current_match_idx - 1
                                };
                                let m = app.search_matches[app.current_match_idx];
                                app.scroll_offset = app.match_row(&m);
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if app.focus_toc {
                                if app.toc_selected < app.toc_entries.len().saturating_sub(1) {
                                    app.toc_selected += 1;
                                }
                            } else {
                                app.scroll_offset = app.scroll_offset.saturating_add(1);
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if app.focus_toc {
                                app.toc_selected = app.toc_selected.saturating_sub(1);
                            } else {
                                app.scroll_offset = app.scroll_offset.saturating_sub(1);
                            }
                        }
                        KeyCode::PageDown | KeyCode::Char(' ') => {
                            app.scroll_offset = app.scroll_offset.saturating_add(20);
                        }
                        KeyCode::PageUp => {
                            app.scroll_offset = app.scroll_offset.saturating_sub(20);
                        }
                        KeyCode::Home | KeyCode::Char('g') => {
                            app.scroll_offset = 0;
                        }
                        KeyCode::End | KeyCode::Char('G') => {
                            app.scroll_offset = app.total_rows().saturating_sub(1);
                        }
                        KeyCode::Tab => {
                            app.focus_toc = !app.focus_toc;
                        }
                        KeyCode::Enter => {
                            if app.focus_toc {
                                if let Some(elem_idx) = find_heading_element(&app.rendered, &app.toc_entries, app.toc_selected) {
                                    app.scroll_offset = app.element_start_row(elem_idx);
                                    app.focus_toc = false;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    Ok(())
}

struct TuiApp {
    content: String,
    rendered: Vec<ContentElement>,
    toc_entries: Vec<TocEntry>,
    file_path: PathBuf,
    watcher_rx: Receiver<()>,
    picker: Option<Picker>,
    scroll_offset: usize,
    toc_selected: usize,
    focus_toc: bool,
    should_quit: bool,
    search_mode: bool,
    search_query: String,
    search_matches: Vec<SearchMatch>,
    current_match_idx: usize,
    /// Width of the content area (excluding borders), updated each draw.
    /// Cached so event handlers can compute width-aware row offsets without
    /// reaching into the layout.
    content_width: u16,
    /// Cumulative starting rows for each element at `metrics_width`. Length is
    /// `rendered.len() + 1`; `cum_rows[i]` is the absolute row where element
    /// `i` starts, and `cum_rows.last()` is the total row count. Recomputed
    /// only when width or content changes (see `ensure_metrics`).
    cum_rows: Vec<usize>,
    /// Width at which `cum_rows` was computed; sentinel for cache invalidation.
    metrics_width: u16,
}

impl TuiApp {
    /// Refresh `cum_rows` if it's stale (width or content changed). After
    /// `ui()` calls this once per frame, all row-math methods are O(1) lookups.
    fn ensure_metrics(&mut self, width: u16) {
        if self.metrics_width == width && self.cum_rows.len() == self.rendered.len() + 1 {
            return;
        }
        self.cum_rows = Vec::with_capacity(self.rendered.len() + 1);
        self.cum_rows.push(0);
        let mut acc = 0usize;
        for e in &self.rendered {
            acc += e.row_height(width) as usize;
            self.cum_rows.push(acc);
        }
        self.metrics_width = width;
    }

    fn total_rows(&self) -> usize {
        *self.cum_rows.last().unwrap_or(&0)
    }

    fn element_start_row(&self, idx: usize) -> usize {
        *self.cum_rows.get(idx).unwrap_or(&0)
    }

    /// Resolve a `SearchMatch` to its absolute row using the cached prefix sum
    /// for the element start, plus a fresh sub-line lookup within the element.
    fn match_row(&self, m: &SearchMatch) -> usize {
        let base = self.element_start_row(m.element_idx);
        let Some(elem) = self.rendered.get(m.element_idx) else { return base; };
        if let ContentElement::TextLine { wrap: true, .. } = elem {
            let text = elem.text();
            let (first, cont) = wrap_widths(self.metrics_width.max(1));
            let offsets = split_wrap_offsets(&text, first, cont);
            for (j, (start, end)) in offsets.iter().enumerate() {
                if m.char_offset >= *start && m.char_offset < *end {
                    return base + j;
                }
            }
            return base + offsets.len().saturating_sub(1);
        }
        base
    }
}

/// Location of a search hit, anchored to the element + character offset within it
/// rather than to an absolute row. Row offsets change with terminal width once
/// wrapping is enabled, so we resolve to a row only at jump time using the
/// current `content_width`.
#[derive(Clone, Copy)]
struct SearchMatch {
    element_idx: usize,
    /// Character offset of the match within the element's concatenated text.
    /// For non-wrapping or empty elements this is 0.
    char_offset: usize,
}

fn update_search_matches(app: &mut TuiApp) {
    app.search_matches.clear();
    app.current_match_idx = 0;
    if app.search_query.is_empty() {
        return;
    }
    let query_lower = app.search_query.to_lowercase();
    for (idx, element) in app.rendered.iter().enumerate() {
        let text = element.text();
        if text.is_empty() {
            continue;
        }
        let text_lower = text.to_lowercase();
        if let Some(byte_offset) = text_lower.find(&query_lower) {
            // Convert byte offset to char offset (matches `split_wrap_offsets` units).
            let char_offset = text_lower[..byte_offset].chars().count();
            app.search_matches.push(SearchMatch { element_idx: idx, char_offset });
        }
    }
    // Auto-scroll to first match.
    if let Some(first) = app.search_matches.first().copied() {
        app.scroll_offset = app.match_row(&first);
    }
}

/// Calculate the total number of terminal rows occupied by all content elements
/// at the given content width. Production code reads this through the cached
/// `TuiApp::total_rows` / `element_start_row`; kept here as a non-caching oracle
/// for tests.
#[cfg(test)]
fn total_content_rows(elements: &[ContentElement], width: u16) -> usize {
    elements.iter().map(|e| e.row_height(width) as usize).sum()
}

/// Resolve a `SearchMatch` to an absolute row offset at the given width.
/// Production code uses `TuiApp::match_row` which shares the cached prefix sum;
/// this is the non-caching oracle used by tests.
#[cfg(test)]
fn match_to_row(elements: &[ContentElement], m: &SearchMatch, width: u16) -> usize {
    let mut row: usize = 0;
    for (i, e) in elements.iter().enumerate() {
        if i == m.element_idx {
            if let ContentElement::TextLine { wrap: true, .. } = e {
                let text = e.text();
                let (first, cont) = wrap_widths(width);
                let offsets = split_wrap_offsets(&text, first, cont);
                for (j, (start, end)) in offsets.iter().enumerate() {
                    if m.char_offset >= *start && m.char_offset < *end {
                        return row + j;
                    }
                }
                // Match at the very end: land on the last sub-line.
                return row + offsets.len().saturating_sub(1);
            }
            return row;
        }
        row += e.row_height(width) as usize;
    }
    row
}


fn ui(f: &mut Frame, app: &mut TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(30),
            Constraint::Min(1),
        ])
        .split(f.area());

    // TOC sidebar
    let toc_items: Vec<ListItem> = app.toc_entries.iter().map(|entry| {
        let indent = "  ".repeat((entry.level as usize).saturating_sub(1));
        let style = match entry.level {
            1 => Style::default().fg(Color::Cyan).bold(),
            2 => Style::default().fg(Color::Blue).bold(),
            3 => Style::default().fg(Color::White),
            _ => Style::default().fg(Color::DarkGray),
        };
        ListItem::new(format!("{}{}", indent, entry.text)).style(style)
    }).collect();

    let toc_border_style = if app.focus_toc {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let toc = List::new(toc_items)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(toc_border_style)
            .title(" TOC ")
            .title_style(Style::default().bold()))
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol(">> ");

    let mut toc_state = ListState::default();
    if app.focus_toc {
        toc_state.select(Some(app.toc_selected));
    }
    f.render_stateful_widget(toc, chunks[0], &mut toc_state);

    // Main content area
    let content_area = chunks[1];
    let inner_area = Block::default()
        .borders(Borders::ALL)
        .border_style(if !app.focus_toc {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(format!(" {} ", app.file_path.display()))
        .title_style(Style::default().bold())
        .inner(content_area);

    let content_height = inner_area.height as usize;
    let content_width = inner_area.width;
    app.content_width = content_width;
    // Refresh the row-metrics cache exactly once per frame; downstream code
    // (this fn + event handlers) reads totals/offsets as O(1) lookups.
    app.ensure_metrics(content_width.max(1));
    let total_rows = app.total_rows();
    let max_scroll = total_rows.saturating_sub(content_height);
    // Persist the clamp so resizing back up doesn't leave scroll past the new end.
    app.scroll_offset = app.scroll_offset.min(max_scroll);
    let scroll = app.scroll_offset;

    // Draw the border block first
    let scroll_info = format!(" {}/{} ", scroll + 1, total_rows.max(1));
    let border_block = Block::default()
        .borders(Borders::ALL)
        .border_style(if !app.focus_toc {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(format!(" {} ", app.file_path.display()))
        .title_style(Style::default().bold())
        .title_bottom(Line::from(scroll_info).right_aligned());
    f.render_widget(border_block, content_area);

    // Now render content elements within the inner area, respecting scroll offset
    render_content_elements(f, inner_area, &mut app.rendered, &app.cum_rows, scroll, content_height, &app.search_matches, app.current_match_idx);

    // Bottom bar
    let bar_text = if app.search_mode {
        let match_info = if app.search_matches.is_empty() {
            if app.search_query.is_empty() { String::new() }
            else { " (no matches)".to_string() }
        } else {
            format!(" ({}/{})", app.current_match_idx + 1, app.search_matches.len())
        };
        format!(" /{}{}  [Enter: next | Esc: close]", app.search_query, match_info)
    } else if !app.search_matches.is_empty() {
        format!(" Search: '{}' ({}/{})  [n/N: next/prev | /: search]",
            app.search_query, app.current_match_idx + 1, app.search_matches.len())
    } else {
        " q: quit | Tab: switch focus | j/k: scroll | /: search | Space/PgDn: page down ".to_string()
    };

    let help_area = Rect {
        x: content_area.x + 1,
        y: content_area.y + content_area.height - 1,
        width: content_area.width.saturating_sub(2).min(bar_text.len() as u16),
        height: 1,
    };

    let bar_style = if app.search_mode {
        Style::default().fg(Color::Yellow).bg(Color::Rgb(40, 40, 40))
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let help_widget = Paragraph::new(bar_text).style(bar_style);
    f.render_widget(help_widget, help_area);
}

/// Render content elements into the given area, handling scroll offset.
/// Heights/positions come from the precomputed `cum_rows` cache so no element
/// is re-measured here. Visible wrapped elements are wrapped once on demand.
/// Search matches highlight the entire matching element (all sub-lines).
fn render_content_elements(
    f: &mut Frame,
    area: Rect,
    elements: &mut [ContentElement],
    cum_rows: &[usize],
    scroll: usize,
    content_height: usize,
    search_matches: &[SearchMatch],
    current_match: usize,
) {
    let width = area.width;
    let mut y_offset: u16 = 0;
    let available_height = content_height as u16;

    let current_element_idx = search_matches.get(current_match).map(|m| m.element_idx);

    for (idx, element) in elements.iter_mut().enumerate() {
        if y_offset >= available_height {
            break;
        }

        // O(1) start/height lookup from the cache.
        let elem_start = *cum_rows.get(idx).unwrap_or(&0);
        let elem_end = *cum_rows.get(idx + 1).unwrap_or(&elem_start);

        // Element fully above the scroll window.
        if elem_end <= scroll {
            continue;
        }

        let skip_within = scroll.saturating_sub(elem_start);

        let is_match = search_matches.iter().any(|m| m.element_idx == idx);
        let is_current = current_element_idx == Some(idx);

        match element {
            ContentElement::TextLine { line, wrap: false } => {
                if skip_within == 0 {
                    let line_area = Rect {
                        x: area.x,
                        y: area.y + y_offset,
                        width,
                        height: 1,
                    };
                    render_styled_line(f, line_area, line, is_match, is_current);
                    y_offset += 1;
                }
                // For 1-row elements, skip_within > 0 means the element is fully past.
            }
            ContentElement::TextLine { line, wrap: true } => {
                // Visible wrapped element: this is the ONLY place we run wrap
                // work per frame for this element. No double-compute.
                let sublines = wrap_line_into_sublines(line, width);
                let total_subs = sublines.len();
                if skip_within >= total_subs {
                    continue;
                }
                let remaining = available_height - y_offset;
                let render_count = ((total_subs - skip_within) as u16).min(remaining);
                for j in 0..render_count {
                    let sub = &sublines[skip_within + j as usize];
                    let line_area = Rect {
                        x: area.x,
                        y: area.y + y_offset + j,
                        width,
                        height: 1,
                    };
                    render_styled_line(f, line_area, sub, is_match, is_current);
                }
                y_offset += render_count;
            }
            ContentElement::Image { protocol, height, .. } => {
                let visible_height = (*height as usize).saturating_sub(skip_within) as u16;
                if visible_height == 0 {
                    continue;
                }
                let remaining = available_height - y_offset;
                let render_height = visible_height.min(remaining);
                if render_height == 0 {
                    continue;
                }
                let img_area = Rect {
                    x: area.x,
                    y: area.y + y_offset,
                    width,
                    height: render_height,
                };
                let image_widget = StatefulImage::default().resize(Resize::Fit(None));
                f.render_stateful_widget(image_widget, img_area, protocol);
                y_offset += render_height;
            }
        }
    }
}

/// Render a single styled line into a 1-row area. Search hits get a yellow tint
/// (bright yellow for the current match, dim yellow for other matches).
fn render_styled_line(
    f: &mut Frame,
    area: Rect,
    line: &Line<'_>,
    is_match: bool,
    is_current: bool,
) {
    let transform: fn(Style) -> Style = if is_current {
        |s| s.bg(Color::Yellow).fg(Color::Black)
    } else if is_match {
        |s| s.bg(Color::Rgb(80, 80, 0))
    } else {
        |s| s
    };
    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .map(|s| Span::styled(s.content.to_string(), transform(s.style)))
        .collect();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Find the element index of the heading matching `toc_entries[toc_index]`.
/// Substring match on the element's concatenated text (matches the original
/// behavior; the caller resolves the index to a row at the current width).
fn find_heading_element(
    elements: &[ContentElement],
    toc_entries: &[TocEntry],
    toc_index: usize,
) -> Option<usize> {
    let entry = toc_entries.get(toc_index)?;
    let search_text = &entry.text;
    for (i, element) in elements.iter().enumerate() {
        // Only consider wrapping text elements (headings are emitted as wrap=true);
        // skip non-wrap rules/underlines to avoid landing on a separator that happens
        // to share the heading's slug characters.
        if matches!(element, ContentElement::TextLine { wrap: true, .. })
            && element.text().contains(search_text)
        {
            return Some(i);
        }
    }
    None
}

/// Build content elements from markdown, loading images where possible.
fn build_content_elements(content: &str, file_path: &PathBuf, picker: &Option<Picker>) -> Vec<ContentElement> {
    let text_lines = markdown_to_lines_with_images(content);
    let canonical_file = std::fs::canonicalize(file_path)
        .unwrap_or_else(|_| {
            std::env::current_dir()
                .map(|cwd| cwd.join(file_path))
                .unwrap_or_else(|_| file_path.clone())
        });
    let base_dir = canonical_file.parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    let mut elements = Vec::new();
    for item in text_lines {
        match item {
            ParsedLine::Text { line, wrap } => {
                elements.push(ContentElement::TextLine { line, wrap });
            }
            ParsedLine::MermaidRef { source } => {
                // Try to render mermaid diagram as an image
                match crate::core::mermaid::render_mermaid_to_svg(&source) {
                    Ok(svg) => {
                        match rasterize_svg(&svg) {
                            Ok(dyn_img) => {
                                if let Some(ref picker) = picker {
                                    let (img_w, img_h) = (dyn_img.width(), dyn_img.height());
                                    let aspect = img_h as f64 / img_w as f64;
                                    let target_cols = 100u16;
                                    let target_rows = ((target_cols as f64) * aspect / 2.0).ceil() as u16;
                                    let height = target_rows.clamp(4, 40);

                                    let protocol = picker.new_resize_protocol(dyn_img);
                                    elements.push(ContentElement::Image {
                                        protocol,
                                        _alt: "mermaid diagram".to_string(),
                                        height,
                                    });
                                } else {
                                    // No picker: fall back to code block display
                                    push_mermaid_fallback_code(&mut elements, &source);
                                }
                            }
                            Err(_) => {
                                push_mermaid_fallback_code(&mut elements, &source);
                            }
                        }
                    }
                    Err(_) => {
                        push_mermaid_fallback_code(&mut elements, &source);
                    }
                }
            }
            ParsedLine::ImageRef { alt, url } => {
                if let Some(ref picker) = picker {
                    match load_image(&url, base_dir) {
                        Ok(dyn_img) => {
                            // Calculate image height in rows. Use a reasonable default:
                            // Fill terminal width for readable images.
                            let (img_w, img_h) = (dyn_img.width(), dyn_img.height());
                            let aspect = img_h as f64 / img_w as f64;
                            let target_cols = 100u16;
                            let target_rows = ((target_cols as f64) * aspect / 2.0).ceil() as u16;
                            let height = target_rows.clamp(4, 40);

                            let protocol = picker.new_resize_protocol(dyn_img);
                            elements.push(ContentElement::Image {
                                protocol,
                                _alt: alt,
                                height,
                            });
                        }
                        Err(_) => {
                            let label = if alt.is_empty() { "image".to_string() } else { alt };
                            elements.push(ContentElement::TextLine {
                                line: Line::from(Span::styled(
                                    format!("[Image: {}]", label),
                                    Style::default().fg(Color::Magenta).italic(),
                                )),
                                wrap: false,
                            });
                        }
                    }
                } else {
                    // No picker available (terminal doesn't support image protocols or detection failed)
                    let label = if alt.is_empty() { "image".to_string() } else { alt };
                    elements.push(ContentElement::TextLine {
                        line: Line::from(Span::styled(
                            format!("[Image: {}]", label),
                            Style::default().fg(Color::Magenta).italic(),
                        )),
                        wrap: false,
                    });
                }
            }
        }
    }

    elements
}

/// Push a mermaid code block as fallback text when rendering fails or no picker is available.
fn push_mermaid_fallback_code(elements: &mut Vec<ContentElement>, source: &str) {
    let nowrap = |line: Line<'static>| ContentElement::TextLine { line, wrap: false };
    elements.push(nowrap(Line::from(Span::styled(
        "┌─ mermaid ─────────────────────────────────┐".to_string(),
        Style::default().fg(Color::DarkGray),
    ))));
    for line in source.lines() {
        elements.push(nowrap(Line::from(Span::styled(
            format!("│ {}", line),
            Style::default().fg(Color::Green),
        ))));
    }
    elements.push(nowrap(Line::from(Span::styled(
        "└─────────────────────────────────────────┘".to_string(),
        Style::default().fg(Color::DarkGray),
    ))));
    elements.push(nowrap(Line::from("")));
}

/// Load an image from a URL, data URI, or local file path.
/// SVG files are rasterized via resvg/usvg before returning.
fn load_image(url: &str, base_dir: &std::path::Path) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
    if url.starts_with("data:") {
        // data: URI - decode base64
        load_image_from_data_uri(url)
    } else if url.starts_with("http://") || url.starts_with("https://") {
        // HTTP fetch
        load_image_from_http(url)
    } else {
        // Local file path (resolve relative to markdown file's directory)
        let path = if std::path::Path::new(url).is_absolute() {
            PathBuf::from(url)
        } else {
            base_dir.join(url)
        };
        // Path traversal protection: ensure resolved path is within base_dir
        if let (Ok(canonical), Ok(canonical_base)) = (path.canonicalize(), base_dir.canonicalize()) {
            if !canonical.starts_with(&canonical_base) {
                return Err("path traversal blocked: image path escapes base directory".into());
            }
        }
        // SVG files need rasterization
        if path.extension().and_then(|e| e.to_str()) == Some("svg") {
            let svg_data = std::fs::read_to_string(&path)?;
            return rasterize_svg(&svg_data);
        }
        let img = image::open(&path)?;
        Ok(img)
    }
}

/// Load an image from a data: URI by decoding the base64 payload.
/// Rejects data URIs larger than 50MB (base64-encoded) to prevent memory exhaustion.
fn load_image_from_data_uri(uri: &str) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
    const MAX_DATA_URI_LEN: usize = 50 * 1024 * 1024; // 50 MB
    if uri.len() > MAX_DATA_URI_LEN {
        return Err(format!("data URI too large ({} bytes, max {})", uri.len(), MAX_DATA_URI_LEN).into());
    }
    // Format: data:[<mediatype>][;base64],<data>
    let comma_pos = uri.find(',').ok_or("Invalid data URI: no comma found")?;
    let header = &uri[..comma_pos];
    let data_part = &uri[comma_pos + 1..];
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        data_part,
    )?;
    // SVG data URIs need rasterization
    if header.contains("image/svg") {
        let svg_str = String::from_utf8(decoded)?;
        return rasterize_svg(&svg_str);
    }
    let img = image::load_from_memory(&decoded)?;
    Ok(img)
}

/// Rasterize an SVG string to a DynamicImage using resvg/usvg.
fn rasterize_svg(svg_data: &str) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
    use std::sync::{Arc, OnceLock};

    static FONTDB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let fontdb = FONTDB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });

    let mut options = usvg::Options::default();
    options.fontdb = Arc::clone(fontdb);
    let tree = usvg::Tree::from_str(svg_data, &options)?;
    let size = tree.size();
    let width = size.width() as u32;
    let height = size.height() as u32;

    if width == 0 || height == 0 {
        return Err("SVG has zero dimensions".into());
    }

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or("Failed to create pixmap")?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

    // Convert RGBA pixmap to DynamicImage
    let img = image::RgbaImage::from_raw(width, height, pixmap.data().to_vec())
        .ok_or("Failed to create image from pixmap")?;
    Ok(image::DynamicImage::ImageRgba8(img))
}

/// Load an image from an HTTP(S) URL using ureq.
fn load_image_from_http(url: &str) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
    let response = ureq::get(url).call()?;
    let mut bytes = Vec::new();
    response.into_body().into_reader().read_to_end(&mut bytes)?;
    let img = image::load_from_memory(&bytes)?;
    Ok(img)
}

/// Intermediate representation for parsed markdown lines.
enum ParsedLine {
    Text {
        line: Line<'static>,
        /// When true, the line is word-wrapped to the content width at render
        /// time. Used for prose: paragraphs, list items, blockquotes, task
        /// items, and heading text. When false (default for code lines, table
        /// rows, borders, horizontal rules, heading underlines) the line is
        /// rendered as-is and truncated if it overflows.
        wrap: bool,
    },
    ImageRef { alt: String, url: String },
    /// A mermaid diagram source extracted from a ```mermaid code block.
    MermaidRef { source: String },
}

fn pt_wrap(line: Line<'static>) -> ParsedLine {
    ParsedLine::Text { line, wrap: true }
}

fn pt_nowrap(line: Line<'static>) -> ParsedLine {
    ParsedLine::Text { line, wrap: false }
}

/// Convert markdown content to a mix of styled text lines and image references.
fn markdown_to_lines_with_images(content: &str) -> Vec<ParsedLine> {
    let mut items = Vec::new();
    let mut in_code_block = false;
    let mut in_table = false;
    let mut in_mermaid_block = false;
    let mut mermaid_source = String::new();

    for line in content.lines() {
        if line.starts_with("```") {
            if in_code_block {
                if in_mermaid_block {
                    // End of mermaid block: emit a MermaidRef instead of code lines
                    in_mermaid_block = false;
                    in_code_block = false;
                    items.push(ParsedLine::MermaidRef { source: mermaid_source.clone() });
                    mermaid_source.clear();
                } else {
                    in_code_block = false;
                    items.push(pt_nowrap(Line::from(Span::styled(
                        "└─────────────────────────────────────────┘",
                        Style::default().fg(Color::DarkGray),
                    ))));
                    items.push(pt_nowrap(Line::from("")));
                }
            } else {
                in_code_block = true;
                let code_lang = line.trim_start_matches('`').trim().to_string();
                if code_lang == "mermaid" {
                    in_mermaid_block = true;
                    mermaid_source.clear();
                } else {
                    let header = if code_lang.is_empty() {
                        "┌─ code ──────────────────────────────────┐".to_string()
                    } else {
                        format!("┌─ {} {}", code_lang, "─".repeat(38usize.saturating_sub(code_lang.len())))
                    };
                    items.push(pt_nowrap(Line::from(Span::styled(
                        header,
                        Style::default().fg(Color::DarkGray),
                    ))));
                }
            }
            continue;
        }

        if in_code_block {
            if in_mermaid_block {
                // Accumulate mermaid source lines
                if !mermaid_source.is_empty() {
                    mermaid_source.push('\n');
                }
                mermaid_source.push_str(line);
            } else {
                items.push(pt_nowrap(Line::from(Span::styled(
                    format!("│ {}", line),
                    Style::default().fg(Color::Green),
                ))));
            }
            continue;
        }

        // Headings: text wraps; underline rule does not.
        if line.starts_with("# ") {
            items.push(pt_nowrap(Line::from("")));
            items.push(pt_wrap(Line::from(Span::styled(
                line[2..].to_string(),
                Style::default().fg(Color::Cyan).bold().underlined(),
            ))));
            items.push(pt_nowrap(Line::from(Span::styled(
                "═".repeat(line.len().saturating_sub(2).min(60)),
                Style::default().fg(Color::Cyan),
            ))));
            items.push(pt_nowrap(Line::from("")));
            continue;
        }
        if line.starts_with("## ") {
            items.push(pt_nowrap(Line::from("")));
            items.push(pt_wrap(Line::from(Span::styled(
                line[3..].to_string(),
                Style::default().fg(Color::Blue).bold(),
            ))));
            items.push(pt_nowrap(Line::from(Span::styled(
                "─".repeat(line.len().saturating_sub(3).min(50)),
                Style::default().fg(Color::Blue),
            ))));
            items.push(pt_nowrap(Line::from("")));
            continue;
        }
        if line.starts_with("### ") {
            items.push(pt_nowrap(Line::from("")));
            items.push(pt_wrap(Line::from(Span::styled(
                line[4..].to_string(),
                Style::default().fg(Color::Yellow).bold(),
            ))));
            items.push(pt_nowrap(Line::from("")));
            continue;
        }
        if line.starts_with("#### ") {
            items.push(pt_wrap(Line::from(Span::styled(
                line[5..].to_string(),
                Style::default().fg(Color::Magenta).bold(),
            ))));
            continue;
        }

        // Horizontal rule
        if line.starts_with("---") || line.starts_with("***") || line.starts_with("___") {
            items.push(pt_nowrap(Line::from(Span::styled(
                "─".repeat(60),
                Style::default().fg(Color::DarkGray),
            ))));
            continue;
        }

        // Table rows (column alignment depends on no-wrap; long rows are truncated).
        if line.contains('|') && line.trim().starts_with('|') {
            if line.contains("---") && !in_table {
                in_table = true;
                items.push(pt_nowrap(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::DarkGray),
                ))));
                continue;
            }
            in_table = true;
            let cells: Vec<&str> = line.split('|')
                .filter(|s| !s.is_empty())
                .map(|s| s.trim())
                .collect();
            let spans: Vec<Span> = cells.iter().enumerate().flat_map(|(i, cell)| {
                let mut v = vec![];
                if i > 0 {
                    v.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
                }
                v.push(Span::styled(cell.to_string(), Style::default().fg(Color::White)));
                v
            }).collect();
            items.push(pt_nowrap(Line::from(spans)));
            continue;
        } else {
            in_table = false;
        }

        // Blockquote
        if line.starts_with("> ") {
            items.push(pt_wrap(Line::from(vec![
                Span::styled("▎ ", Style::default().fg(Color::DarkGray)),
                Span::styled(line[2..].to_string(), Style::default().fg(Color::Gray).italic()),
            ])));
            continue;
        }

        // Task list
        if line.trim_start().starts_with("- [x] ") {
            let indent = line.len() - line.trim_start().len();
            items.push(pt_wrap(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled("☑ ", Style::default().fg(Color::Green)),
                Span::styled(
                    line.trim_start()[6..].to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ])));
            continue;
        }
        if line.trim_start().starts_with("- [ ] ") {
            let indent = line.len() - line.trim_start().len();
            items.push(pt_wrap(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled("☐ ", Style::default().fg(Color::Yellow)),
                Span::styled(line.trim_start()[6..].to_string(), Style::default()),
            ])));
            continue;
        }

        // Unordered list
        if line.trim_start().starts_with("- ") || line.trim_start().starts_with("* ") {
            let indent = line.len() - line.trim_start().len();
            items.push(pt_wrap(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled("• ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    line.trim_start()[2..].to_string(),
                    Style::default(),
                ),
            ])));
            continue;
        }

        // Ordered list
        if let Some(rest) = try_parse_ordered_list(line) {
            let indent = line.len() - line.trim_start().len();
            items.push(pt_wrap(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(rest.0.clone(), Style::default().fg(Color::Cyan)),
                Span::styled(rest.1.clone(), Style::default()),
            ])));
            continue;
        }

        // Image: ![alt](url) on its own line
        if line.trim_start().starts_with("![") {
            if let Some((alt, url)) = extract_image_alt_and_url(line) {
                items.push(ParsedLine::ImageRef { alt, url });
                continue;
            }
        }

        // Regular text with inline formatting (wrapped paragraph).
        items.push(pt_wrap(parse_inline_formatting(line)));
    }

    items
}

/// Extract alt text and URL from a markdown image line: ![alt](url)
fn extract_image_alt_and_url(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    let start = trimmed.find("![")?;
    let rest = &trimmed[start + 2..];
    let bracket_end = rest.find("](")?;
    let alt = rest[..bracket_end].to_string();
    let after_bracket = &rest[bracket_end + 2..];
    let paren_end = after_bracket.find(')')?;
    let url = after_bracket[..paren_end].to_string();
    Some((alt, url))
}

/// Try to parse an ordered list item, returns (number prefix, text)
fn try_parse_ordered_list(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    let dot_pos = trimmed.find(". ")?;
    let num_part = &trimmed[..dot_pos];
    if num_part.chars().all(|c| c.is_ascii_digit()) && !num_part.is_empty() {
        let text = trimmed[dot_pos + 2..].to_string();
        Some((format!("{}. ", num_part), text))
    } else {
        None
    }
}

/// Parse inline markdown formatting (bold, italic, code, strikethrough, links)
fn parse_inline_formatting(line: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut chars = line.chars().peekable();
    let mut current = String::new();

    while let Some(c) = chars.next() {
        match c {
            '`' => {
                if !current.is_empty() {
                    spans.push(Span::raw(current.clone()));
                    current.clear();
                }
                let mut code = String::new();
                for c in chars.by_ref() {
                    if c == '`' { break; }
                    code.push(c);
                }
                spans.push(Span::styled(code, Style::default().fg(Color::Green).bg(Color::Rgb(30, 30, 30))));
            }
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if !current.is_empty() {
                    spans.push(Span::raw(current.clone()));
                    current.clear();
                }
                let mut bold = String::new();
                while let Some(c) = chars.next() {
                    if c == '*' && chars.peek() == Some(&'*') {
                        chars.next();
                        break;
                    }
                    bold.push(c);
                }
                spans.push(Span::styled(bold, Style::default().bold()));
            }
            '*' | '_' => {
                if !current.is_empty() {
                    spans.push(Span::raw(current.clone()));
                    current.clear();
                }
                let mut italic = String::new();
                for ch in chars.by_ref() {
                    if ch == c { break; }
                    italic.push(ch);
                }
                spans.push(Span::styled(italic, Style::default().italic()));
            }
            '~' if chars.peek() == Some(&'~') => {
                chars.next();
                if !current.is_empty() {
                    spans.push(Span::raw(current.clone()));
                    current.clear();
                }
                let mut strike = String::new();
                while let Some(c) = chars.next() {
                    if c == '~' && chars.peek() == Some(&'~') {
                        chars.next();
                        break;
                    }
                    strike.push(c);
                }
                spans.push(Span::styled(
                    strike,
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT),
                ));
            }
            '!' if chars.peek() == Some(&'[') => {
                // Image: ![alt](url)
                chars.next(); // consume '['
                let mut alt = String::new();
                let mut found_close = false;
                for ch in chars.by_ref() {
                    if ch == ']' { found_close = true; break; }
                    alt.push(ch);
                }
                if found_close && chars.peek() == Some(&'(') {
                    chars.next();
                    let mut _url = String::new();
                    for ch in chars.by_ref() {
                        if ch == ')' { break; }
                        _url.push(ch);
                    }
                    if !current.is_empty() {
                        spans.push(Span::raw(current.clone()));
                        current.clear();
                    }
                    let label = if alt.is_empty() { "image".to_string() } else { alt };
                    spans.push(Span::styled(
                        format!("[Image: {}]", label),
                        Style::default().fg(Color::Magenta).italic(),
                    ));
                } else {
                    current.push('!');
                    current.push('[');
                    current.push_str(&alt);
                    if found_close { current.push(']'); }
                }
            }
            '[' => {
                // Link: [text](url)
                let mut text = String::new();
                let mut found_close = false;
                for ch in chars.by_ref() {
                    if ch == ']' { found_close = true; break; }
                    text.push(ch);
                }
                if found_close && chars.peek() == Some(&'(') {
                    chars.next();
                    let mut _url = String::new();
                    for ch in chars.by_ref() {
                        if ch == ')' { break; }
                        _url.push(ch);
                    }
                    if !current.is_empty() {
                        spans.push(Span::raw(current.clone()));
                        current.clear();
                    }
                    spans.push(Span::styled(text, Style::default().fg(Color::Blue).underlined()));
                } else {
                    current.push('[');
                    current.push_str(&text);
                    if found_close { current.push(']'); }
                }
            }
            _ => current.push(c),
        }
    }

    if !current.is_empty() {
        spans.push(Span::raw(current));
    }

    if spans.is_empty() {
        Line::from("")
    } else {
        Line::from(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_image_svg_local_file() {
        // Create a minimal SVG file in a temp directory
        let dir = std::env::temp_dir().join("mdr_test_svg");
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("test.svg");
        let mut f = std::fs::File::create(&svg_path).unwrap();
        write!(f, r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="100" height="100" fill="red"/></svg>"#).unwrap();

        let result = load_image("test.svg", &dir);
        // This should succeed — SVG files must be rasterized before display
        assert!(result.is_ok(), "load_image should handle SVG files but got: {:?}", result.err());
        let img = result.unwrap();
        assert!(img.width() > 0 && img.height() > 0);

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_content_elements_with_local_svg() {
        // Create a temp dir with an SVG and a markdown file referencing it
        let dir = std::env::temp_dir().join("mdr_test_svg_content");
        std::fs::create_dir_all(&dir).unwrap();

        let svg_path = dir.join("logo.svg");
        let mut f = std::fs::File::create(&svg_path).unwrap();
        write!(f, r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="100" height="100" fill="red"/></svg>"#).unwrap();

        let md = "# Hello\n\n![my logo](logo.svg)\n\nSome text after.\n";
        let md_path = dir.join("test.md");
        std::fs::write(&md_path, md).unwrap();

        // Build content elements (without a picker, images become placeholders OR succeed via rasterize)
        let elements = build_content_elements(md, &md_path, &None);

        // Without a picker, SVG falls back to placeholder — but the markdown parser should find it.
        // Placeholders are now emitted as non-wrapping TextLines styled with magenta italic.
        let has_image_ref = elements.iter().any(|e| {
            matches!(e, ContentElement::TextLine { wrap: false, .. })
                && e.text().starts_with("[Image:")
        });
        assert!(has_image_ref, "Should find an image placeholder for the SVG reference");

        // Now test load_image directly to confirm SVG rasterization works
        let img = load_image("logo.svg", &dir);
        assert!(img.is_ok(), "load_image should rasterize SVG, got: {:?}", img.err());
        let img = img.unwrap();
        assert_eq!(img.width(), 100);
        assert_eq!(img.height(), 100);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_image_svg_data_uri() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="50"><circle cx="25" cy="25" r="20" fill="blue"/></svg>"#;
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, svg.as_bytes());
        let data_uri = format!("data:image/svg+xml;base64,{}", b64);

        let result = load_image(&data_uri, std::path::Path::new("."));
        assert!(result.is_ok(), "load_image should handle SVG data URIs but got: {:?}", result.err());
    }

    #[test]
    fn mermaid_block_produces_mermaid_ref() {
        let md = "# Title\n\n```mermaid\ngraph LR\n  A-->B\n```\n\nSome text after.\n";
        let items = markdown_to_lines_with_images(md);

        let has_mermaid_ref = items.iter().any(|item| matches!(item, ParsedLine::MermaidRef { .. }));
        assert!(has_mermaid_ref, "Mermaid code block should produce a MermaidRef variant");

        // Verify the source is captured correctly
        let mermaid_source = items.iter().find_map(|item| {
            if let ParsedLine::MermaidRef { source } = item {
                Some(source.clone())
            } else {
                None
            }
        }).expect("Should have a MermaidRef");
        assert!(mermaid_source.contains("graph LR"), "MermaidRef should contain the mermaid source, got: {}", mermaid_source);
        assert!(mermaid_source.contains("A-->B"), "MermaidRef should contain the diagram content");
    }

    #[test]
    fn mermaid_block_not_rendered_as_code_text() {
        let md = "```mermaid\ngraph LR\n  A-->B\n```\n";
        let items = markdown_to_lines_with_images(md);

        // Should NOT have green code lines for mermaid content
        let has_green_code = items.iter().any(|item| {
            if let ParsedLine::Text { line, .. } = item {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                text.contains("│ graph LR") || text.contains("│   A-->B")
            } else {
                false
            }
        });
        assert!(!has_green_code, "Mermaid content should NOT appear as regular code text");
    }

    #[test]
    fn non_mermaid_code_block_unchanged() {
        let md = "```rust\nfn main() {}\n```\n";
        let items = markdown_to_lines_with_images(md);

        let has_mermaid_ref = items.iter().any(|item| matches!(item, ParsedLine::MermaidRef { .. }));
        assert!(!has_mermaid_ref, "Non-mermaid code blocks should NOT produce MermaidRef");

        // Should have regular code text emitted as non-wrapping lines (truncated, not wrapped).
        let has_code_text = items.iter().any(|item| {
            if let ParsedLine::Text { line, wrap: false } = item {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                text.contains("│ fn main()")
            } else {
                false
            }
        });
        assert!(has_code_text, "Non-mermaid code should appear as regular code text");
    }

    #[test]
    fn mermaid_build_content_elements_fallback_without_picker() {
        // Without a picker, mermaid should fall back to code block display
        let md = "```mermaid\ngraph LR\n  A-->B\n```\n";
        let md_path = std::path::PathBuf::from("/tmp/test_mermaid.md");
        let elements = build_content_elements(md, &md_path, &None);

        assert!(!elements.is_empty(), "Should produce content elements for mermaid block");

        // The fallback uses non-wrapping text lines (preserve box-drawing).
        let has_text = elements.iter().any(|e| matches!(e, ContentElement::TextLine { .. }));
        assert!(has_text, "Mermaid fallback should produce text lines");
    }

    // --- wrap tests ---

    #[test]
    fn wrap_short_line_is_single_row() {
        let line = Line::from("hello world");
        assert_eq!(wrapped_line_count(&line, 80), 1);
        assert_eq!(wrap_line_into_sublines(&line, 80).len(), 1);
    }

    #[test]
    fn wrap_breaks_on_whitespace() {
        let line = Line::from("one two three four five six");
        // Width 11 = "one two" (7) + space — second word "three" (5) overflows → wrap.
        let count = wrapped_line_count(&line, 11);
        assert!(count >= 2, "expected at least 2 rows, got {}", count);
        let subs = wrap_line_into_sublines(&line, 11);
        assert_eq!(subs.len() as u16, count);
        // No sub-line should exceed the width.
        for s in &subs {
            let text: String = s.spans.iter().map(|sp| sp.content.as_ref()).collect();
            assert!(text.chars().count() <= 11, "sub-line too wide: {:?}", text);
        }
    }

    #[test]
    fn wrap_preserves_span_styles_within_sublines() {
        let line = Line::from(vec![
            Span::styled("hello ", Style::default().fg(Color::Red)),
            Span::styled("world", Style::default().fg(Color::Blue)),
        ]);
        let subs = wrap_line_into_sublines(&line, 100);
        assert_eq!(subs.len(), 1);
        // Should still have two styled spans, not merged into one.
        assert_eq!(subs[0].spans.len(), 2);
        assert_eq!(subs[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(subs[0].spans[1].style.fg, Some(Color::Blue));
    }

    #[test]
    fn wrap_empty_line_is_one_row() {
        assert_eq!(wrapped_line_count(&Line::from(""), 80), 1);
        let subs = wrap_line_into_sublines(&Line::from(""), 80);
        assert_eq!(subs.len(), 1);
    }

    #[test]
    fn wrap_long_word_hard_breaks() {
        // A single word longer than the width must be broken.
        let line = Line::from("abcdefghij");
        let count = wrapped_line_count(&line, 4);
        assert!(count >= 2, "long word should break; got {} rows", count);
    }

    // --- wrap linebreakchars ("++++") tests ---

    fn subline_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wrap_linebreakchars_appear_only_on_continuation_sublines() {
        // First sub-line: no prefix. Every continuation sub-line: a dimmed
        // `++++` prefix appears as the first span. Empty lines remain empty.
        let line = Line::from("one two three four five six seven eight nine ten");
        let subs = wrap_line_into_sublines(&line, 12);
        assert!(subs.len() >= 2, "expected wrapping, got {} sub-lines", subs.len());

        // First sub-line has no leading linebreakchars.
        assert!(
            !subline_text(&subs[0]).starts_with(WRAP_LINEBREAKCHARS),
            "first sub-line must not have the linebreakchars, got {:?}",
            subline_text(&subs[0])
        );

        // Every continuation sub-line starts with the dimmed prefix span.
        for (i, sub) in subs.iter().enumerate().skip(1) {
            let prefix = &sub.spans[0];
            assert_eq!(
                prefix.content.as_ref(),
                WRAP_LINEBREAKCHARS,
                "sub-line {} should start with linebreakchars, got {:?}",
                i,
                subline_text(sub)
            );
            assert!(
                prefix.style.add_modifier.contains(Modifier::DIM),
                "sub-line {} prefix should be DIM, got style {:?}",
                i,
                prefix.style
            );
        }

        // Empty input still produces exactly one sub-line and no prefix.
        let empty = wrap_line_into_sublines(&Line::from(""), 80);
        assert_eq!(empty.len(), 1);
        assert!(!subline_text(&empty[0]).starts_with(WRAP_LINEBREAKCHARS));
    }

    #[test]
    fn wrap_sublines_fit_within_width_including_linebreakchars() {
        // Linebreakchars (WRAP_LINEBREAKCHARS_WIDTH) + content must not exceed
        // the available width.
        let line = Line::from("aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk");
        for width in [12u16, 20, 40] {
            let subs = wrap_line_into_sublines(&line, width);
            for (i, s) in subs.iter().enumerate() {
                let n = subline_text(s).chars().count() as u16;
                assert!(
                    n <= width,
                    "sub-line {} at width {} too wide: {} cols",
                    i,
                    width,
                    n
                );
            }
        }
    }

    #[test]
    fn wrap_no_linebreakchars_at_very_narrow_widths() {
        // When width <= WRAP_LINEBREAKCHARS_WIDTH + 1, the chars would leave
        // no room for content; behaviour falls back to no-prefix wrapping.
        let line = Line::from("abc def ghi");
        let subs = wrap_line_into_sublines(&line, WRAP_LINEBREAKCHARS_WIDTH);
        for s in &subs {
            assert!(
                !subline_text(s).starts_with(WRAP_LINEBREAKCHARS),
                "no linebreakchars expected at narrow width, got {:?}",
                subline_text(s)
            );
        }
    }

    #[test]
    fn wrap_count_reflects_linebreakchars_width_reservation() {
        // Same content wraps to more rows when the linebreakchars steal columns
        // from continuation rows (compared to a hypothetical no-prefix run).
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let with_chars = split_wrap_offsets(text, 20, 20 - WRAP_LINEBREAKCHARS_WIDTH).len();
        let without_chars = split_wrap_offsets(text, 20, 20).len();
        assert!(
            with_chars >= without_chars,
            "linebreakchars-aware count ({}) should be >= no-prefix count ({})",
            with_chars,
            without_chars
        );
    }

    #[test]
    fn match_to_row_jumps_past_linebreakchars_widths() {
        // With linebreakchars reducing continuation widths, a late match should
        // still resolve to the correct (further) sub-line.
        let para = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
        let elements = vec![
            ContentElement::TextLine { line: Line::from(para), wrap: true },
        ];
        let m = SearchMatch {
            element_idx: 0,
            char_offset: para.find("lambda").unwrap(),
        };
        let row = match_to_row(&elements, &m, 20);
        let total = total_content_rows(&elements, 20);
        assert!(row > 0, "match should not be on first sub-line");
        assert!(row < total, "match row {} should be < total {}", row, total);
    }

    #[test]
    fn row_height_no_wrap_is_one() {
        let elem = ContentElement::TextLine {
            line: Line::from("this is a very long line that would wrap if it were allowed to"),
            wrap: false,
        };
        assert_eq!(elem.row_height(20), 1);
    }

    #[test]
    fn row_height_wrap_grows_at_narrow_width() {
        let elem = ContentElement::TextLine {
            line: Line::from("alpha beta gamma delta epsilon zeta eta theta iota"),
            wrap: true,
        };
        let wide = elem.row_height(200);
        let narrow = elem.row_height(15);
        assert_eq!(wide, 1, "should fit on one row when wide");
        assert!(narrow > wide, "should wrap to more rows when narrow");
    }

    #[test]
    fn total_content_rows_scales_with_width() {
        // A paragraph that needs wrapping should contribute more rows at narrow widths.
        let para = "one two three four five six seven eight nine ten eleven twelve";
        let elements = vec![
            ContentElement::TextLine { line: Line::from(para), wrap: true },
        ];
        let wide = total_content_rows(&elements, 200);
        let narrow = total_content_rows(&elements, 20);
        assert_eq!(wide, 1);
        assert!(narrow > 1, "narrow width should produce more rows");
    }

    #[test]
    fn parser_marks_paragraphs_as_wrap() {
        let md = "This is a plain paragraph.\n";
        let items = markdown_to_lines_with_images(md);
        let para = items.iter().find_map(|it| {
            if let ParsedLine::Text { line, wrap } = it {
                let txt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                if txt.contains("plain paragraph") {
                    return Some(*wrap);
                }
            }
            None
        });
        assert_eq!(para, Some(true), "paragraph should be wrap=true");
    }

    #[test]
    fn parser_marks_code_lines_as_nowrap() {
        let md = "```\nfn main() { println!(\"hi\"); }\n```\n";
        let items = markdown_to_lines_with_images(md);
        let code = items.iter().find_map(|it| {
            if let ParsedLine::Text { line, wrap } = it {
                let txt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                if txt.contains("fn main") {
                    return Some(*wrap);
                }
            }
            None
        });
        assert_eq!(code, Some(false), "code line should be wrap=false");
    }

    #[test]
    fn parser_marks_table_rows_as_nowrap() {
        let md = "| col |\n| --- |\n| value |\n";
        let items = markdown_to_lines_with_images(md);
        let any_wrap = items.iter().any(|it| matches!(it, ParsedLine::Text { wrap: true, .. }));
        assert!(!any_wrap, "table rows should all be wrap=false");
    }

    #[test]
    fn parser_marks_heading_text_wrap_underline_nowrap() {
        let md = "# Heading One\n";
        let items = markdown_to_lines_with_images(md);
        // Find the heading text line and the underline rule line.
        let heading_text = items.iter().find_map(|it| {
            if let ParsedLine::Text { line, wrap } = it {
                let txt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                if txt == "Heading One" {
                    return Some(*wrap);
                }
            }
            None
        });
        assert_eq!(heading_text, Some(true), "heading text should wrap");

        let has_nowrap_rule = items.iter().any(|it| {
            if let ParsedLine::Text { line, wrap: false } = it {
                let txt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                txt.chars().all(|c| c == '═')
            } else {
                false
            }
        });
        assert!(has_nowrap_rule, "heading underline should be non-wrapping");
    }

    #[test]
    fn search_matches_resolve_to_correct_subline_after_wrap() {
        // Build a single wrapping paragraph with a known match position.
        let para = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        let elements = vec![
            ContentElement::TextLine { line: Line::from(para), wrap: true },
        ];
        let m = SearchMatch {
            element_idx: 0,
            char_offset: para.find("kappa").unwrap(),
        };
        // Narrow width forces wrapping. The match for "kappa" should not land on row 0.
        let row = match_to_row(&elements, &m, 10);
        assert!(row > 0, "match for late word should be on a later sub-line, got row {}", row);
        // Wide width: everything on one row, match is on row 0.
        let row_wide = match_to_row(&elements, &m, 200);
        assert_eq!(row_wide, 0);
    }

    #[test]
    fn element_start_row_accounts_for_wrap() {
        let para = "one two three four five six seven eight nine ten";
        let elements = vec![
            ContentElement::TextLine { line: Line::from("first"), wrap: true },
            ContentElement::TextLine { line: Line::from(para), wrap: true },
            ContentElement::TextLine { line: Line::from("after"), wrap: true },
        ];
        // At a width where the middle paragraph wraps, the third element starts
        // later than at a wide width.
        let narrow = total_content_rows(&elements[..2], 15);
        let wide = total_content_rows(&elements[..2], 200);
        assert!(narrow > wide, "narrow={} should exceed wide={}", narrow, wide);
        // The first element occupies 1 row at any width.
        assert_eq!(wide, 2);
    }

    #[test]
    fn find_heading_element_returns_element_index() {
        let md = "# Title\n\nSome body text.\n\n## Sub\n\nMore text.\n";
        let md_path = std::path::PathBuf::from("/tmp/test_find_heading.md");
        let elements = build_content_elements(md, &md_path, &None);
        let toc = crate::core::toc::extract_toc(md);
        let idx_title = find_heading_element(&elements, &toc, 0).expect("Title found");
        let idx_sub = find_heading_element(&elements, &toc, 1).expect("Sub found");
        assert!(idx_title < idx_sub, "Title element should come before Sub");
        assert!(matches!(&elements[idx_title], ContentElement::TextLine { wrap: true, .. }));
        assert!(matches!(&elements[idx_sub], ContentElement::TextLine { wrap: true, .. }));
    }
}
