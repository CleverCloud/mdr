use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::core::mermaid::preprocess_mermaid_for_egui;
use crate::core::toc::{self, TocEntry};

/// The platform's UI font, best first — the same intent as the `system-ui`
/// stack the `web` backend asks CSS for.
const UI_FONT_FAMILIES: &[&str] = &[
    "SF Pro Text",
    "SF Pro Display",
    ".AppleSystemUIFont",
    "Helvetica Neue",
    "Segoe UI Variable Text",
    "Segoe UI",
    "Cantarell",
    "Ubuntu",
    "Noto Sans",
    "DejaVu Sans",
];

/// Monospace equivalents, matching the `ui-monospace` stack in the stylesheet.
const MONO_FONT_FAMILIES: &[&str] = &[
    "SF Mono",
    "SFMono-Regular",
    "Menlo",
    "Cascadia Mono",
    "Consolas",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
];

/// Where `name` sits in `preferences`, if at all. Lower is better.
fn preference(preferences: &[&str], name: &str) -> Option<usize> {
    preferences
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(name))
}

/// Load system fonts into egui: the platform's UI and monospace faces first, so
/// the backend does not render in egui's bundled typeface while `web` renders in
/// the system one; every other installed face stays behind them as a fallback
/// for non-Latin scripts (CJK, etc.).
fn load_system_fonts(ctx: &egui::Context) {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();

    let mut fonts = egui::FontDefinitions::default();
    let mut preferred_ui: Option<(usize, String)> = None;
    let mut preferred_mono: Option<(usize, String)> = None;

    let mut counter = 0usize;
    for face in db.faces() {
        let source = match &face.source {
            fontdb::Source::Binary(_) => continue,
            fontdb::Source::File(path) => path,
            fontdb::Source::SharedFile(path, _) => path,
        };

        let name = face
            .families
            .first()
            .map_or_else(|| format!("font_{counter}"), |(name, _)| name.clone());

        if let Ok(data) = std::fs::read(source) {
            // The key has to identify the FACE, not the family: two files of the
            // same family — a regular and a bold — both report index 0, so
            // keying on family and index made the second overwrite the first,
            // and the face finally drawn was whichever happened to load last.
            // `counter` is per-face and therefore unique.
            let key = format!("{name}#{counter}");
            // `from_owned` always sets index 0. Inside a collection (.ttc) that
            // is a different face from the one selected above, so the real index
            // has to be put back.
            let mut font_data = egui::FontData::from_owned(data);
            font_data.index = face.index;
            fonts.font_data.insert(key.clone(), font_data.into());

            // Insert into proportional and monospace fallbacks
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .push(key.clone());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push(key.clone());

            // Only upright regular faces are candidates for the primary font;
            // picking an italic or a bold as the body face looks like a bug.
            // Weight and style alone do not separate a condensed or expanded
            // face from the plain one, and either would be a surprising body
            // font.
            let upright_regular = face.weight == fontdb::Weight::NORMAL
                && face.style == fontdb::Style::Normal
                && face.stretch == fontdb::Stretch::Normal;
            if upright_regular {
                if let Some(rank) = preference(UI_FONT_FAMILIES, &name)
                    && preferred_ui.as_ref().is_none_or(|(best, _)| rank < *best)
                {
                    preferred_ui = Some((rank, key.clone()));
                }
                if let Some(rank) = preference(MONO_FONT_FAMILIES, &name)
                    && preferred_mono.as_ref().is_none_or(|(best, _)| rank < *best)
                {
                    preferred_mono = Some((rank, key));
                }
            }
        }
        counter += 1;
    }

    // Move the chosen faces to the front of their family, ahead of egui's own.
    for (family, chosen) in [
        (egui::FontFamily::Proportional, preferred_ui),
        (egui::FontFamily::Monospace, preferred_mono),
    ] {
        if let Some((_, key)) = chosen
            && let Some(list) = fonts.families.get_mut(&family)
        {
            list.retain(|existing| existing != &key);
            list.insert(0, key);
        }
    }

    ctx.set_fonts(fonts);
}

/// Apply the shared palette and type scale.
///
/// Without this the backend runs on egui's defaults: a 13 pt body with an 18 pt
/// heading, so `h1` through `h6` all land within five points of each other,
/// while `web` renders the same document on a 16 px body and a 2 em `h1`.
fn apply_style(ctx: &egui::Context) {
    use crate::core::style::{self, BASE_FONT_SIZE, CODE_FONT_SCALE};
    use egui::{FontFamily, FontId, TextStyle};

    let colour = |c: style::Rgb| egui::Color32::from_rgb(c[0], c[1], c[2]);

    // Sizes are the same whichever palette is in use.
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (
                TextStyle::Small,
                FontId::new(BASE_FONT_SIZE * 0.875, FontFamily::Proportional),
            ),
            (
                TextStyle::Body,
                FontId::new(BASE_FONT_SIZE, FontFamily::Proportional),
            ),
            (
                TextStyle::Button,
                FontId::new(BASE_FONT_SIZE, FontFamily::Proportional),
            ),
            (
                TextStyle::Heading,
                FontId::new(style::heading_size(1), FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(BASE_FONT_SIZE * CODE_FONT_SCALE, FontFamily::Monospace),
            ),
        ]
        .into();
    });

    // Both palettes are installed, so following the OS costs nothing at runtime.
    for (theme, palette) in [
        (egui::Theme::Dark, &style::DARK),
        (egui::Theme::Light, &style::LIGHT),
    ] {
        ctx.style_mut_of(theme, |s| {
            let v = &mut s.visuals;
            v.panel_fill = colour(palette.bg);
            v.window_fill = colour(palette.bg);
            v.extreme_bg_color = colour(palette.code_bg);
            v.code_bg_color = colour(palette.inline_code_bg);
            v.hyperlink_color = colour(palette.link);
            v.widgets.noninteractive.fg_stroke.color = colour(palette.fg);
            v.widgets.inactive.fg_stroke.color = colour(palette.fg);
            // `RichText::strong()` resolves to this one, and egui has no font
            // weights — brightening the colour is the only bold it can draw.
            v.widgets.hovered.fg_stroke.color = colour(palette.strong);
            v.widgets.active.fg_stroke.color = colour(palette.strong);
            v.widgets.noninteractive.bg_stroke.color = colour(palette.border);
            // egui_commonmark draws blockquotes with `weak_text_color`, which
            // otherwise stays egui's own grey rather than the shared muted.
            v.weak_text_color = Some(colour(palette.muted));
        });
    }
}

