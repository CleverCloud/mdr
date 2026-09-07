use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::*;

use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};

use crate::core::toc::{self, TocEntry};

/// One logical line of text together with its wrapped rendering.
///
/// The unwrapped `source` is kept so the line can be folded again when the
/// terminal is resized, and so search and TOC lookups keep working on the
/// original text rather than on whatever the current width happens to be.
struct WrappedText {
    source: Line<'static>,
    lines: Vec<Line<'static>>,
}

impl WrappedText {
    fn new(source: Line<'static>) -> Self {
        Self {
            lines: vec![source.clone()],
            source,
        }
    }

    fn rewrap(&mut self, width: usize) {
        self.lines = wrap_line(&self.source, width);
    }

    /// Rows this line occupies on screen once wrapped.
    fn height(&self) -> usize {
        self.lines.len().max(1)
    }

    /// The unwrapped text, without styling.
    fn text(&self) -> String {
        self.source
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }
}

/// Represents a single line element in the rendered content.
/// Lines can be either text (rendered as ratatui Lines) or images (rendered as StatefulImage).
enum ContentElement {
    TextLine(WrappedText),
    /// An image element that spans a number of rows in the terminal.
    /// Stores the stateful protocol, alt text (for fallback), and the desired height in rows.
    ///
    /// `StatefulProtocol` is by far the largest variant, so it is boxed to keep
    /// `Vec<ContentElement>` — one entry per rendered line — small.
    Image {
        protocol: Box<StatefulProtocol>,
        _alt: String,
        height: u16,
    },
    /// Fallback placeholder when image loading fails.
    ImagePlaceholder(WrappedText),
}

impl ContentElement {
    /// Returns the number of terminal rows this element occupies.
    fn row_height(&self) -> u16 {
        match self {
            ContentElement::TextLine(text) | ContentElement::ImagePlaceholder(text) => {
                text.height() as u16
            }
            ContentElement::Image { height, .. } => *height,
        }
    }
}

/// Re-fold every text element to `width` columns. Images keep their own height.
fn rewrap_elements(elements: &mut [ContentElement], width: usize) {
    for element in elements.iter_mut() {
        if let ContentElement::TextLine(text) | ContentElement::ImagePlaceholder(text) = element {
            text.rewrap(width);
        }
    }
}

/// Display width of a string, in terminal columns.
fn str_width(s: &str) -> usize {
    Span::raw(s).width()
}

fn char_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    str_width(ch.encode_utf8(&mut buf))
}

/// Split `s` so that the first part is at most `width` columns wide.
fn split_at_width(s: &str, width: usize) -> (&str, &str) {
    let mut used = 0usize;
    for (idx, ch) in s.char_indices() {
        let cw = char_width(ch);
        if used + cw > width {
            return s.split_at(idx);
        }
        used += cw;
    }
    (s, "")
}

/// The prefix continuation lines get, so wrapped text keeps its visual column.
///
/// A code-block line repeats its `│ ` gutter verbatim, which keeps the drawn box
/// closed; every other marker (bullets, task boxes, blockquote bars, ordered
/// list numbers) is replaced by blanks so the continuation lines up under the
/// text of the first line instead of under its marker.
fn continuation_prefix(line: &Line<'_>) -> Span<'static> {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let indent_len = text.chars().take_while(|c| *c == ' ').count();
    let indent = " ".repeat(indent_len);
    let rest = &text[indent_len..];

    const GUTTER: &str = "│ ";
    if rest.starts_with(GUTTER) {
        // Keep the gutter, and its colour, on every folded row.
        let style = line.spans.first().map(|s| s.style).unwrap_or_default();
        return Span::styled(format!("{}{}", indent, GUTTER), style);
    }

    const MARKERS: &[&str] = &["• ", "☑ ", "☐ ", "▎ ", "- ", "* "];
    for marker in MARKERS {
        if rest.starts_with(marker) {
            return Span::raw(format!("{}{}", indent, " ".repeat(str_width(marker))));
        }
    }

    // Ordered lists: "12. "
    if let Some(dot) = rest.find(". ") {
        let num = &rest[..dot];
        if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
            return Span::raw(format!("{}{}", indent, " ".repeat(dot + 2)));
        }
    }

    Span::raw(indent)
}

/// A run of characters sharing one style, either all blanks or none.
struct WrapToken {
    text: String,
    style: Style,
    is_space: bool,
}

fn tokenize(line: &Line<'_>) -> Vec<WrapToken> {
    let mut tokens = Vec::new();
    for span in &line.spans {
        let mut chunk = String::new();
        let mut chunk_is_space = false;
        for ch in span.content.chars() {
            let is_space = ch == ' ' || ch == '\t';
            if !chunk.is_empty() && is_space != chunk_is_space {
                tokens.push(WrapToken {
                    text: std::mem::take(&mut chunk),
                    style: span.style,
                    is_space: chunk_is_space,
                });
            }
            chunk_is_space = is_space;
            chunk.push(ch);
        }
        if !chunk.is_empty() {
            tokens.push(WrapToken {
                text: chunk,
                style: span.style,
                is_space: chunk_is_space,
            });
        }
    }
    tokens
}

