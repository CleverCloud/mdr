use std::io;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::core::toc::{self, TocEntry};

pub fn run(file_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(&file_path)?;
    let toc_entries = toc::extract_toc(&content);
    let rendered = markdown_to_lines(&content);

    let watcher_rx = crate::core::watcher::watch_file(&file_path)?;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp {
        content,
        rendered,
        toc_entries,
        file_path,
        watcher_rx,
        scroll_offset: 0,
        toc_selected: 0,
        focus_toc: false,
        should_quit: false,
    };

    // Main loop
    loop {
        terminal.draw(|f| ui(f, &app))?;

        // Check for file changes
        if app.watcher_rx.try_recv().is_ok() {
            while app.watcher_rx.try_recv().is_ok() {}
            if let Ok(new_content) = std::fs::read_to_string(&app.file_path) {
                app.toc_entries = toc::extract_toc(&new_content);
                app.rendered = markdown_to_lines(&new_content);
                app.content = new_content;
            }
        }

        // Poll events with 100ms timeout for file watching
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.should_quit = true;
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
                        app.scroll_offset = app.rendered.len().saturating_sub(1);
                    }
                    KeyCode::Tab => {
                        app.focus_toc = !app.focus_toc;
                    }
                    KeyCode::Enter => {
                        if app.focus_toc {
                            // Navigate to heading
                            if let Some(offset) = find_heading_line(&app.rendered, &app.toc_entries, app.toc_selected) {
                                app.scroll_offset = offset;
                                app.focus_toc = false;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

struct TuiApp {
    content: String,
    rendered: Vec<Line<'static>>,
    toc_entries: Vec<TocEntry>,
    file_path: PathBuf,
    watcher_rx: Receiver<()>,
    scroll_offset: usize,
    toc_selected: usize,
    focus_toc: bool,
    should_quit: bool,
}

fn ui(f: &mut Frame, app: &TuiApp) {
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
        .highlight_symbol("▶ ");

    let mut toc_state = ListState::default();
    if app.focus_toc {
        toc_state.select(Some(app.toc_selected));
    }
    f.render_stateful_widget(toc, chunks[0], &mut toc_state);

    // Main content
    let content_height = chunks[1].height.saturating_sub(2) as usize; // minus borders
    let max_scroll = app.rendered.len().saturating_sub(content_height);
    let scroll = app.scroll_offset.min(max_scroll);

    let visible_lines: Vec<Line> = app.rendered.iter()
        .skip(scroll)
        .take(content_height)
        .cloned()
        .collect();

    let content_border_style = if !app.focus_toc {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let scroll_info = format!(" {}/{} ", scroll + 1, app.rendered.len().max(1));
    let paragraph = Paragraph::new(visible_lines)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(content_border_style)
            .title(format!(" {} ", app.file_path.display()))
            .title_style(Style::default().bold())
            .title_bottom(Line::from(scroll_info).right_aligned()));

    f.render_widget(paragraph, chunks[1]);

    // Help bar at bottom - use an overlay
    let help = " q: quit | Tab: switch focus | j/k: scroll | Enter: navigate | Space/PgDn: page down ";
    let help_area = Rect {
        x: chunks[1].x + 1,
        y: chunks[1].y + chunks[1].height - 1,
        width: chunks[1].width.saturating_sub(2).min(help.len() as u16),
        height: 1,
    };
    let help_widget = Paragraph::new(help)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(help_widget, help_area);
}

/// Find the line index where a heading appears in the rendered output.
fn find_heading_line(lines: &[Line], toc_entries: &[TocEntry], toc_index: usize) -> Option<usize> {
    let entry = toc_entries.get(toc_index)?;
    let search_text = &entry.text;

    lines.iter().position(|line| {
        let line_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        line_text.contains(search_text)
    })
}

/// Convert markdown content to styled ratatui Lines.
fn markdown_to_lines(content: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut in_table = false;

    for line in content.lines() {
        if line.starts_with("```") {
            if in_code_block {
                in_code_block = false;
                lines.push(Line::from(Span::styled(
                    "└─────────────────────────────────────────┘",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
            } else {
                in_code_block = true;
                code_lang = line.trim_start_matches('`').to_string();
                let header = if code_lang.is_empty() {
                    "┌─ code ──────────────────────────────────┐".to_string()
                } else {
                    format!("┌─ {} {}", code_lang, "─".repeat(38usize.saturating_sub(code_lang.len())))
                };
                lines.push(Line::from(Span::styled(
                    header,
                    Style::default().fg(Color::DarkGray),
                )));
            }
            continue;
        }

        if in_code_block {
            lines.push(Line::from(Span::styled(
                format!("│ {}", line),
                Style::default().fg(Color::Green),
            )));
            continue;
        }

        // Headings
        if line.starts_with("# ") {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                line[2..].to_string(),
                Style::default().fg(Color::Cyan).bold().underlined(),
            )));
            lines.push(Line::from(Span::styled(
                "═".repeat(line.len().saturating_sub(2).min(60)),
                Style::default().fg(Color::Cyan),
            )));
            lines.push(Line::from(""));
            continue;
        }
        if line.starts_with("## ") {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                line[3..].to_string(),
                Style::default().fg(Color::Blue).bold(),
            )));
            lines.push(Line::from(Span::styled(
                "─".repeat(line.len().saturating_sub(3).min(50)),
                Style::default().fg(Color::Blue),
            )));
            lines.push(Line::from(""));
            continue;
        }
        if line.starts_with("### ") {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                line[4..].to_string(),
                Style::default().fg(Color::Yellow).bold(),
            )));
            lines.push(Line::from(""));
            continue;
        }
        if line.starts_with("#### ") {
            lines.push(Line::from(Span::styled(
                line[5..].to_string(),
                Style::default().fg(Color::Magenta).bold(),
            )));
            continue;
        }

        // Horizontal rule
        if line.starts_with("---") || line.starts_with("***") || line.starts_with("___") {
            lines.push(Line::from(Span::styled(
                "─".repeat(60),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        // Table rows
        if line.contains('|') && line.trim().starts_with('|') {
            if line.contains("---") && !in_table {
                in_table = true;
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::DarkGray),
                )));
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
            lines.push(Line::from(spans));
            continue;
        } else {
            in_table = false;
        }

        // Blockquote
        if line.starts_with("> ") {
            lines.push(Line::from(vec![
                Span::styled("▎ ", Style::default().fg(Color::DarkGray)),
                Span::styled(line[2..].to_string(), Style::default().fg(Color::Gray).italic()),
            ]));
            continue;
        }

        // Task list
        if line.trim_start().starts_with("- [x] ") {
            let indent = line.len() - line.trim_start().len();
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled("☑ ", Style::default().fg(Color::Green)),
                Span::styled(
                    line.trim_start()[6..].to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            continue;
        }
        if line.trim_start().starts_with("- [ ] ") {
            let indent = line.len() - line.trim_start().len();
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled("☐ ", Style::default().fg(Color::Yellow)),
                Span::styled(line.trim_start()[6..].to_string(), Style::default()),
            ]));
            continue;
        }

        // Unordered list
        if line.trim_start().starts_with("- ") || line.trim_start().starts_with("* ") {
            let indent = line.len() - line.trim_start().len();
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled("• ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    line.trim_start()[2..].to_string(),
                    Style::default(),
                ),
            ]));
            continue;
        }

        // Ordered list
        if let Some(rest) = try_parse_ordered_list(line) {
            let indent = line.len() - line.trim_start().len();
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(rest.0.clone(), Style::default().fg(Color::Cyan)),
                Span::styled(rest.1.clone(), Style::default()),
            ]));
            continue;
        }

        // Image: ![alt](url)
        if line.trim_start().starts_with("![") {
            if let Some(alt) = extract_image_alt(line) {
                let label = if alt.is_empty() { "image".to_string() } else { alt };
                lines.push(Line::from(Span::styled(
                    format!("[Image: {}]", label),
                    Style::default().fg(Color::Magenta).italic(),
                )));
                continue;
            }
        }

        // Regular text with inline formatting
        lines.push(parse_inline_formatting(line));
    }

    lines
}

/// Extract alt text from a markdown image line: ![alt](url)
fn extract_image_alt(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let start = trimmed.find("![")?;
    let rest = &trimmed[start + 2..];
    let end = rest.find("](")?;
    Some(rest[..end].to_string())
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