pub fn run(file_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_file = std::fs::canonicalize(&file_path).unwrap_or_else(|_| {
        std::env::current_dir().map_or_else(|_| file_path.clone(), |cwd| cwd.join(&file_path))
    });
    let base_dir = canonical_file.parent().map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        std::path::Path::to_path_buf,
    );
    let raw_markdown = std::fs::read_to_string(&file_path)
        .unwrap_or_else(|e| format!("# Error\nCould not read `{}`: {}", file_path.display(), e));

    let markdown = preprocess_mermaid_for_egui(&raw_markdown);
    let markdown = resolve_local_image_paths(&markdown, &base_dir);
    // The TOC and the sections are both derived from the *rendered* markdown,
    // so a preprocessing step can never shift one against the other (#57).
    let toc_entries = toc::extract_toc(&markdown);
    let (has_preamble, sections) = split_by_headings(&markdown);

    let watcher_rx = crate::core::watcher::watch_file(&file_path)?;

    let (icon_rgba, icon_w, icon_h) = crate::core::icon::load_icon_rgba();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 900.0])
            .with_title(format!("mdr - {}", file_path.display()))
            .with_icon(egui::IconData {
                rgba: icon_rgba,
                width: icon_w,
                height: icon_h,
            }),
        ..Default::default()
    };

    eframe::run_native(
        "mdr",
        options,
        Box::new(move |cc| {
            load_system_fonts(&cc.egui_ctx);
            apply_style(&cc.egui_ctx);
            Ok(Box::new(MdrApp {
                markdown,
                sections,
                has_preamble,
                caches: Vec::new(),
                file_path,
                base_dir,
                watcher_rx,
                toc_entries,
                scroll_to_section: None,
                search_active: false,
                search_query: String::new(),
                search_section_matches: Vec::new(),
                current_match: 0,
                toc_visible: true,
                focus_search: false,
            }))
        }),
    )
    .map_err(|e| e.to_string().into())
}

/// The 1-based line numbers each heading of `markdown` starts on.
///
/// Parsed with comrak, using exactly the same options as
/// [`crate::core::toc::extract_toc`], so the two lists can never disagree
/// about what a heading is (#57): setext headings (`Title` + `-----`) count,
/// a `---` opening a YAML front matter block does not, and `#` inside a code
/// fence does not either.
fn heading_start_lines(markdown: &str) -> Vec<usize> {
    use comrak::nodes::NodeValue;
    use comrak::{Arena, Options, parse_document};

    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.front_matter_delimiter = Some("---".to_string());

    let root = parse_document(&arena, markdown, &options);
    let mut lines = Vec::new();
    for node in root.descendants() {
        let data = node.data.borrow();
        if matches!(data.value, NodeValue::Heading(_)) {
            lines.push(data.sourcepos.start.line);
        }
    }
    lines.sort_unstable();
    lines
}