/// Fold a styled line to `width` columns, preserving every span's style (#54).
///
/// Folding happens on blanks where possible; a single word wider than the line
/// is split mid-word rather than dropped. Blanks that land on a fold are
/// discarded so continuation lines start at their indent. A `width` of 0 means
/// "unknown width" and leaves the line untouched.
fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    if width == 0 || line.width() <= width {
        return vec![line.clone()];
    }

    let prefix = continuation_prefix(line);
    let prefix_width = str_width(&prefix.content);
    // A prefix eating half the line would leave no usable room for the text.
    let (prefix, prefix_width) = if prefix_width * 2 >= width {
        (Span::raw(""), 0)
    } else {
        (prefix, prefix_width)
    };

    let mut folded: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_width = 0usize;

    for token in tokenize(line) {
        let mut remaining: &str = &token.text;
        loop {
            let limit = if folded.is_empty() {
                width
            } else {
                width - prefix_width
            };

            if token.is_space {
                // Blanks never open a continuation line (the leading indent of
                // the very first line is text, not a fold artefact), and never
                // overflow one.
                let opens_a_fold = current.is_empty() && !folded.is_empty();
                if !opens_a_fold && current_width + str_width(remaining) <= limit {
                    current_width += str_width(remaining);
                    current.push(Span::styled(remaining.to_string(), token.style));
                }
                break;
            }

            let token_width = str_width(remaining);
            if current_width + token_width <= limit {
                current.push(Span::styled(remaining.to_string(), token.style));
                current_width += token_width;
                break;
            }

            if current_width > 0 {
                // Try the word again on a fresh line.
                folded.push(std::mem::take(&mut current));
                current_width = 0;
                continue;
            }

            // The word alone is wider than the line: split it mid-word, taking
            // at least one character so this never spins.
            let (head, tail) = split_at_width(remaining, limit);
            let (head, tail) = if head.is_empty() {
                let idx = remaining
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| i)
                    .unwrap_or(remaining.len());
                remaining.split_at(idx)
            } else {
                (head, tail)
            };
            current.push(Span::styled(head.to_string(), token.style));
            folded.push(std::mem::take(&mut current));
            current_width = 0;
            remaining = tail;
            if remaining.is_empty() {
                break;
            }
        }
    }

    if !current.is_empty() || folded.is_empty() {
        folded.push(current);
    }

    folded
        .into_iter()
        .enumerate()
        .map(|(i, spans)| {
            if i == 0 || prefix_width == 0 {
                Line::from(spans)
            } else {
                let mut with_prefix = Vec::with_capacity(spans.len() + 1);
                with_prefix.push(prefix.clone());
                with_prefix.extend(spans);
                Line::from(with_prefix)
            }
        })
        .collect()
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

    // The picker is deliberately *not* initialized here: detecting the image
    // protocol means writing a query to the terminal and waiting for its answer,
    // which costs a full two-second timeout on terminals that never reply (#58).
    // The document is drawn first, and the query only happens for documents that
    // actually have something to draw.
    let needs_picker = document_needs_picker(&content);
    let rendered = build_content_elements(&content, &file_path, &None);
    let watcher_rx = crate::core::watcher::watch_file(&file_path)?;

    let mut app = TuiApp {
        content,
        rendered,
        toc_entries,
        file_path,
        watcher_rx,
        picker: None,
        picker_queried: false,
        content_width: 0,
        scroll_offset: 0,
        toc_selected: 0,
        focus_toc: false,
        should_quit: false,
        search_mode: false,
        search_query: String::new(),
        search_matches: Vec::new(),
        current_match_idx: 0,
    };

    // Show the document immediately, then pay for the capability query — and
    // only when the document has an image or a diagram to display.
    terminal.draw(|f| ui(f, &mut app))?;
    if needs_picker {
        ensure_picker(&mut app);
        if app.picker.is_some() {
            // Only worth re-rendering when the terminal can actually draw pixels.
            rebuild_rendered(&mut app);
        }
        // The query talks to the terminal behind ratatui's back; repaint from
        // scratch so a stray reply cannot be left on screen.
        terminal.clear()?;
    }

    // Main loop
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        // Check for file changes
        if app.watcher_rx.try_recv().is_ok() {
            while app.watcher_rx.try_recv().is_ok() {}
            if let Ok(new_content) = std::fs::read_to_string(&app.file_path) {
                app.toc_entries = toc::extract_toc(&new_content);
                if document_needs_picker(&new_content) {
                    ensure_picker(&mut app);
                }
                app.content = new_content;
                rebuild_rendered(&mut app);
            }
        }

        // Poll events with 100ms timeout for file watching
        if event::poll(std::time::Duration::from_millis(100))? {
            let ev = event::read()?;
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
                                app.current_match_idx =
                                    (app.current_match_idx + 1) % app.search_matches.len();
                                app.scroll_offset = app.search_matches[app.current_match_idx];
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
                                app.current_match_idx =
                                    (app.current_match_idx + 1) % app.search_matches.len();
                                app.scroll_offset = app.search_matches[app.current_match_idx];
                            }
                        }
                        KeyCode::Char('N') => {
                            if !app.search_matches.is_empty() {
                                app.current_match_idx = if app.current_match_idx == 0 {
                                    app.search_matches.len() - 1
                                } else {
                                    app.current_match_idx - 1
                                };
                                app.scroll_offset = app.search_matches[app.current_match_idx];
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
                            let total_rows = total_content_rows(&app.rendered);
                            app.scroll_offset = total_rows.saturating_sub(1);
                        }
                        KeyCode::Tab => {
                            app.focus_toc = !app.focus_toc;
                        }
                        KeyCode::Enter if app.focus_toc => {
                            if let Some(offset) =
                                find_heading_row(&app.rendered, &app.toc_entries, app.toc_selected)
                            {
                                app.scroll_offset = offset;
                                app.focus_toc = false;
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
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

struct TuiApp {
    content: String,
    rendered: Vec<ContentElement>,
    toc_entries: Vec<TocEntry>,
    file_path: PathBuf,
    watcher_rx: Receiver<()>,
    /// The terminal's image protocol, once it has been asked for. `None` means
    /// either "not asked yet" or "the terminal cannot display images"; the
    /// `picker_queried` flag tells the two apart.
    picker: Option<Picker>,
    picker_queried: bool,
    /// Width the text is currently wrapped to, in columns. 0 until the first
    /// frame tells us how wide the content panel really is.
    content_width: usize,
    scroll_offset: usize,
    toc_selected: usize,
    focus_toc: bool,
    should_quit: bool,
    search_mode: bool,
    search_query: String,
    search_matches: Vec<usize>,
    current_match_idx: usize,
}

/// Ask the terminal which image protocol it speaks, at most once per run.
///
/// `Picker::from_query_stdio()` writes an escape sequence and blocks until the
/// terminal answers — two seconds on terminals that never do. Calling it lazily
/// keeps that cost out of the startup path of text-only documents (#58).
fn ensure_picker(app: &mut TuiApp) {
    if app.picker_queried {
        return;
    }
    app.picker_queried = true;
    app.picker = Picker::from_query_stdio().ok();
}

/// Rebuild the rendered elements from the current document content.
fn rebuild_rendered(app: &mut TuiApp) {
    let content = std::mem::take(&mut app.content);
    app.rendered = build_content_elements(&content, &app.file_path, &app.picker);
    rewrap_elements(&mut app.rendered, app.content_width);
    app.content = content;
}

/// Row offsets of the lines matching `query`, in the current wrapped layout.
///
/// Offsets are counted in *rendered* rows, wrapped height included, so that
/// scrolling to a match lands on it (#54).
fn compute_search_matches(elements: &[ContentElement], query: &str) -> Vec<usize> {
    let mut matches = Vec::new();
    if query.is_empty() {
        return matches;
    }
    let query_lower = query.to_lowercase();
    let mut row_offset: usize = 0;
    for element in elements {
        if let ContentElement::TextLine(text) | ContentElement::ImagePlaceholder(text) = element {
            if text.text().to_lowercase().contains(&query_lower) {
                matches.push(row_offset);
            }
        }
        row_offset += element.row_height() as usize;
    }
    matches
}

fn update_search_matches(app: &mut TuiApp) {
    app.search_matches = compute_search_matches(&app.rendered, &app.search_query);
    app.current_match_idx = 0;
    // Auto-scroll to first match
    if !app.search_matches.is_empty() {
        app.scroll_offset = app.search_matches[0];
    }
}

/// Calculate the total number of terminal rows occupied by all content elements.
fn total_content_rows(elements: &[ContentElement]) -> usize {
    elements.iter().map(|e| e.row_height() as usize).sum()
}

fn ui(f: &mut Frame, app: &mut TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(1)])
        .split(f.area());

    // TOC sidebar
    let toc_items: Vec<ListItem> = app
        .toc_entries
        .iter()
        .map(|entry| {
            let indent = "  ".repeat((entry.level as usize).saturating_sub(1));
            let style = match entry.level {
                1 => Style::default().fg(Color::Cyan).bold(),
                2 => Style::default().fg(Color::Blue).bold(),
                3 => Style::default().fg(Color::White),
                _ => Style::default().fg(Color::DarkGray),
            };
            ListItem::new(format!("{}{}", indent, entry.text)).style(style)
        })
        .collect();

    let toc_border_style = if app.focus_toc {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let toc = List::new(toc_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(toc_border_style)
                .title(" TOC ")
                .title_style(Style::default().bold()),
        )
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

    // Fold the text to the panel width, and only when that width changes: the
    // wrapped height feeds every scroll offset below (#54).
    if inner_area.width as usize != app.content_width {
        app.content_width = inner_area.width as usize;
        rewrap_elements(&mut app.rendered, app.content_width);
        app.search_matches = compute_search_matches(&app.rendered, &app.search_query);
        if app.current_match_idx >= app.search_matches.len() {
            app.current_match_idx = 0;
        }
    }

    let content_height = inner_area.height as usize;
    let total_rows = total_content_rows(&app.rendered);
    let max_scroll = total_rows.saturating_sub(content_height);
    let scroll = app.scroll_offset.min(max_scroll);

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
    render_content_elements(
        f,
        inner_area,
        &mut app.rendered,
        scroll,
        content_height,
        &app.search_matches,
        app.current_match_idx,
    );

    // Bottom bar
    let bar_text = if app.search_mode {
        let match_info = if app.search_matches.is_empty() {
            if app.search_query.is_empty() {
                String::new()
            } else {
                " (no matches)".to_string()
            }
        } else {
            format!(
                " ({}/{})",
                app.current_match_idx + 1,
                app.search_matches.len()
            )
        };
        format!(
            " /{}{}  [Enter: next | Esc: close]",
            app.search_query, match_info
        )
    } else if !app.search_matches.is_empty() {
        format!(
            " Search: '{}' ({}/{})  [n/N: next/prev | /: search]",
            app.search_query,
            app.current_match_idx + 1,
            app.search_matches.len()
        )
    } else {
        " q: quit | Tab: switch focus | j/k: scroll | /: search | Space/PgDn: page down "
            .to_string()
    };

    let help_area = Rect {
        x: content_area.x + 1,
        y: content_area.y + content_area.height - 1,
        width: content_area
            .width
            .saturating_sub(2)
            .min(bar_text.len() as u16),
        height: 1,
    };

    let bar_style = if app.search_mode {
        Style::default()
            .fg(Color::Yellow)
            .bg(Color::Rgb(40, 40, 40))
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let help_widget = Paragraph::new(bar_text).style(bar_style);
    f.render_widget(help_widget, help_area);
}

/// Render content elements into the given area, handling scroll offset.
/// This function iterates through elements, skipping rows according to the scroll offset,
/// and renders visible text lines and images. Search matches are highlighted.
fn render_content_elements(
    f: &mut Frame,
    area: Rect,
    elements: &mut [ContentElement],
    scroll: usize,
    content_height: usize,
    search_matches: &[usize],
    current_match: usize,
) {
    let mut rows_skipped: usize = 0;
    let mut y_offset: u16 = 0;
    let available_height = content_height as u16;
    // Track absolute row offset for each element (independent of scroll)
    let mut absolute_row: usize = 0;

    for element in elements.iter_mut() {
        if y_offset >= available_height {
            break;
        }

        let elem_height = element.row_height() as usize;
        let current_absolute_row = absolute_row;
        absolute_row += elem_height;

        // Check if this element is before the scroll window
        if rows_skipped + elem_height <= scroll {
            rows_skipped += elem_height;
            continue;
        }

        // This element is at least partially visible
        let skip_within = scroll.saturating_sub(rows_skipped);
        rows_skipped += elem_height;

        match element {
            ContentElement::TextLine(text) | ContentElement::ImagePlaceholder(text) => {
                // A search hit is recorded at the first row of the logical line,
                // so every one of its wrapped rows is highlighted together.
                let is_match = search_matches.contains(&current_absolute_row);
                let is_current =
                    is_match && search_matches.get(current_match) == Some(&current_absolute_row);

                for line in text.lines.iter().skip(skip_within) {
                    if y_offset >= available_height {
                        break;
                    }
                    let line_area = Rect {
                        x: area.x,
                        y: area.y + y_offset,
                        width: area.width,
                        height: 1,
                    };
                    let rendered = if is_match {
                        highlight_line(line, is_current)
                    } else {
                        line.clone()
                    };
                    f.render_widget(Paragraph::new(rendered), line_area);
                    y_offset += 1;
                }
            }
            ContentElement::Image {
                protocol, height, ..
            } => {
                // Show the visible portion of the image.
                // When partially scrolled, show only the remaining rows.
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
                    width: area.width,
                    height: render_height,
                };
                let image_widget = StatefulImage::default().resize(Resize::Fit(None));
                f.render_stateful_widget(image_widget, img_area, protocol.as_mut());
                y_offset += render_height;
            }
        }
    }
}

/// Repaint a line with the search highlight, keeping each span's own styling.
fn highlight_line(line: &Line<'static>, is_current: bool) -> Line<'static> {
    Line::from(
        line.spans
            .iter()
            .map(|s| {
                let style = if is_current {
                    s.style.bg(Color::Yellow).fg(Color::Black)
                } else {
                    s.style.bg(Color::Rgb(80, 80, 0))
                };
                Span::styled(s.content.clone(), style)
            })
            .collect::<Vec<_>>(),
    )
}

/// Find the row offset where a heading appears in the rendered output.
fn find_heading_row(
    elements: &[ContentElement],
    toc_entries: &[TocEntry],
    toc_index: usize,
) -> Option<usize> {
    let entry = toc_entries.get(toc_index)?;
    let search_text = &entry.text;
    let mut row_offset: usize = 0;

    for element in elements {
        if let ContentElement::TextLine(text) | ContentElement::ImagePlaceholder(text) = element {
            if text.text().contains(search_text) {
                return Some(row_offset);
            }
        }
        row_offset += element.row_height() as usize;
    }

    None
}

/// Build content elements from markdown, loading images where possible.
fn build_content_elements(
    content: &str,
    file_path: &PathBuf,
    picker: &Option<Picker>,
) -> Vec<ContentElement> {
    let text_lines = markdown_to_lines_with_images(content);
    let canonical_file = std::fs::canonicalize(file_path).unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|cwd| cwd.join(file_path))
            .unwrap_or_else(|_| file_path.clone())
    });
    let base_dir = canonical_file
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    let mut elements = Vec::new();
    for item in text_lines {
        match item {
            ParsedLine::Text(line) => {
                elements.push(ContentElement::TextLine(WrappedText::new(line)));
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
                                    let target_rows =
                                        ((target_cols as f64) * aspect / 2.0).ceil() as u16;
                                    let height = target_rows.clamp(4, 40);

                                    let protocol = Box::new(picker.new_resize_protocol(dyn_img));
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

                            let protocol = Box::new(picker.new_resize_protocol(dyn_img));
                            elements.push(ContentElement::Image {
                                protocol,
                                _alt: alt,
                                height,
                            });
                        }
                        Err(_) => {
                            let label = if alt.is_empty() {
                                "image".to_string()
                            } else {
                                alt
                            };
                            elements.push(ContentElement::ImagePlaceholder(WrappedText::new(
                                Line::from(Span::styled(
                                    format!("[Image: {}]", label),
                                    Style::default().fg(Color::Magenta).italic(),
                                )),
                            )));
                        }
                    }
                } else {
                    // No picker available (terminal doesn't support image protocols or detection failed)
                    let label = if alt.is_empty() {
                        "image".to_string()
                    } else {
                        alt
                    };
                    elements.push(ContentElement::ImagePlaceholder(WrappedText::new(
                        Line::from(Span::styled(
                            format!("[Image: {}]", label),
                            Style::default().fg(Color::Magenta).italic(),
                        )),
                    )));
                }
            }
        }
    }

    elements
}

