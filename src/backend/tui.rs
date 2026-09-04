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
            "┌─ mermaid ─────────────────────────────────┐".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
    ))));
    for line in source.lines() {
        elements.push(ContentElement::TextLine(WrappedText::new(Line::from(
            Span::styled(format!("│ {}", line), Style::default().fg(Color::Green)),
        ))));
    }
    elements.push(ContentElement::TextLine(WrappedText::new(Line::from(
        Span::styled(
            "└─────────────────────────────────────────┘".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
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
                    items.push(ParsedLine::MermaidRef {
                        source: mermaid_source.clone(),
                    });
                    mermaid_source.clear();
                } else {
                    in_code_block = false;
                    items.push(ParsedLine::Text(Line::from(Span::styled(
                        "└─────────────────────────────────────────┘",
                        Style::default().fg(Color::DarkGray),
                    ))));
                    items.push(ParsedLine::Text(Line::from("")));
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
                        format!(
                            "┌─ {} {}",
                            code_lang,
                            "─".repeat(38usize.saturating_sub(code_lang.len()))
                        )
                    };
                    items.push(ParsedLine::Text(Line::from(Span::styled(
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
                items.push(ParsedLine::Text(Line::from(Span::styled(
                    format!("│ {}", line),
                    Style::default().fg(Color::Green),
                ))));
            }
            continue;
        }

        // Headings
        if let Some(title) = line.strip_prefix("# ") {
            items.push(ParsedLine::Text(Line::from("")));
            items.push(ParsedLine::Text(Line::from(Span::styled(
                title.to_string(),
                Style::default().fg(Color::Cyan).bold().underlined(),
            ))));
            items.push(ParsedLine::Text(Line::from(Span::styled(
                "═".repeat(title.len().min(60)),
                Style::default().fg(Color::Cyan),
            ))));
            items.push(ParsedLine::Text(Line::from("")));
            continue;
        }
        if let Some(title) = line.strip_prefix("## ") {
            items.push(ParsedLine::Text(Line::from("")));
            items.push(ParsedLine::Text(Line::from(Span::styled(
                title.to_string(),
                Style::default().fg(Color::Blue).bold(),
            ))));
            items.push(ParsedLine::Text(Line::from(Span::styled(
                "─".repeat(title.len().min(50)),
                Style::default().fg(Color::Blue),
            ))));
            items.push(ParsedLine::Text(Line::from("")));
            continue;
        }
        if let Some(title) = line.strip_prefix("### ") {
            items.push(ParsedLine::Text(Line::from("")));
            items.push(ParsedLine::Text(Line::from(Span::styled(
                title.to_string(),
                Style::default().fg(Color::Yellow).bold(),
            ))));
            items.push(ParsedLine::Text(Line::from("")));
            continue;
        }
        if let Some(title) = line.strip_prefix("#### ") {
            items.push(ParsedLine::Text(Line::from(Span::styled(
                title.to_string(),
                Style::default().fg(Color::Magenta).bold(),
            ))));
            continue;
        }

        // Horizontal rule
        if line.starts_with("---") || line.starts_with("***") || line.starts_with("___") {
            items.push(ParsedLine::Text(Line::from(Span::styled(
                "─".repeat(60),
                Style::default().fg(Color::DarkGray),
            ))));
            continue;
        }

        // Table rows
        if line.contains('|') && line.trim().starts_with('|') {
            if line.contains("---") && !in_table {
                in_table = true;
                items.push(ParsedLine::Text(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::DarkGray),
                ))));
                continue;
            }
            in_table = true;
            let cells: Vec<&str> = line
                .split('|')
                .filter(|s| !s.is_empty())
                .map(|s| s.trim())
                .collect();
            let spans: Vec<Span> = cells
                .iter()
                .enumerate()
                .flat_map(|(i, cell)| {
                    let mut v = vec![];
                    if i > 0 {
                        v.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
                    }
                    v.push(Span::styled(
                        cell.to_string(),
                        Style::default().fg(Color::White),
                    ));
                    v
                })
                .collect();
            items.push(ParsedLine::Text(Line::from(spans)));
            continue;
        } else {
            in_table = false;
        }

        // Blockquote
        if let Some(quoted) = line.strip_prefix("> ") {
            items.push(ParsedLine::Text(Line::from(vec![
                Span::styled("▎ ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    quoted.to_string(),
                    Style::default().fg(Color::Gray).italic(),
                ),
            ])));
            continue;
        }

        // Task list
        if line.trim_start().starts_with("- [x] ") {
            let indent = line.len() - line.trim_start().len();
            items.push(ParsedLine::Text(Line::from(vec![
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
            items.push(ParsedLine::Text(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled("☐ ", Style::default().fg(Color::Yellow)),
                Span::styled(line.trim_start()[6..].to_string(), Style::default()),
            ])));
            continue;
        }

        // Unordered list
        if line.trim_start().starts_with("- ") || line.trim_start().starts_with("* ") {
            let indent = line.len() - line.trim_start().len();
            items.push(ParsedLine::Text(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled("• ", Style::default().fg(Color::Cyan)),
                Span::styled(line.trim_start()[2..].to_string(), Style::default()),
            ])));
            continue;
        }

        // Ordered list
        if let Some(rest) = try_parse_ordered_list(line) {
            let indent = line.len() - line.trim_start().len();
            items.push(ParsedLine::Text(Line::from(vec![
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

        // Regular text with inline formatting
        items.push(ParsedLine::Text(parse_inline_formatting(line)));
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
                    if c == '`' {
                        break;
                    }
                    code.push(c);
                }
                spans.push(Span::styled(
                    code,
                    Style::default().fg(Color::Green).bg(Color::Rgb(30, 30, 30)),
                ));
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
                    if ch == c {
                        break;
                    }
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
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::CROSSED_OUT),
                ));
            }
            '!' if chars.peek() == Some(&'[') => {
                // Image: ![alt](url)
                chars.next(); // consume '['
                let mut alt = String::new();
                let mut found_close = false;
                for ch in chars.by_ref() {
                    if ch == ']' {
                        found_close = true;
                        break;
                    }
                    alt.push(ch);
                }
                if found_close && chars.peek() == Some(&'(') {
                    chars.next();
                    let mut _url = String::new();
                    for ch in chars.by_ref() {
                        if ch == ')' {
                            break;
                        }
                        _url.push(ch);
                    }
                    if !current.is_empty() {
                        spans.push(Span::raw(current.clone()));
                        current.clear();
                    }
                    let label = if alt.is_empty() {
                        "image".to_string()
                    } else {
                        alt
                    };
                    spans.push(Span::styled(
                        format!("[Image: {}]", label),
                        Style::default().fg(Color::Magenta).italic(),
                    ));
                } else {
                    current.push('!');
                    current.push('[');
                    current.push_str(&alt);
                    if found_close {
                        current.push(']');
                    }
                }
            }
            '[' => {
                // Link: [text](url)
                let mut text = String::new();
                let mut found_close = false;
                for ch in chars.by_ref() {
                    if ch == ']' {
                        found_close = true;
                        break;
                    }
                    text.push(ch);
                }
                if found_close && chars.peek() == Some(&'(') {
                    chars.next();
                    let mut _url = String::new();
                    for ch in chars.by_ref() {
                        if ch == ')' {
                            break;
                        }
                        _url.push(ch);
                    }
                    if !current.is_empty() {
                        spans.push(Span::raw(current.clone()));
                        current.clear();
                    }
                    spans.push(Span::styled(
                        text,
                        Style::default().fg(Color::Blue).underlined(),
                    ));
                } else {
                    current.push('[');
                    current.push_str(&text);
                    if found_close {
                        current.push(']');
                    }
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