/// Split markdown into sections at heading boundaries.
/// Returns (`has_preamble`, sections) where `has_preamble` is true if there's
/// content before the first heading (which means headings start at index 1).
///
/// The boundaries come from [`heading_start_lines`], i.e. from the same
/// comrak parse the table of contents is built from, so section `i + 1`
/// (or `i` without a preamble) always belongs to TOC entry `i`.
fn split_by_headings(markdown: &str) -> (bool, Vec<String>) {
    let starts = heading_start_lines(markdown);
    let mut next_start = starts.iter().copied().peekable();

    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();

    for (index, line) in markdown.lines().enumerate() {
        let lineno = index + 1;
        if next_start.peek() == Some(&lineno) {
            next_start.next();
            if !current.is_empty() {
                sections.push(std::mem::take(&mut current));
            }
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.is_empty() {
        sections.push(current);
    }

    // Anything pushed beyond one section per heading is the preamble.
    let has_preamble = sections.len() > starts.len();

    (has_preamble, sections)
}

/// What a key press means, decided independently of any egui context so it can
/// be unit-tested (#63).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Quit,
    ToggleToc,
    OpenSearch,
    CloseSearch,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    GoTop,
    GoBottom,
}

/// How far one arrow / `j` / `k` press scrolls, in points.
const SCROLL_STEP: f32 = 64.0;
/// How far one `PageUp` / `PageDown` / space press scrolls, in points.
const PAGE_STEP: f32 = 600.0;
/// Larger than any realistic document, and clamped by the scroll area, so it
/// lands exactly on the bottom.
const SCROLL_TO_END: f32 = 1.0e9;

/// Map a key press to an [`Action`].
///
/// `search_open` is true while the search bar is showing and taking keyboard
/// input: bare keys must then stay typable, so only modifier shortcuts and
/// function keys fire.
///
/// Modifier note: egui's `Modifiers::command` is documented as "⌘ Command on
/// Mac, Ctrl elsewhere" (`egui-0.34/src/data/input.rs`), so testing `command`
/// — rather than `ctrl`, which never gets set by ⌘ — is what makes Cmd+F work
/// on macOS while keeping Ctrl+F on Linux and Windows.
fn key_action(key: egui::Key, modifiers: egui::Modifiers, search_open: bool) -> Option<Action> {
    use egui::Key;

    if modifiers.command {
        return match key {
            Key::Q | Key::W => Some(Action::Quit),
            Key::F => Some(if search_open {
                Action::CloseSearch
            } else {
                Action::OpenSearch
            }),
            _ => None,
        };
    }

    // A bare ⌃ on macOS (or Alt anywhere) is not one of our bindings.
    if modifiers.alt || modifiers.ctrl || modifiers.mac_cmd {
        return None;
    }

    // Escape dismisses the search first; it only closes the window once there
    // is nothing left to dismiss.
    if key == Key::Escape {
        return Some(if search_open {
            Action::CloseSearch
        } else {
            Action::Quit
        });
    }

    // F10 is not typable, so it keeps working while the search field has focus.
    if key == Key::F10 {
        return Some(Action::ToggleToc);
    }

    // Everything below is a bare key: it must not steal input from the search
    // field, where it is either a character or a cursor movement.
    if search_open {
        return None;
    }

    if modifiers.shift {
        return (key == Key::G).then_some(Action::GoBottom);
    }

    // Same bindings as the tui and webview backends (see `SHORTCUTS` in
    // `webview.rs`).
    match key {
        Key::Q => Some(Action::Quit),
        Key::ArrowDown | Key::J => Some(Action::ScrollDown),
        Key::ArrowUp | Key::K => Some(Action::ScrollUp),
        Key::PageDown | Key::Space => Some(Action::PageDown),
        Key::PageUp => Some(Action::PageUp),
        Key::Home | Key::G => Some(Action::GoTop),
        Key::End => Some(Action::GoBottom),
        _ => None,
    }
}

/// The key presses of this frame, already translated to actions.
fn frame_actions(ctx: &egui::Context, search_open: bool) -> Vec<Action> {
    ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => key_action(*key, *modifiers, search_open),
                _ => None,
            })
            .collect()
    })
}

struct MdrApp {
    markdown: String,
    sections: Vec<String>,
    has_preamble: bool,
    caches: Vec<CommonMarkCache>,
    file_path: PathBuf,
    base_dir: PathBuf,
    watcher_rx: Receiver<()>,
    toc_entries: Vec<TocEntry>,
    scroll_to_section: Option<usize>,
    search_active: bool,
    search_query: String,
    search_section_matches: Vec<usize>,
    current_match: usize,
    toc_visible: bool,
    /// Set when Cmd/Ctrl+F opens the search, so the field takes focus on the
    /// next frame it is shown.
    focus_search: bool,
}

impl eframe::App for MdrApp {
    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root_ui.ctx().clone();

        // Ensure text in labels is selectable and copyable (Cmd+C / Ctrl+C)
        ctx.global_style_mut(|s| s.interaction.selectable_labels = true);

        // Check for file changes
        if self.watcher_rx.try_recv().is_ok() {
            while self.watcher_rx.try_recv().is_ok() {}
            if let Ok(content) = std::fs::read_to_string(&self.file_path) {
                self.markdown = preprocess_mermaid_for_egui(&content);
                self.markdown = resolve_local_image_paths(&self.markdown, &self.base_dir);
                self.toc_entries = toc::extract_toc(&self.markdown);
                let (has_preamble, sections) = split_by_headings(&self.markdown);
                self.has_preamble = has_preamble;
                self.sections = sections;
                self.caches.clear();
            }
        }

        // Ensure we have enough caches
        while self.caches.len() < self.sections.len() {
            self.caches.push(CommonMarkCache::default());
        }

        // Keyboard handling (#63). Bare keys must stay typable, so they are
        // suppressed while the search field is taking input.
        let search_open = self.search_active || ctx.text_edit_focused();
        let mut scroll_delta = 0.0_f32;
        let mut scroll_to_offset: Option<f32> = None;
        for action in frame_actions(&ctx, search_open) {
            match action {
                Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Action::ToggleToc => self.toc_visible = !self.toc_visible,
                Action::OpenSearch => {
                    self.search_active = true;
                    self.focus_search = true;
                }
                Action::CloseSearch => {
                    self.search_active = false;
                    self.focus_search = false;
                    self.search_query.clear();
                    self.search_section_matches.clear();
                }
                Action::ScrollDown => scroll_delta -= SCROLL_STEP,
                Action::ScrollUp => scroll_delta += SCROLL_STEP,
                Action::PageDown => scroll_delta -= PAGE_STEP,
                Action::PageUp => scroll_delta += PAGE_STEP,
                Action::GoTop => scroll_to_offset = Some(0.0),
                Action::GoBottom => scroll_to_offset = Some(SCROLL_TO_END),
            }
        }