/// Push a mermaid code block as fallback text when rendering fails or no picker is available.
fn push_mermaid_fallback_code(elements: &mut Vec<ContentElement>, source: &str) {
    elements.push(ContentElement::TextLine(WrappedText::new(Line::from(
        Span::styled(
            code_frame_top("mermaid"),
            Style::default().fg(Color::DarkGray),
        ),
    ))));
    for line in source.lines() {
        elements.push(ContentElement::TextLine(WrappedText::new(Line::from(
            Span::styled(format!("│ {}", line), Style::default().fg(Color::Green)),
        ))));
    }
    elements.push(ContentElement::TextLine(WrappedText::new(Line::from(
        Span::styled(CODE_FRAME_BOTTOM, Style::default().fg(Color::DarkGray)),
    ))));
    elements.push(ContentElement::TextLine(WrappedText::new(Line::from(""))));
}

/// Load an image from a URL, data URI, or local file path.
/// SVG files are rasterized via resvg/usvg before returning.
fn load_image(
    url: &str,
    base_dir: &std::path::Path,
) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
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
        // Path traversal protection: the image must stay inside the enclosing
        // project (see core::paths), so `../images/logo.png` from `docs/page.md`
        // works while `../../../etc/passwd` does not.
        if path.exists() && !crate::core::paths::is_within_image_root(&path, base_dir) {
            return Err("path traversal blocked: image path escapes the project directory".into());
        }
        crate::core::image_validation::validate_image_file(&path)
            .map_err(|e| format!("invalid image file: {}", e))?;
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
        return Err(format!(
            "data URI too large ({} bytes, max {})",
            uri.len(),
            MAX_DATA_URI_LEN
        )
        .into());
    }
    // Format: data:[<mediatype>][;base64],<data>
    let comma_pos = uri.find(',').ok_or("Invalid data URI: no comma found")?;
    let header = &uri[..comma_pos];
    let data_part = &uri[comma_pos + 1..];
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data_part)?;
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

    let options = usvg::Options {
        fontdb: Arc::clone(fontdb),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg_data, &options)?;
    let size = tree.size();
    let width = size.width() as u32;
    let height = size.height() as u32;

    if width == 0 || height == 0 {
        return Err("SVG has zero dimensions".into());
    }

    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("Failed to create pixmap")?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

    // Convert RGBA pixmap to DynamicImage
    let img = image::RgbaImage::from_raw(width, height, pixmap.data().to_vec())
        .ok_or("Failed to create image from pixmap")?;
    Ok(image::DynamicImage::ImageRgba8(img))
}

/// Load an image from an HTTP(S) URL using ureq (30s timeout).
fn load_image_from_http(url: &str) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
    use std::sync::OnceLock;
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .into()
    });
    let response = agent.get(url).call()?;
    let mut bytes = Vec::new();
    response.into_body().into_reader().read_to_end(&mut bytes)?;
    let img = image::load_from_memory(&bytes)?;
    Ok(img)
}