        // Search bar panel
        if self.search_active {
            egui::Panel::top("search_bar").show(root_ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    let response = ui.text_edit_singleline(&mut self.search_query);
                    if response.changed() {
                        // Update matches
                        self.search_section_matches.clear();
                        self.current_match = 0;
                        if !self.search_query.is_empty() {
                            let query_lower = self.search_query.to_lowercase();
                            for (i, section) in self.sections.iter().enumerate() {
                                if section.to_lowercase().contains(&query_lower) {
                                    self.search_section_matches.push(i);
                                }
                            }
                            if !self.search_section_matches.is_empty() {
                                self.scroll_to_section = Some(self.search_section_matches[0]);
                            }
                        }
                    }
                    // Take focus on the frame the search was opened on.
                    if self.focus_search {
                        self.focus_search = false;
                        response.request_focus();
                    }

                    let match_text = if self.search_section_matches.is_empty() {
                        if self.search_query.is_empty() {
                            String::new()
                        } else {
                            "No matches".to_string()
                        }
                    } else {
                        format!(
                            "{}/{}",
                            self.current_match + 1,
                            self.search_section_matches.len()
                        )
                    };
                    ui.label(&match_text);

                    if (ui.button("\u{25B2}").clicked()
                        || (ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.shift)
                            && self.search_active))
                        && !self.search_section_matches.is_empty()
                    {
                        self.current_match = if self.current_match == 0 {
                            self.search_section_matches.len() - 1
                        } else {
                            self.current_match - 1
                        };
                        self.scroll_to_section =
                            Some(self.search_section_matches[self.current_match]);
                    }
                    if (ui.button("\u{25BC}").clicked()
                        || (ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
                            && self.search_active))
                        && !self.search_section_matches.is_empty()
                    {
                        self.current_match =
                            (self.current_match + 1) % self.search_section_matches.len();
                        self.scroll_to_section =
                            Some(self.search_section_matches[self.current_match]);
                    }
                    if ui
                        .button(if self.toc_visible {
                            "Hide TOC"
                        } else {
                            "Show TOC"
                        })
                        .clicked()
                    {
                        self.toc_visible = !self.toc_visible;
                    }
                    if ui.button("\u{2715}").clicked() {
                        self.search_active = false;
                        self.search_query.clear();
                        self.search_section_matches.clear();
                    }
                });
            });
        }

        // TOC sidebar
        let has_preamble = self.has_preamble;
        let scroll_target = &mut self.scroll_to_section;

        if self.toc_visible {
            egui::Panel::left("toc_panel")
                .default_size(220.0)
                .resizable(true)
                .show(root_ui, |ui| {
                    use crate::core::style::{self, BASE_FONT_SIZE};

                    // Follow the theme in use: pinning this to the dark palette
                    // put #8b949e on white, a 3.08:1 contrast at this size.
                    let palette = if ui.visuals().dark_mode {
                        &style::DARK
                    } else {
                        &style::LIGHT
                    };
                    let muted = egui::Color32::from_rgb(
                        palette.muted[0],
                        palette.muted[1],
                        palette.muted[2],
                    );

                    // A small uppercase label, like the `web` sidebar — not a
                    // document heading. `ui.heading` resolves to the h1 size and
                    // would take two lines of the panel to say "Contents".
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("TABLE OF CONTENTS")
                            .size(BASE_FONT_SIZE * 0.75)
                            .color(muted),
                    );
                    ui.separator();

                    // `auto_shrink` off: the default lets the area hug its widest
                    // entry, which puts egui's floating scrollbar on top of the
                    // text instead of at the panel's edge, and drags the panel
                    // wider than the size the reader chose.
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            // Entries wider than the panel are ellipsised rather
                            // than forcing it wider than the size the reader
                            // dragged it to.
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                            for (i, entry) in self.toc_entries.iter().enumerate() {
                                let indent = ((f32::from(entry.level) - 1.0) * 12.0).max(0.0);
                                ui.horizontal(|ui| {
                                    ui.add_space(indent);
                                    // Graded by depth, on the same scale as the
                                    // body rather than on hard-coded points.
                                    let text = egui::RichText::new(&entry.text);
                                    let text = match entry.level {
                                        1 => text.strong(),
                                        2 => text.size(BASE_FONT_SIZE * 0.875).strong(),
                                        3 => text.size(BASE_FONT_SIZE * 0.875),
                                        _ => text.size(BASE_FONT_SIZE * 0.8125).color(muted),
                                    };
                                    if ui.link(text).clicked() {
                                        // Map TOC index to section index
                                        let section_idx = if has_preamble { i + 1 } else { i };
                                        *scroll_target = Some(section_idx);
                                    }
                                });
                            }
                        });
                });
        }

        // Main content - render each section with scroll anchors
        let scroll_to = self.scroll_to_section.take();

        egui::CentralPanel::default().show(root_ui, |ui| {
            let mut area = egui::ScrollArea::vertical();
            // Home / End jump straight to an offset; the scroll area clamps it.
            if let Some(offset) = scroll_to_offset {
                area = area.vertical_scroll_offset(offset);
            }
            area.show(ui, |ui| {
                if scroll_delta != 0.0 {
                    // Negative y moves the content up, i.e. scrolls down.
                    ui.scroll_with_delta(egui::vec2(0.0, scroll_delta));
                }
                for (i, section) in self.sections.iter().enumerate() {
                    // Place an invisible anchor widget before the section
                    let response = ui.allocate_response(egui::vec2(0.0, 0.0), egui::Sense::hover());

                    // If this is the target section, scroll to the anchor
                    if scroll_to == Some(i) {
                        response.scroll_to_me(Some(egui::Align::TOP));
                    }

                    // Render the section
                    let anchor_id = ui.id().with(format!("section_{i}"));
                    ui.push_id(anchor_id, |ui| {
                        CommonMarkViewer::new().show(ui, &mut self.caches[i], section);
                    });
                }
            });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

/// Resolve relative image paths in markdown to inline data URIs.
///
/// Data URIs are used for every image rather than `file://` URLs because:
/// - `file://` URLs break when paths contain spaces;
/// - data URIs are self-contained and always work.
///
/// SVG files are rasterized to PNG first to avoid `egui_commonmark` parsing issues.
fn resolve_local_image_paths(markdown: &str, base_dir: &std::path::Path) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)").unwrap());
    re.replace_all(markdown, |caps: &regex::Captures| {
        rewrite_image(
            &caps[1],
            &caps[2],
            &caps[0],
            base_dir,
            &crate::core::net::remote_image_data_uri,
        )
    })
    .to_string()
}