/// Intermediate representation for parsed markdown lines.
enum ParsedLine {
    Text(Line<'static>),
    ImageRef {
        alt: String,
        url: String,
    },
    /// A mermaid diagram source extracted from a ```mermaid code block.
    MermaidRef {
        source: String,
    },
}

/// Whether this document has anything that has to be drawn as pixels: a
/// standalone image or a mermaid diagram.
///
/// This is the gate for the terminal capability query (#58). It is deliberately
/// built on the very same parser that produces the rendered elements, so the
/// answer can never disagree with what is actually displayed: an image written
/// inside a paragraph or a code block is shown as text and needs no picker.
fn document_needs_picker(content: &str) -> bool {
    markdown_to_lines_with_images(content).iter().any(|item| {
        matches!(
            item,
            ParsedLine::ImageRef { .. } | ParsedLine::MermaidRef { .. }
        )
    })
}

/// Convert markdown content to a mix of styled text lines and image references.
/// The bottom edge of a code block frame.
const CODE_FRAME_BOTTOM: &str = "└─────────────────────────────────────────┘";

/// The top edge, labelled and closed at exactly the width of the bottom one.
/// A named language used to leave the box open on the right.
fn code_frame_top(label: &str) -> String {
    let inner = str_width(CODE_FRAME_BOTTOM).saturating_sub(2);
    let opening = format!("─ {} ", label);
    let fill = inner.saturating_sub(str_width(&opening));
    format!("┌{}{}┐", opening, "─".repeat(fill))
}

/// Whether the terminal says it has a light background.
///
/// `COLORFGBG` is the only signal available without talking to the terminal and
/// waiting for an answer — which is exactly the two-second stall #58 removed, so
/// it is not an option here. Terminals that do not set the variable simply give
/// no answer, and the caller falls back to dark.
///
/// The value is `fg;bg` or `fg;<something>;bg`; the background is the last
/// field, as an ANSI colour index. 0-6 and 8 are the dark half of the palette,
/// 7 and 9-15 the light half.
fn terminal_background_is_light(colorfgbg: Option<&str>) -> Option<bool> {
    let value = colorfgbg?;
    let bg = value.rsplit(';').next()?.trim();
    let index: u8 = bg.parse().ok()?;
    match index {
        0..=6 | 8 => Some(false),
        7 | 9..=15 => Some(true),
        _ => None,
    }
}

/// The syntect theme to highlight code with.
///
/// An explicit setting always wins; `auto` asks the terminal and falls back to
/// dark, which is what the overwhelming majority of terminals running a pager
/// actually are.
fn syntax_theme_name(setting: crate::core::Theme, colorfgbg: Option<&str>) -> &'static str {
    let light = match setting {
        crate::core::Theme::Light => true,
        crate::core::Theme::Dark => false,
        crate::core::Theme::Auto => terminal_background_is_light(colorfgbg).unwrap_or(false),
    };
    if light {
        "InspiredGitHub"
    } else {
        "base16-ocean.dark"
    }
}

/// Syntax highlighting assets, built once. `SyntaxSet` parsing is the expensive
/// part, so it is shared across every code block of every reload.
fn syntax_assets() -> &'static (syntect::parsing::SyntaxSet, syntect::highlighting::Theme) {
    use std::sync::OnceLock;
    static ASSETS: OnceLock<(syntect::parsing::SyntaxSet, syntect::highlighting::Theme)> =
        OnceLock::new();
    ASSETS.get_or_init(|| {
        let syntaxes = syntect::parsing::SyntaxSet::load_defaults_newlines();
        let mut themes = syntect::highlighting::ThemeSet::load_defaults();
        let wanted = syntax_theme_name(
            crate::core::theme(),
            std::env::var("COLORFGBG").ok().as_deref(),
        );
        let theme = themes
            .themes
            .remove(wanted)
            .or_else(|| themes.themes.remove("base16-ocean.dark"))
            .unwrap_or_default();
        (syntaxes, theme)
    })
}

/// Colour one code block, one `Vec<Span>` per source line (#59).
///
/// Falls back to a single uncoloured span per line when the language is unknown
/// or highlighting fails, so an exotic fence never costs more than colour.
fn highlight_code(code: &str, lang: &str) -> Vec<Vec<Span<'static>>> {
    let plain = |code: &str| -> Vec<Vec<Span<'static>>> {
        code.lines()
            .map(|l| {
                vec![Span::styled(
                    l.to_string(),
                    Style::default().fg(Color::Green),
                )]
            })
            .collect()
    };

    let (syntaxes, theme) = syntax_assets();
    let Some(syntax) = syntaxes
        .find_syntax_by_token(lang)
        .or_else(|| syntaxes.find_syntax_by_extension(lang))
    else {
        return plain(code);
    };

    let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme);
    let mut out = Vec::new();
    for line in code.lines() {
        // `load_defaults_newlines` expects the newline to be present.
        let with_newline = format!("{}\n", line);
        match highlighter.highlight_line(&with_newline, syntaxes) {
            Ok(ranges) => out.push(
                ranges
                    .into_iter()
                    .map(|(style, text)| {
                        let c = style.foreground;
                        Span::styled(
                            text.trim_end_matches('\n').to_string(),
                            Style::default().fg(Color::Rgb(c.r, c.g, c.b)),
                        )
                    })
                    .filter(|s| !s.content.is_empty())
                    .collect(),
            ),
            Err(_) => return plain(code),
        }
    }
    out
}

/// How deep inside lists and block quotes a block sits.
#[derive(Clone, Copy, Default)]
struct BlockCtx {
    indent: usize,
    quote: usize,
    /// Inside a tight list, paragraphs must not be separated by a blank line —
    /// that is what "tight" means in CommonMark.
    tight: bool,
}

impl BlockCtx {
    fn indented(self, by: usize) -> Self {
        Self {
            indent: self.indent + by,
            ..self
        }
    }
    fn quoted(self) -> Self {
        Self {
            quote: self.quote + 1,
            ..self
        }
    }
    fn tight(self, tight: bool) -> Self {
        Self { tight, ..self }
    }
    /// The blanks and quote bars every line of this block starts with.
    fn prefix(self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        if self.indent > 0 {
            spans.push(Span::raw(" ".repeat(self.indent)));
        }
        for _ in 0..self.quote {
            spans.push(Span::styled("▎ ", Style::default().fg(Color::DarkGray)));
        }
        spans
    }
}

/// Renders the comrak AST to terminal lines (#59).
///
/// The terminal output is derived from the very same parse the table of
/// contents and the other two backends use, so the three cannot drift apart on
/// what a heading, a list or a table is. That is what makes h5/h6, syntax
/// highlighting, aligned tables and footnotes fall out rather than being four
/// separate special cases.
struct MdRenderer {
    out: Vec<ParsedLine>,
    /// Footnote definitions, rendered together at the end of the document as
    /// the HTML backends do, whatever their position in the source.
    footnotes: Vec<(String, Vec<ParsedLine>)>,
}

type AstNode<'a> = comrak::arena_tree::Node<'a, std::cell::RefCell<comrak::nodes::Ast>>;

impl MdRenderer {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            footnotes: Vec::new(),
        }
    }

    fn push(&mut self, ctx: BlockCtx, mut spans: Vec<Span<'static>>) {
        let mut line = ctx.prefix();
        line.append(&mut spans);
        self.out.push(ParsedLine::Text(Line::from(line)));
    }

    fn blank(&mut self) {
        // Never open on a blank line, and never repeat one.
        if matches!(self.out.last(), None | Some(ParsedLine::Text(_)))
            && self.plain_last().is_some_and(|t| t.trim().is_empty())
        {
            return;
        }
        if self.out.is_empty() {
            return;
        }
        self.out.push(ParsedLine::Text(Line::from("")));
    }

    fn plain_last(&self) -> Option<String> {
        match self.out.last() {
            Some(ParsedLine::Text(l)) => Some(l.spans.iter().map(|s| s.content.as_ref()).collect()),
            _ => None,
        }
    }

    fn children<'a>(&mut self, node: &'a AstNode<'a>, ctx: BlockCtx) {
        for child in node.children() {
            self.block(child, ctx);
        }
    }

    fn block<'a>(&mut self, node: &'a AstNode<'a>, ctx: BlockCtx) {
        use comrak::nodes::{ListType, NodeValue};

        let value = node.data.borrow().value.clone();
        match value {
            NodeValue::Document => self.children(node, ctx),

            NodeValue::FrontMatter(_) => {}

            NodeValue::Heading(h) => {
                let text: String = inline_text(node);
                let spans = inlines(node, heading_style(h.level));
                if h.level <= 2 {
                    self.blank();
                }
                self.push(ctx, spans);
                if let Some(rule) = heading_rule(h.level, &text) {
                    self.push(ctx, vec![rule]);
                }
                self.blank();
            }

            NodeValue::Paragraph => {
                // A paragraph that is nothing but an image is the one case the
                // terminal can draw as pixels.
                if let Some(image) = lone_image(node) {
                    self.out.push(image);
                    return;
                }
                self.push(ctx, inlines(node, Style::default()));
                if !ctx.tight {
                    self.blank();
                }
            }

            NodeValue::BlockQuote => {
                self.children(node, ctx.quoted());
                self.blank();
            }

            NodeValue::CodeBlock(code) => {
                let lang = code
                    .info
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string();
                if lang == "mermaid" {
                    self.out.push(ParsedLine::MermaidRef {
                        source: code.literal.trim_end().to_string(),
                    });
                    return;
                }
                let gutter = Style::default().fg(Color::DarkGray);
                let label = if lang.is_empty() { "code" } else { &lang };
                self.push(ctx, vec![Span::styled(code_frame_top(label), gutter)]);
                for mut spans in highlight_code(code.literal.trim_end_matches('\n'), &lang) {
                    let mut line = vec![Span::styled("│ ", gutter)];
                    line.append(&mut spans);
                    self.push(ctx, line);
                }
                self.push(ctx, vec![Span::styled(CODE_FRAME_BOTTOM, gutter)]);
                self.blank();
            }

            NodeValue::List(list) => {
                self.children(node, ctx.tight(list.tight));
                // `ctx` here is still the *enclosing* context: a list nested
                // inside a tight one must not add breathing room of its own.
                if !ctx.tight {
                    self.blank();
                }
            }

            NodeValue::Item(list) => {
                let marker = match list.list_type {
                    ListType::Bullet => "• ".to_string(),
                    ListType::Ordered => format!("{}. ", list.start),
                };
                self.list_item(node, ctx, marker);
            }

            NodeValue::TaskItem(task) => {
                let marker = if task.symbol.is_some() {
                    "☑ "
                } else {
                    "☐ "
                };
                self.list_item(node, ctx, marker.to_string());
            }

            NodeValue::ThematicBreak => {
                self.push(
                    ctx,
                    vec![Span::styled(
                        "─".repeat(60),
                        Style::default().fg(Color::DarkGray),
                    )],
                );
                self.blank();
            }

            NodeValue::Table(table) => self.table(node, ctx, &table.alignments),

            NodeValue::FootnoteDefinition(def) => {
                let mut sub = MdRenderer::new();
                sub.children(node, BlockCtx::default());
                self.footnotes.push((def.name.clone(), sub.out));
            }

            NodeValue::HtmlBlock(html) => {
                // Raw HTML has no terminal rendering; show it as dim text rather
                // than dropping content the author wrote.
                for line in html.literal.lines() {
                    self.push(
                        ctx,
                        vec![Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::DarkGray),
                        )],
                    );
                }
                self.blank();
            }

            // Anything else that can hold blocks is walked through.
            _ => self.children(node, ctx),
        }
    }

    fn list_item<'a>(&mut self, node: &'a AstNode<'a>, ctx: BlockCtx, marker: String) {
        let before = self.out.len();
        self.children(node, ctx.indented(marker.chars().count()));
        // The marker replaces the indent of the item's first line, so a wrapped
        // continuation lines up under the text (see `continuation_prefix`).
        if let Some(ParsedLine::Text(line)) = self.out.get_mut(before) {
            let indent = ctx.indent;
            let mut spans = std::mem::take(&mut line.spans);
            if !spans.is_empty() && spans[0].content.chars().all(|c| c == ' ') {
                spans.remove(0);
            }
            let mut prefixed = Vec::new();
            if indent > 0 {
                prefixed.push(Span::raw(" ".repeat(indent)));
            }
            prefixed.push(Span::styled(marker, Style::default().fg(Color::Cyan)));
            prefixed.append(&mut spans);
            *line = Line::from(prefixed);
        }
    }

    fn table<'a>(
        &mut self,
        node: &'a AstNode<'a>,
        ctx: BlockCtx,
        alignments: &[comrak::nodes::TableAlignment],
    ) {
        use comrak::nodes::NodeValue;

        // First pass: render every cell, and measure the columns (#59).
        let mut rows: Vec<(bool, Vec<Vec<Span<'static>>>)> = Vec::new();
        for row in node.children() {
            let NodeValue::TableRow(is_header) = row.data.borrow().value else {
                continue;
            };
            let cells: Vec<Vec<Span<'static>>> = row
                .children()
                .map(|cell| {
                    let style = if is_header {
                        Style::default().bold()
                    } else {
                        Style::default()
                    };
                    inlines(cell, style)
                })
                .collect();
            rows.push((is_header, cells));
        }
        if rows.is_empty() {
            return;
        }

        let columns = rows.iter().map(|(_, c)| c.len()).max().unwrap_or(0);
        let mut widths = vec![0usize; columns];
        for (_, cells) in &rows {
            for (i, cell) in cells.iter().enumerate() {
                let w: usize = cell.iter().map(|s| s.width()).sum();
                widths[i] = widths[i].max(w);
            }
        }

        let sep = Style::default().fg(Color::DarkGray);
        for (index, (is_header, cells)) in rows.iter().enumerate() {
            let mut line: Vec<Span<'static>> = Vec::new();
            for (col, width) in widths.iter().enumerate() {
                if col > 0 {
                    line.push(Span::styled(" │ ", sep));
                }
                let empty = Vec::new();
                let cell = cells.get(col).unwrap_or(&empty);
                let used: usize = cell.iter().map(|s| s.width()).sum();
                let pad = width.saturating_sub(used);
                let align = alignments
                    .get(col)
                    .copied()
                    .unwrap_or(comrak::nodes::TableAlignment::None);
                let (left, right) = match align {
                    comrak::nodes::TableAlignment::Right => (pad, 0),
                    comrak::nodes::TableAlignment::Center => (pad / 2, pad - pad / 2),
                    _ => (0, pad),
                };
                if left > 0 {
                    line.push(Span::raw(" ".repeat(left)));
                }
                line.extend(cell.iter().cloned());
                if right > 0 {
                    line.push(Span::raw(" ".repeat(right)));
                }
            }
            self.push(ctx, line);

            if *is_header || (index == 0 && rows.len() > 1) {
                let rule: Vec<Span<'static>> = (0..columns)
                    .map(|col| {
                        let mut s = String::new();
                        if col > 0 {
                            s.push_str("─┼─");
                        }
                        s.push_str(&"─".repeat(widths[col]));
                        Span::styled(s, sep)
                    })
                    .collect();
                self.push(ctx, rule);
            }
        }
        self.blank();
    }

    fn finish(mut self) -> Vec<ParsedLine> {
        if !self.footnotes.is_empty() {
            let notes = std::mem::take(&mut self.footnotes);
            self.blank();
            self.push(
                BlockCtx::default(),
                vec![Span::styled(
                    "─".repeat(20),
                    Style::default().fg(Color::DarkGray),
                )],
            );
            for (name, body) in notes {
                let mut body = body.into_iter();
                if let Some(ParsedLine::Text(first)) = body.next() {
                    let mut spans = vec![Span::styled(
                        format!("[{}] ", name),
                        Style::default().fg(Color::Yellow).bold(),
                    )];
                    spans.extend(first.spans);
                    self.out.push(ParsedLine::Text(Line::from(spans)));
                }
                self.out.extend(body);
            }
        }
        // Never end on padding.
        while matches!(self.plain_last(), Some(t) if t.trim().is_empty()) {
            self.out.pop();
        }
        self.out
    }
}

fn heading_style(level: u8) -> Style {
    let base = Style::default().bold();
    match level {
        1 => base.fg(Color::Cyan).underlined(),
        2 => base.fg(Color::Blue),
        3 => base.fg(Color::Yellow),
        4 => base.fg(Color::Magenta),
        5 => base.fg(Color::Green),
        _ => base.fg(Color::Gray),
    }
}

/// The rule drawn under a heading, for the two levels that get one.
fn heading_rule(level: u8, text: &str) -> Option<Span<'static>> {
    let width = str_width(text);
    match level {
        1 => Some(Span::styled(
            "═".repeat(width.min(60)),
            Style::default().fg(Color::Cyan),
        )),
        2 => Some(Span::styled(
            "─".repeat(width.min(50)),
            Style::default().fg(Color::Blue),
        )),
        _ => None,
    }
}

/// The image of a paragraph that holds nothing else — the only shape the
/// terminal can draw as pixels. Anything else stays text.
fn lone_image<'a>(paragraph: &'a AstNode<'a>) -> Option<ParsedLine> {
    use comrak::nodes::NodeValue;
    let mut image = None;
    for child in paragraph.children() {
        match &child.data.borrow().value {
            NodeValue::Image(link) => {
                if image.is_some() {
                    return None;
                }
                image = Some((inline_text(child), link.url.clone()));
            }
            NodeValue::Text(t) if t.trim().is_empty() => {}
            NodeValue::SoftBreak => {}
            _ => return None,
        }
    }
    image.map(|(alt, url)| ParsedLine::ImageRef { alt, url })
}