/// Rewrite a single `![alt](src)` link, returning `original` untouched when the
/// image cannot be embedded.
///
/// `fetch_remote` is injected so the tests can exercise the remote-image path
/// (#60) without ever touching the network.
fn rewrite_image(
    alt: &str,
    src: &str,
    original: &str,
    base_dir: &std::path::Path,
    fetch_remote: &dyn Fn(&str) -> Option<String>,
) -> String {
    // #60: remote images are downloaded and inlined as `data:` URIs, which
    // egui_commonmark renders through its own data-URL loader
    // (`egui_commonmark_backend/src/data_url_loader.rs`, pulled in by the
    // `embedded_image` feature).
    if crate::core::net::is_remote_url(src) {
        return match fetch_remote(src) {
            Some(data_uri) => format!("![{alt}]({data_uri})"),
            None => original.to_string(),
        };
    }
    if src.starts_with("data:") || src.starts_with("file://") {
        return original.to_string();
    }

    let abs_path = base_dir.join(src);
    // #61: images may live anywhere inside the enclosing project, not only next
    // to the Markdown file — but never outside of it.
    if !crate::core::paths::is_within_image_root(&abs_path, base_dir) {
        return original.to_string();
    }
    if !abs_path.exists() {
        return original.to_string();
    }

    if let Err(e) = crate::core::image_validation::validate_image_file(&abs_path) {
        return format!(
            "[⚠ Invalid image: {} — {}]",
            abs_path.file_name().unwrap_or_default().to_string_lossy(),
            e
        );
    }
    // SVG files: rasterize to PNG data URI to avoid parsing failures
    let is_svg = abs_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    if is_svg {
        // Try rasterizing SVG to PNG (handles complex SVGs better)
        if let Ok(data_uri) = rasterize_svg_to_png_data_uri(&abs_path) {
            return format!("![{alt}]({data_uri})");
        }
        // Fallback: embed SVG directly as data URI for egui_commonmark's SVG feature
        if let Ok(data_uri) = file_to_data_uri(&abs_path) {
            return format!("![{alt}]({data_uri})");
        }
        // SVG completely failed — skip it
        return original.to_string();
    }
    // All non-SVG images: embed as base64 data URI
    match file_to_data_uri(&abs_path) {
        Ok(data_uri) => format!("![{alt}]({data_uri})"),
        Err(_) => original.to_string(),
    }
}

/// Convert a local file to a base64 data URI string.
const MAX_IMAGE_FILE_SIZE: u64 = 100 * 1024 * 1024; // 100 MB

fn file_to_data_uri(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_IMAGE_FILE_SIZE {
        return Err(format!(
            "image file too large ({} bytes, max {})",
            metadata.len(),
            MAX_IMAGE_FILE_SIZE
        )
        .into());
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mime = match ext.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    };
    let data = std::fs::read(path)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    Ok(format!("data:{mime};base64,{b64}"))
}