/// The plain text of a node's inline content.
fn inline_text<'a>(node: &'a AstNode<'a>) -> String {
    use comrak::nodes::NodeValue;
    let mut out = String::new();
    for child in node.descendants() {
        match &child.data.borrow().value {
            NodeValue::Text(t) => out.push_str(t),
            NodeValue::Code(c) => out.push_str(&c.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
            _ => {}
        }
    }
    out
}

/// Render the inline children of `node` as styled spans.
fn inlines<'a>(node: &'a AstNode<'a>, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for child in node.children() {
        inline_into(child, base, &mut spans);
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

fn inline_into<'a>(node: &'a AstNode<'a>, style: Style, out: &mut Vec<Span<'static>>) {
    use comrak::nodes::NodeValue;
    let value = node.data.borrow().value.clone();
    match value {
        NodeValue::Text(text) => out.push(Span::styled(text.to_string(), style)),
        NodeValue::Code(code) => out.push(Span::styled(
            code.literal.clone(),
            style.fg(Color::Green).bg(Color::Rgb(40, 40, 40)),
        )),
        NodeValue::Emph => descend(node, style.italic(), out),
        NodeValue::Strong => descend(node, style.bold(), out),
        NodeValue::Strikethrough => descend(node, style.crossed_out(), out),
        NodeValue::Underline => descend(node, style.underlined(), out),
        NodeValue::SoftBreak | NodeValue::LineBreak => out.push(Span::styled(" ", style)),
        NodeValue::Link(_) => descend(node, style.fg(Color::Blue).underlined(), out),
        NodeValue::Image(link) => {
            // An image sharing a paragraph with text cannot be drawn as pixels,
            // so it is named instead of dropped.
            let alt = inline_text(node);
            let label = if alt.is_empty() {
                link.url.clone()
            } else {
                alt
            };
            out.push(Span::styled(
                format!("[{}]", label),
                style.fg(Color::Magenta).italic(),
            ));
        }
        NodeValue::FootnoteReference(fr) => out.push(Span::styled(
            format!("[{}]", fr.name),
            style.fg(Color::Yellow),
        )),
        NodeValue::HtmlInline(html) => {
            out.push(Span::styled(html.clone(), style.fg(Color::DarkGray)))
        }
        NodeValue::Escaped => descend(node, style, out),
        _ => descend(node, style, out),
    }
}

fn descend<'a>(node: &'a AstNode<'a>, style: Style, out: &mut Vec<Span<'static>>) {
    for child in node.children() {
        inline_into(child, style, out);
    }
}

/// Parse `content` and render it to terminal lines.
///
/// Uses exactly the comrak options `core::toc` and `core::markdown` use, so the
/// terminal, the table of contents and the two graphical backends agree on the
/// structure of the document (#59).
fn markdown_to_lines_with_images(content: &str) -> Vec<ParsedLine> {
    use comrak::{parse_document, Arena, Options};

    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.front_matter_delimiter = Some("---".to_string());

    let root = parse_document(&arena, content, &options);
    let mut renderer = MdRenderer::new();
    renderer.block(root, BlockCtx::default());
    renderer.finish()
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
        assert!(
            result.is_ok(),
            "load_image should handle SVG files but got: {:?}",
            result.err()
        );
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

        // Should have parsed lines including the image reference
        // Without a picker, SVG falls back to placeholder — but the markdown parser should find it
        let has_image_ref = elements
            .iter()
            .any(|e| matches!(e, ContentElement::ImagePlaceholder(_)));
        assert!(
            has_image_ref,
            "Should find an image placeholder for the SVG reference"
        );

        // Now test load_image directly to confirm SVG rasterization works
        let img = load_image("logo.svg", &dir);
        assert!(
            img.is_ok(),
            "load_image should rasterize SVG, got: {:?}",
            img.err()
        );
        let img = img.unwrap();
        assert_eq!(img.width(), 100);
        assert_eq!(img.height(), 100);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_image_svg_data_uri() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="50"><circle cx="25" cy="25" r="20" fill="blue"/></svg>"#;
        let b64 =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, svg.as_bytes());
        let data_uri = format!("data:image/svg+xml;base64,{}", b64);

        let result = load_image(&data_uri, std::path::Path::new("."));
        assert!(
            result.is_ok(),
            "load_image should handle SVG data URIs but got: {:?}",
            result.err()
        );
    }

    #[test]
    fn mermaid_block_produces_mermaid_ref() {
        let md = "# Title\n\n```mermaid\ngraph LR\n  A-->B\n```\n\nSome text after.\n";
        let items = markdown_to_lines_with_images(md);

        let has_mermaid_ref = items
            .iter()
            .any(|item| matches!(item, ParsedLine::MermaidRef { .. }));
        assert!(
            has_mermaid_ref,
            "Mermaid code block should produce a MermaidRef variant"
        );

        // Verify the source is captured correctly
        let mermaid_source = items
            .iter()
            .find_map(|item| {
                if let ParsedLine::MermaidRef { source } = item {
                    Some(source.clone())
                } else {
                    None
                }
            })
            .expect("Should have a MermaidRef");
        assert!(
            mermaid_source.contains("graph LR"),
            "MermaidRef should contain the mermaid source, got: {}",
            mermaid_source
        );
        assert!(
            mermaid_source.contains("A-->B"),
            "MermaidRef should contain the diagram content"
        );
    }

    #[test]
    fn mermaid_block_not_rendered_as_code_text() {
        let md = "```mermaid\ngraph LR\n  A-->B\n```\n";
        let items = markdown_to_lines_with_images(md);

        // Should NOT have green code lines for mermaid content
        let has_green_code = items.iter().any(|item| {
            if let ParsedLine::Text(line) = item {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                text.contains("│ graph LR") || text.contains("│   A-->B")
            } else {
                false
            }
        });
        assert!(
            !has_green_code,
            "Mermaid content should NOT appear as regular code text"
        );
    }

    #[test]
    fn non_mermaid_code_block_unchanged() {
        let md = "```rust\nfn main() {}\n```\n";
        let items = markdown_to_lines_with_images(md);

        let has_mermaid_ref = items
            .iter()
            .any(|item| matches!(item, ParsedLine::MermaidRef { .. }));
        assert!(
            !has_mermaid_ref,
            "Non-mermaid code blocks should NOT produce MermaidRef"
        );

        // Should have regular code text
        let has_code_text = items.iter().any(|item| {
            if let ParsedLine::Text(line) = item {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                text.contains("│ fn main()")
            } else {
                false
            }
        });
        assert!(
            has_code_text,
            "Non-mermaid code should appear as regular code text"
        );
    }

    // --- #54: long lines must be wrapped, not cut off -------------------------

    fn plain_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_line_shorter_than_the_width_is_left_alone() {
        let line = Line::from("hello world");
        let out = wrap_line(&line, 40);
        assert_eq!(out.len(), 1);
        assert_eq!(plain_text(&out[0]), "hello world");
    }

    #[test]
    fn a_long_line_is_folded_at_word_boundaries() {
        let line = Line::from("the quick brown fox jumps over the lazy dog");
        let out = wrap_line(&line, 20);
        assert!(out.len() > 1, "a 43-column line must not fit in 20 columns");
        for l in &out {
            assert!(l.width() <= 20, "line too wide: {:?}", plain_text(l));
        }
        let joined = out
            .iter()
            .map(|l| plain_text(l).trim_end().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, "the quick brown fox jumps over the lazy dog");
    }

    #[test]
    fn a_word_longer_than_the_width_is_hard_split() {
        let line = Line::from("supercalifragilisticexpialidocious");
        let out = wrap_line(&line, 10);
        for l in &out {
            assert!(l.width() <= 10, "line too wide: {:?}", plain_text(l));
        }
        let joined: String = out.iter().map(|l| plain_text(l)).collect();
        assert_eq!(joined, "supercalifragilisticexpialidocious");
    }

    #[test]
    fn wrapping_keeps_the_style_of_every_span() {
        let line = Line::from(vec![
            Span::styled("aaaa bbbb ", Style::default().fg(Color::Red)),
            Span::styled("cccc dddd", Style::default().fg(Color::Blue)),
        ]);
        let out = wrap_line(&line, 12);
        assert!(out.len() > 1);

        let by_color = |color: Color| -> String {
            out.iter()
                .flat_map(|l| l.spans.iter())
                .filter(|s| s.style.fg == Some(color))
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .replace(' ', "")
        };
        assert_eq!(by_color(Color::Red), "aaaabbbb");
        assert_eq!(by_color(Color::Blue), "ccccdddd");
    }

    #[test]
    fn an_empty_line_stays_a_single_empty_line() {
        let out = wrap_line(&Line::from(""), 10);
        assert_eq!(out.len(), 1);
        assert_eq!(plain_text(&out[0]), "");
    }

    #[test]
    fn a_tiny_width_still_yields_the_whole_text() {
        let line = Line::from("alpha beta gamma");
        for width in 0..6 {
            let out = wrap_line(&line, width);
            assert!(!out.is_empty(), "width {} produced no line at all", width);
            let joined: String = out.iter().map(|l| plain_text(l)).collect();
            assert!(
                joined.replace(' ', "").contains("alphabetagamma"),
                "width {} lost text: {:?}",
                width,
                joined
            );
        }
    }

    #[test]
    fn a_wrapped_list_item_keeps_its_bullet_indent() {
        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled("\u{2022} ", Style::default().fg(Color::Cyan)),
            Span::raw("one two three four five six seven eight"),
        ]);
        let out = wrap_line(&line, 20);
        assert!(out.len() > 1);
        let second = plain_text(&out[1]);
        assert!(
            second.starts_with("    "),
            "continuation must line up under the item text, got {:?}",
            second
        );
        assert!(
            !second.contains('\u{2022}'),
            "the bullet must not be repeated: {:?}",
            second
        );
    }

    #[test]
    fn a_wrapped_code_line_keeps_its_gutter() {
        let line = Line::from(Span::styled(
            "\u{2502} let x = some_very_long_expression_here();",
            Style::default().fg(Color::Green),
        ));
        let out = wrap_line(&line, 20);
        assert!(out.len() > 1);
        assert!(
            plain_text(&out[1]).starts_with("\u{2502} "),
            "the code gutter must be repeated, got {:?}",
            plain_text(&out[1])
        );
    }

    #[test]
    fn wrapped_lines_count_towards_the_scroll_height() {
        let md = "a bb ccc dddd eeeee ffffff ggggggg hhhhhhhh iiiiiiiii jjjjjjjjjj\n";
        let path = std::path::PathBuf::from("/tmp/mdr_wrap_height.md");
        let mut elements = build_content_elements(md, &path, &None);
        let unwrapped = total_content_rows(&elements);
        rewrap_elements(&mut elements, 20);
        let wrapped = total_content_rows(&elements);
        assert!(
            wrapped > unwrapped,
            "wrapping must be reflected in the scroll height ({} -> {})",
            unwrapped,
            wrapped
        );
    }

    #[test]
    fn search_offsets_follow_the_wrapped_layout() {
        let md = "aaaa bbbb cccc dddd eeee ffff gggg hhhh\n\nneedle\n";
        let path = std::path::PathBuf::from("/tmp/mdr_wrap_search.md");
        let mut elements = build_content_elements(md, &path, &None);
        rewrap_elements(&mut elements, 12);

        let matches = compute_search_matches(&elements, "needle");
        assert_eq!(matches.len(), 1, "exactly one line holds the needle");

        // The offset must be the sum of the *wrapped* heights of everything
        // above it, otherwise jumping to a match scrolls to the wrong place.
        let mut expected = 0usize;
        for element in &elements {
            if let ContentElement::TextLine(text) = element {
                if text.text().contains("needle") {
                    break;
                }
            }
            expected += element.row_height() as usize;
        }
        assert_eq!(matches[0], expected);
        assert!(
            expected >= 4,
            "the wrapped paragraph should push the match down, got {}",
            expected
        );
    }

    // --- #58: the terminal capability query must not delay the first frame ----

    #[test]
    fn a_document_without_images_needs_no_picker() {
        let md =
            "# Title\n\nJust text with `code` and a [link](https://example.com).\n\n- a\n- b\n";
        assert!(!document_needs_picker(md));
    }

    #[test]
    fn a_document_with_a_local_image_needs_a_picker() {
        assert!(document_needs_picker("# T\n\n![logo](images/logo.png)\n"));
    }

    #[test]
    fn a_document_with_a_remote_image_needs_a_picker() {
        assert!(document_needs_picker(
            "![logo](https://example.com/logo.png)\n"
        ));
    }

    #[test]
    fn a_document_with_a_mermaid_diagram_needs_a_picker() {
        assert!(document_needs_picker(
            "```mermaid\ngraph LR\n  A-->B\n```\n"
        ));
    }

    #[test]
    fn an_image_inside_a_paragraph_needs_no_picker() {
        // Inline images are rendered as `[Image: alt]` text, never as pixels.
        assert!(!document_needs_picker("see ![logo](logo.png) in context\n"));
    }

    #[test]
    fn an_image_written_inside_a_code_block_needs_no_picker() {
        assert!(!document_needs_picker("```md\n![logo](logo.png)\n```\n"));
    }

    // --- #61: images living outside the document's own directory -------------

    /// Write a tiny valid SVG, the cheapest file `validate_image_file` accepts.
    fn write_svg(path: &std::path::Path) {
        std::fs::write(
            path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red"/></svg>"#,
        )
        .unwrap();
    }

    #[test]
    fn an_image_in_a_sibling_directory_of_the_project_is_loaded() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        std::fs::create_dir_all(proj.join("docs")).unwrap();
        std::fs::create_dir_all(proj.join("images")).unwrap();
        write_svg(&proj.join("images/schema.svg"));

        let img = load_image("../images/schema.svg", &proj.join("docs"));
        assert!(
            img.is_ok(),
            "an image from a parent directory inside the project must load, got: {:?}",
            img.err()
        );
    }

    #[test]
    fn an_image_outside_the_project_is_still_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        std::fs::create_dir_all(proj.join("docs")).unwrap();
        write_svg(&tmp.path().join("secret.svg"));

        let img = load_image("../../secret.svg", &proj.join("docs"));
        assert!(
            img.is_err(),
            "an image outside the enclosing project must stay refused"
        );
    }

    #[test]
    fn mermaid_build_content_elements_fallback_without_picker() {
        // Without a picker, mermaid should fall back to code block display
        let md = "```mermaid\ngraph LR\n  A-->B\n```\n";
        let md_path = std::path::PathBuf::from("/tmp/test_mermaid.md");
        let elements = build_content_elements(md, &md_path, &None);

        // Without picker, mermaid rendering should either produce TextLines (fallback)
        // or ImagePlaceholder - but NOT be empty
        assert!(
            !elements.is_empty(),
            "Should produce content elements for mermaid block"
        );

        // Check that we have some text lines (the fallback code display)
        let has_text = elements
            .iter()
            .any(|e| matches!(e, ContentElement::TextLine(_)));
        assert!(has_text, "Mermaid fallback should produce text lines");
    }
}

#[cfg(test)]
mod fidelity_tests {
    use super::*;

    /// The plain text of every rendered line, in order.
    fn rendered(md: &str) -> Vec<String> {
        markdown_to_lines_with_images(md)
            .into_iter()
            .filter_map(|item| match item {
                ParsedLine::Text(line) => {
                    Some(line.spans.iter().map(|s| s.content.as_ref()).collect())
                }
                _ => None,
            })
            .collect()
    }