/// Rasterize an SVG file to PNG and return as a base64 data URI.
/// Caps dimensions at 8192px to avoid GPU texture overflow.
fn rasterize_svg_to_png_data_uri(
    path: &std::path::Path,
) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;
    use std::sync::{Arc, OnceLock};

    const MAX_DIM: f32 = 8192.0;

    let svg_data = std::fs::read_to_string(path)?;

    // Reject files that aren't actually SVG (e.g. HTML pages saved with .svg extension)
    let trimmed = svg_data.trim_start();
    if (!trimmed.starts_with('<')
        || trimmed.starts_with("<!DOCTYPE html")
        || trimmed.starts_with("<html"))
        && !trimmed.contains("<svg")
    {
        return Err("File is not a valid SVG (possibly an HTML page)".into());
    }

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
    let tree = usvg::Tree::from_str(&svg_data, &options)?;
    let size = tree.size();
    let svg_w = size.width();
    let svg_h = size.height();

    if svg_w <= 0.0 || svg_h <= 0.0 {
        return Err("SVG has zero dimensions".into());
    }

    // Scale 2x for retina, but cap at MAX_DIM
    let ideal_scale = 2.0_f32;
    let max_scale_w = MAX_DIM / svg_w;
    let max_scale_h = MAX_DIM / svg_h;
    let scale = ideal_scale.min(max_scale_w).min(max_scale_h);

    let width = (svg_w * scale) as u32;
    let height = (svg_h * scale) as u32;

    if width == 0 || height == 0 {
        return Err("SVG too small after scaling".into());
    }

    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("Failed to create pixmap")?;
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let png_data = pixmap.encode_png()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);
    Ok(format!("data:image/png;base64,{b64}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `apply_style` needs a context, not a window, so the palette and the type
    /// scale can be checked without opening anything.
    #[test]
    fn the_style_carries_the_shared_palette_into_both_themes() {
        use crate::core::style;

        let ctx = egui::Context::default();
        apply_style(&ctx);

        for (theme, palette, name) in [
            (egui::Theme::Dark, &style::DARK, "dark"),
            (egui::Theme::Light, &style::LIGHT, "light"),
        ] {
            let style = ctx.style_of(theme);
            let expect = |c: style::Rgb| egui::Color32::from_rgb(c[0], c[1], c[2]);

            assert_eq!(
                style.visuals.panel_fill,
                expect(palette.bg),
                "{name}: the page background must come from the shared palette"
            );
            assert_eq!(
                style.visuals.hyperlink_color,
                expect(palette.link),
                "{name}: links must come from the shared palette"
            );
            assert_eq!(
                style.visuals.code_bg_color,
                expect(palette.inline_code_bg),
                "{name}: inline code must use the chip background, not the block one"
            );
            assert_eq!(
                style.visuals.weak_text_color(),
                expect(palette.muted),
                "{name}: blockquotes read weak_text_color, so it has to be set"
            );
            assert_eq!(
                style.visuals.widgets.hovered.fg_stroke.color,
                expect(palette.strong),
                "{name}: egui draws bold by colour alone, so strong must differ"
            );
            assert_ne!(
                style.visuals.widgets.hovered.fg_stroke.color,
                style.visuals.widgets.inactive.fg_stroke.color,
                "{name}: bold text must not be the same colour as body text"
            );
        }
    }

    #[test]
    fn the_style_sets_the_shared_type_scale() {
        use crate::core::style::{BASE_FONT_SIZE, CODE_FONT_SCALE, heading_size};
        use egui::TextStyle;

        let ctx = egui::Context::default();
        apply_style(&ctx);
        let style = ctx.style_of(egui::Theme::Dark);

        assert_eq!(
            style.text_styles[&TextStyle::Body].size,
            BASE_FONT_SIZE,
            "the body must be the shared size, not egui's 13 pt default"
        );
        assert_eq!(
            style.text_styles[&TextStyle::Heading].size,
            heading_size(1),
            "the heading style is the h1 end of the scale"
        );
        assert_eq!(
            style.text_styles[&TextStyle::Monospace].size,
            BASE_FONT_SIZE * CODE_FONT_SCALE,
            "code is set at a fraction of the prose around it"
        );
        assert!(
            style.text_styles[&TextStyle::Heading].size > style.text_styles[&TextStyle::Body].size,
            "a heading that is not larger than the body is the bug this fixed"
        );
    }

    // --- split_by_headings tests ---

    #[test]
    fn split_by_headings_single_heading() {
        let md = "# Title\nSome content\n";
        let (has_preamble, sections) = split_by_headings(md);
        assert!(!has_preamble);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].contains("# Title"));
        assert!(sections[0].contains("Some content"));
    }

    #[test]
    fn split_by_headings_multiple_headings() {
        let md = "# First\nContent 1\n## Second\nContent 2\n### Third\nContent 3\n";
        let (has_preamble, sections) = split_by_headings(md);
        assert!(!has_preamble);
        assert_eq!(sections.len(), 3);
        assert!(sections[0].contains("# First"));
        assert!(sections[1].contains("## Second"));
        assert!(sections[2].contains("### Third"));
    }

    #[test]
    fn split_by_headings_with_preamble() {
        let md = "Some introductory text.\n\n# First Heading\nContent here.\n";
        let (has_preamble, sections) = split_by_headings(md);
        assert!(has_preamble);
        assert_eq!(sections.len(), 2);
        assert!(sections[0].contains("Some introductory text."));
        assert!(sections[1].contains("# First Heading"));
    }

    #[test]
    fn split_by_headings_no_headings() {
        let md = "Just some text.\nNo headings here.\n";
        let (has_preamble, sections) = split_by_headings(md);
        assert!(has_preamble);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].contains("Just some text."));
    }

    #[test]
    fn split_by_headings_empty_input() {
        let (has_preamble, sections) = split_by_headings("");
        assert!(!has_preamble);
        assert!(sections.is_empty());
    }

    #[test]
    fn split_by_headings_hash_in_code_block_not_split() {
        // Lines starting with # inside code are not headings if they lack
        // the space after the # sequence. But the function checks for trimmed.starts_with(' ')
        // so `# comment` inside code would still split. This tests that non-heading # lines
        // (like shebang #!) are ignored.
        let md = "# Title\n#!/bin/bash\necho hello\n";
        let (has_preamble, sections) = split_by_headings(md);
        assert!(!has_preamble);
        // The shebang line starts with #! which is filtered by !line.starts_with("#!")
        assert_eq!(sections.len(), 1);
        assert!(sections[0].contains("#!/bin/bash"));
    }

    #[test]
    fn split_by_headings_fenced_code_hash_not_split() {
        let md = "# Title\n\n```bash\n$>cat file\n# Comment in code rendered as title\n```\n";
        let (has_preamble, sections) = split_by_headings(md);
        assert!(!has_preamble);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].contains("# Comment in code rendered as title"));
    }

    #[test]
    fn split_by_headings_shebang_as_first_line() {
        let md = "#!/bin/bash\n# Title\nContent\n";
        let (has_preamble, sections) = split_by_headings(md);
        // First line is #!/bin/bash which is not a heading -> preamble
        assert!(has_preamble);
        assert_eq!(sections.len(), 2);
    }

    #[test]
    fn split_by_headings_consecutive_headings() {
        let md = "# H1\n## H2\n## H3\n";
        let (has_preamble, sections) = split_by_headings(md);
        assert!(!has_preamble);
        assert_eq!(sections.len(), 3);
    }

    #[test]
    fn split_by_headings_heading_without_space_not_treated_as_heading() {
        // "#notaheading" should not be treated as a heading (no space after #)
        let md = "# Real Heading\n#notaheading\ntext\n";
        let (has_preamble, sections) = split_by_headings(md);
        assert!(!has_preamble);
        // #notaheading lacks space after #, so it doesn't split
        assert_eq!(sections.len(), 1);
        assert!(sections[0].contains("#notaheading"));
    }

    /// #57: `extract_toc` (comrak) sees setext headings, so the sections must
    /// see them too — otherwise every TOC entry after the first setext title
    /// scrolls to the wrong place.
    #[test]
    fn split_by_headings_matches_toc_for_setext_headings() {
        let md = "Intro text.\n\nFirst\n=====\n\nBody one.\n\nSecond\n------\n\nBody two.\n\n## Third\n\nBody three.\n";
        let toc = toc::extract_toc(md);
        let (has_preamble, sections) = split_by_headings(md);

        assert_eq!(toc.len(), 3, "{toc:?}");
        assert!(has_preamble);
        assert_eq!(sections.len(), toc.len() + 1, "sections: {sections:?}");
        for (i, entry) in toc.iter().enumerate() {
            let section = &sections[i + 1];
            assert!(
                section.lines().next().unwrap_or("").contains(&entry.text),
                "section {} = {:?} should start with {:?}",
                i + 1,
                section,
                entry.text
            );
        }
    }

    #[test]
    fn split_by_headings_preserves_content_within_sections() {
        let md = "# Title\nLine 1\nLine 2\n\n## Next\nLine 3\n";
        let (_, sections) = split_by_headings(md);
        assert!(sections[0].contains("Line 1"));
        assert!(sections[0].contains("Line 2"));
        assert!(sections[1].contains("Line 3"));
    }

    /// The `---` of a YAML front matter block is consumed by comrak, so it is
    /// preamble, never a setext heading (#56 / #57).
    #[test]
    fn split_by_headings_treats_front_matter_as_preamble() {
        let md = "---\ntitle: hello\n---\n\n# Title\n\nBody.\n";
        let toc = toc::extract_toc(md);
        let (has_preamble, sections) = split_by_headings(md);

        assert_eq!(toc.len(), 1);
        assert!(has_preamble);
        assert_eq!(sections.len(), 2, "sections: {sections:?}");
        assert!(sections[0].contains("title: hello"));
        assert!(sections[1].starts_with("# Title"));
    }

    // --- key_action tests (#63) ---

    #[test]
    fn cmd_or_ctrl_f_opens_and_closes_the_search() {
        // `Modifiers::COMMAND` is ⌘ on macOS and Ctrl elsewhere: the single
        // binding covers both platforms.
        assert_eq!(
            key_action(egui::Key::F, egui::Modifiers::COMMAND, false),
            Some(Action::OpenSearch)
        );
        assert_eq!(
            key_action(egui::Key::F, egui::Modifiers::COMMAND, true),
            Some(Action::CloseSearch)
        );
    }

    #[test]
    fn a_bare_control_key_on_macos_does_not_open_the_search() {
        // On macOS ⌃F arrives as `ctrl` without `command`; only ⌘F counts.
        assert_eq!(key_action(egui::Key::F, egui::Modifiers::CTRL, false), None);
    }

    #[test]
    fn cmd_q_and_cmd_w_quit_even_while_searching() {
        for key in [egui::Key::Q, egui::Key::W] {
            assert_eq!(
                key_action(key, egui::Modifiers::COMMAND, false),
                Some(Action::Quit)
            );
            assert_eq!(
                key_action(key, egui::Modifiers::COMMAND, true),
                Some(Action::Quit)
            );
        }
    }

    #[test]
    fn bare_q_quits_only_when_the_search_is_closed() {
        assert_eq!(
            key_action(egui::Key::Q, egui::Modifiers::NONE, false),
            Some(Action::Quit)
        );
        assert_eq!(key_action(egui::Key::Q, egui::Modifiers::NONE, true), None);
    }

    #[test]
    fn escape_closes_the_search_before_it_closes_the_window() {
        assert_eq!(
            key_action(egui::Key::Escape, egui::Modifiers::NONE, true),
            Some(Action::CloseSearch)
        );
        assert_eq!(
            key_action(egui::Key::Escape, egui::Modifiers::NONE, false),
            Some(Action::Quit)
        );
    }

    #[test]
    fn f10_toggles_the_toc_even_while_searching() {
        assert_eq!(
            key_action(egui::Key::F10, egui::Modifiers::NONE, true),
            Some(Action::ToggleToc)
        );
    }

    #[test]
    fn scrolling_keys_match_the_other_backends() {
        let none = egui::Modifiers::NONE;
        assert_eq!(
            key_action(egui::Key::ArrowDown, none, false),
            Some(Action::ScrollDown)
        );
        assert_eq!(
            key_action(egui::Key::J, none, false),
            Some(Action::ScrollDown)
        );
        assert_eq!(
            key_action(egui::Key::ArrowUp, none, false),
            Some(Action::ScrollUp)
        );
        assert_eq!(
            key_action(egui::Key::K, none, false),
            Some(Action::ScrollUp)
        );
        assert_eq!(
            key_action(egui::Key::PageDown, none, false),
            Some(Action::PageDown)
        );
        assert_eq!(
            key_action(egui::Key::Space, none, false),
            Some(Action::PageDown)
        );
        assert_eq!(
            key_action(egui::Key::PageUp, none, false),
            Some(Action::PageUp)
        );
        assert_eq!(
            key_action(egui::Key::Home, none, false),
            Some(Action::GoTop)
        );
        assert_eq!(
            key_action(egui::Key::End, none, false),
            Some(Action::GoBottom)
        );
        assert_eq!(key_action(egui::Key::G, none, false), Some(Action::GoTop));
        assert_eq!(
            key_action(egui::Key::G, egui::Modifiers::SHIFT, false),
            Some(Action::GoBottom)
        );
    }

    #[test]
    fn no_bare_key_fires_while_typing_in_the_search_field() {
        let none = egui::Modifiers::NONE;
        for key in [
            egui::Key::J,
            egui::Key::K,
            egui::Key::G,
            egui::Key::Space,
            egui::Key::ArrowDown,
            egui::Key::ArrowUp,
            egui::Key::PageDown,
            egui::Key::PageUp,
            egui::Key::Home,
            egui::Key::End,
        ] {
            assert_eq!(key_action(key, none, true), None, "{key:?} fired");
        }
        assert_eq!(key_action(egui::Key::G, egui::Modifiers::SHIFT, true), None);
    }

    #[test]
    fn unhandled_keys_produce_no_action() {
        assert_eq!(key_action(egui::Key::Z, egui::Modifiers::NONE, false), None);
        assert_eq!(key_action(egui::Key::J, egui::Modifiers::ALT, false), None);
    }

    // --- image rewriting tests (#60, #61) ---

    const PNG: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00];

    /// A project with a `.git` marker, a `docs/` directory and `images/logo.png`.
    fn project() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        std::fs::create_dir_all(proj.join("docs")).unwrap();
        std::fs::create_dir_all(proj.join("images")).unwrap();
        std::fs::write(proj.join("images/logo.png"), PNG).unwrap();
        std::fs::write(tmp.path().join("secret.png"), PNG).unwrap();
        tmp
    }

    fn never_fetch(_: &str) -> Option<String> {
        panic!("the tests must never touch the network");
    }

    /// #61: `docs/page.md` referencing `../images/logo.png` must be embedded,
    /// not silently dropped by the traversal guard.
    #[test]
    fn an_image_in_a_sibling_directory_of_the_project_is_embedded() {
        let tmp = project();
        let docs = tmp.path().join("proj/docs");
        let out = rewrite_image(
            "logo",
            "../images/logo.png",
            "![logo](../images/logo.png)",
            &docs,
            &never_fetch,
        );
        assert!(
            out.starts_with("![logo](data:image/png;base64,"),
            "got {out}"
        );
    }

    /// The widened root still stops at the project boundary.
    #[test]
    fn an_image_outside_the_project_is_still_refused() {
        let tmp = project();
        let docs = tmp.path().join("proj/docs");
        let original = "![x](../../secret.png)";
        assert_eq!(
            rewrite_image("x", "../../secret.png", original, &docs, &never_fetch),
            original
        );
    }

    /// #60: a remote image is downloaded once and inlined as a `data:` URI,
    /// which `egui_commonmark`'s data-URL loader can display.
    #[test]
    fn remote_images_become_data_uris() {
        let tmp = project();
        let docs = tmp.path().join("proj/docs");
        let fetch = |url: &str| {
            assert_eq!(url, "https://example.com/badge.png");
            Some("data:image/png;base64,YWI=".to_string())
        };
        assert_eq!(
            rewrite_image(
                "badge",
                "https://example.com/badge.png",
                "![badge](https://example.com/badge.png)",
                &docs,
                &fetch,
            ),
            "![badge](data:image/png;base64,YWI=)"
        );
    }

    /// Offline, or on a failed download, the original link is kept.
    #[test]
    fn an_unfetchable_remote_image_is_left_untouched() {
        let tmp = project();
        let docs = tmp.path().join("proj/docs");
        let original = "![badge](https://example.com/badge.png)";
        assert_eq!(
            rewrite_image(
                "badge",
                "https://example.com/badge.png",
                original,
                &docs,
                &|_| None,
            ),
            original
        );
    }

    #[test]
    fn data_and_file_uris_are_left_untouched() {
        let tmp = project();
        let docs = tmp.path().join("proj/docs");
        for src in ["data:image/png;base64,YWI=", "file:///tmp/a.png"] {
            let original = format!("![x]({src})");
            assert_eq!(
                rewrite_image("x", src, &original, &docs, &never_fetch),
                original
            );
        }
    }
}