    /// Every distinct foreground colour used across the rendered lines.
    fn colours(md: &str) -> std::collections::BTreeSet<String> {
        markdown_to_lines_with_images(md)
            .into_iter()
            .filter_map(|item| match item {
                ParsedLine::Text(line) => Some(line),
                _ => None,
            })
            .flat_map(|line| {
                line.spans
                    .iter()
                    .map(|s| format!("{:?}", s.style.fg))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    // --- code frame and syntax theme (0.5.1) ---

    /// The frame around a code block used to be left open on the right as soon
    /// as the fence named a language: `┌─ rust ─────────` with no `┐`.
    #[test]
    fn the_code_frame_is_closed_and_square_whatever_the_label() {
        for label in [
            "code",
            "rust",
            "mermaid",
            "",
            "a-very-long-language-name-indeed",
        ] {
            let top = code_frame_top(label);
            assert!(top.starts_with('┌'), "{:?}", top);
            assert!(
                top.ends_with('┐'),
                "top edge left open for {:?}: {:?}",
                label,
                top
            );
            assert_eq!(
                str_width(&top),
                str_width(CODE_FRAME_BOTTOM),
                "top and bottom edges must line up for {:?}: {:?}",
                label,
                top
            );
        }
    }

    #[test]
    fn a_named_language_still_appears_in_the_frame() {
        assert!(code_frame_top("rust").contains("rust"));
    }

    /// `COLORFGBG` is `fg;bg`, sometimes with a middle field. The background is
    /// the last one, as an ANSI palette index.
    #[test]
    fn the_terminal_background_is_read_from_colorfgbg() {
        assert_eq!(terminal_background_is_light(Some("15;0")), Some(false));
        assert_eq!(terminal_background_is_light(Some("0;15")), Some(true));
        assert_eq!(
            terminal_background_is_light(Some("15;default;0")),
            Some(false)
        );
        assert_eq!(
            terminal_background_is_light(Some("0;default;7")),
            Some(true)
        );
        // Nothing usable: no answer, so the caller keeps its default.
        assert_eq!(terminal_background_is_light(None), None);
        assert_eq!(terminal_background_is_light(Some("")), None);
        assert_eq!(terminal_background_is_light(Some("15;default")), None);
        assert_eq!(terminal_background_is_light(Some("0;99")), None);
    }

    #[test]
    fn an_explicit_theme_always_wins_over_the_terminal() {
        use crate::core::Theme;
        // A light terminal, overridden to dark, and the other way round.
        assert_eq!(
            syntax_theme_name(Theme::Dark, Some("0;15")),
            "base16-ocean.dark"
        );
        assert_eq!(
            syntax_theme_name(Theme::Light, Some("15;0")),
            "InspiredGitHub"
        );
    }

    #[test]
    fn auto_follows_the_terminal_and_falls_back_to_dark() {
        use crate::core::Theme;
        assert_eq!(
            syntax_theme_name(Theme::Auto, Some("0;15")),
            "InspiredGitHub"
        );
        assert_eq!(
            syntax_theme_name(Theme::Auto, Some("15;0")),
            "base16-ocean.dark"
        );
        // A terminal that says nothing must not cost a query, and dark is the
        // safe assumption for a pager.
        assert_eq!(syntax_theme_name(Theme::Auto, None), "base16-ocean.dark");
    }

    /// Both theme names must exist in syntect's defaults, or highlighting would
    /// silently fall back to an empty theme.
    #[test]
    fn both_themes_exist_in_syntect_defaults() {
        let themes = syntect::highlighting::ThemeSet::load_defaults();
        for name in ["base16-ocean.dark", "InspiredGitHub"] {
            assert!(
                themes.themes.contains_key(name),
                "syntect has no theme {:?}; available: {:?}",
                name,
                themes.themes.keys().collect::<Vec<_>>()
            );
        }
    }

    // --- regressions found while writing the AST renderer ---

    /// CommonMark "tight" vs "loose": a list written without blank lines
    /// between its items must not gain any, and one written with them must
    /// keep them. Both directions broke at different points of the rewrite.
    #[test]
    fn a_tight_list_does_not_breathe_and_a_loose_one_does() {
        let tight = rendered("- un\n- deux\n- trois\n");
        let blanks = tight.iter().filter(|l| l.trim().is_empty()).count();
        assert_eq!(
            blanks, 0,
            "a tight list must not gain blank lines: {:?}",
            tight
        );

        let loose = rendered("- un\n\n- deux\n\n- trois\n");
        let blanks = loose.iter().filter(|l| l.trim().is_empty()).count();
        assert!(
            blanks >= 2,
            "a loose list must keep its spacing: {:?}",
            loose
        );
    }

    /// A list nested inside a tight list must not add spacing of its own.
    #[test]
    fn a_nested_list_inside_a_tight_list_stays_tight() {
        let lines = rendered("- un\n- deux\n  - imbriqué\n- trois\n");
        assert!(
            !lines.iter().any(|l| l.trim().is_empty()),
            "no blank line belongs inside a tight list: {:?}",
            lines
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("  ") && l.contains("imbriqué")),
            "the nested item must keep its indent: {:?}",
            lines
        );
    }

    /// `find_heading_row` scrolls the TOC by looking for the entry's text
    /// inside a rendered line. If the renderer ever decorated headings in a way
    /// that broke that `contains`, TOC navigation would silently stop working.
    #[test]
    fn every_toc_entry_can_still_be_found_in_the_rendered_lines() {
        let md = "# Un\n\ntexte\n\n## Deux trois\n\ntexte\n\n##### Cinq\n\ntexte\n";
        let lines = rendered(md);
        for entry in crate::core::toc::extract_toc(md) {
            assert!(
                lines.iter().any(|l| l.contains(&entry.text)),
                "TOC entry {:?} has no rendered line containing it: {:?}",
                entry.text,
                lines
            );
        }
    }

    /// Inline markup must not reach the screen as raw syntax.
    #[test]
    fn inline_markup_is_styled_not_printed() {
        let lines = rendered("A **b** *c* ~~d~~ `e` [f](http://x) end\n");
        let joined = lines.join(" ");
        for raw in ["**", "~~", "`", "](", "http://x"] {
            assert!(
                !joined.contains(raw),
                "raw {:?} reached the screen: {:?}",
                raw,
                joined
            );
        }
        for word in ["b", "c", "d", "e", "f", "end"] {
            assert!(
                joined.contains(word),
                "{:?} was dropped: {:?}",
                word,
                joined
            );
        }
    }

    /// Column alignment markers are honoured, not just the width.
    #[test]
    fn table_alignment_markers_are_honoured() {
        let md = "| l | c | r |\n|:--|:-:|--:|\n| x | x | x |\n";
        let body = rendered(md)
            .into_iter()
            .find(|l| l.matches('x').count() == 3)
            .expect("body row");
        let cells: Vec<&str> = body.split('│').collect();
        assert_eq!(cells.len(), 3, "expected three cells: {:?}", body);
        assert!(cells[0].starts_with('x'), "left column: {:?}", cells[0]);
        assert!(
            cells[2].trim_start().ends_with('x'),
            "right column: {:?}",
            cells[2]
        );
    }

    // #59, symptom 1: headings deeper than #### are printed raw.
    #[test]
    fn h5_and_h6_are_rendered_as_headings_not_raw_text() {
        for (md, title) in [("##### Deep\n", "Deep"), ("###### Deeper\n", "Deeper")] {
            let lines = rendered(md);
            assert!(
                lines.iter().any(|l| l.trim() == title),
                "expected a line holding just {:?}, got {:?}",
                title,
                lines
            );
            assert!(
                !lines.iter().any(|l| l.contains('#')),
                "the hashes must not reach the screen, got {:?}",
                lines
            );
        }
    }

    // #59, symptom 2: code blocks have no syntax highlighting.
    #[test]
    fn a_code_block_is_syntax_highlighted() {
        let md = "```rust\nfn main() { let x: u32 = 1; }\n```\n";
        let used = colours(md);
        assert!(
            used.len() > 3,
            "a highlighted Rust block should use more than a couple of colours, got {:?}",
            used
        );
    }

    // #59, symptom 3: table cells are not aligned on column width.
    #[test]
    fn table_cells_are_padded_to_the_column_width() {
        let md = "| a | long header |\n|---|---|\n| 1 | 2 |\n";
        let lines: Vec<String> = rendered(md)
            .into_iter()
            .filter(|l| l.contains('1') || l.contains("long header"))
            .collect();
        assert!(
            lines.len() >= 2,
            "expected header and body rows, got {:?}",
            lines
        );
        // Deliberately not trimmed: the trailing padding *is* the alignment.
        let widths: std::collections::BTreeSet<usize> =
            lines.iter().map(|l| l.chars().count()).collect();
        assert_eq!(
            widths.len(),
            1,
            "every row of a table must be the same width once padded, got {:?}",
            lines
        );
    }

    // #59, symptom 4: footnotes are not rendered — the raw `[^1]` and `[^1]:`
    // markers reach the screen instead of being turned into a reference and a
    // note section.
    #[test]
    fn footnotes_are_rendered() {
        let md = "Some text[^1].\n\n[^1]: The note itself.\n";
        let lines = rendered(md);
        assert!(
            lines.iter().any(|l| l.contains("The note itself")),
            "the footnote body must appear, got {:?}",
            lines
        );
        assert!(
            !lines.iter().any(|l| l.contains("[^1]")),
            "the raw footnote syntax must not reach the screen, got {:?}",
            lines
        );
    }
}
