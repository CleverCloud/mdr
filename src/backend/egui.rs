use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::core::mermaid::preprocess_mermaid_for_egui;
use crate::core::toc::{self, TocEntry};

pub fn run(file_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let base_dir = file_path.parent()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
        .unwrap_or_default();
    let raw_markdown = std::fs::read_to_string(&file_path)
        .unwrap_or_else(|e| format!("# Error\nCould not read `{}`: {}", file_path.display(), e));

    let toc_entries = toc::extract_toc(&raw_markdown);
    let markdown = preprocess_mermaid_for_egui(&raw_markdown);
    let markdown = resolve_local_image_paths(&markdown, &base_dir);
    let (has_preamble, sections) = split_by_headings(&markdown);

    let watcher_rx = crate::core::watcher::watch_file(&file_path)?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 900.0])
            .with_title(format!("mdr - {}", file_path.display())),
        ..Default::default()
    };

    let file_path_clone = file_path.clone();
    eframe::run_native(
        "mdr",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(MdrApp {
                markdown,
                sections,
                has_preamble,
                caches: Vec::new(),
                file_path: file_path_clone,
                base_dir,
                watcher_rx,
                toc_entries,
                scroll_to_section: None,
            }))
        }),
    )
    .map_err(|e| e.to_string().into())
}

/// Split markdown into sections at heading boundaries.
/// Returns (has_preamble, sections) where has_preamble is true if there's
/// content before the first heading (which means headings start at index 1).
fn split_by_headings(markdown: &str) -> (bool, Vec<String>) {
    let mut sections = Vec::new();
    let mut current = String::new();

    for line in markdown.lines() {
        if line.starts_with('#') && !line.starts_with("#!") {
            let trimmed = line.trim_start_matches('#');
            if trimmed.starts_with(' ') && !current.is_empty() {
                sections.push(current);
                current = String::new();
            }
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.is_empty() {
        sections.push(current);
    }

    // Check if section 0 starts with a heading or is preamble text
    let has_preamble = sections.first()
        .map(|s| {
            let first_line = s.lines().next().unwrap_or("");
            let trimmed = first_line.trim_start_matches('#');
            !(first_line.starts_with('#') && trimmed.starts_with(' '))
        })
        .unwrap_or(false);

    (has_preamble, sections)
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
}

impl eframe::App for MdrApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check for file changes
        if self.watcher_rx.try_recv().is_ok() {
            while self.watcher_rx.try_recv().is_ok() {}
            if let Ok(content) = std::fs::read_to_string(&self.file_path) {
                self.toc_entries = toc::extract_toc(&content);
                self.markdown = preprocess_mermaid_for_egui(&content);
                self.markdown = resolve_local_image_paths(&self.markdown, &self.base_dir);
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

        // TOC sidebar
        let has_preamble = self.has_preamble;
        let scroll_target = &mut self.scroll_to_section;

        egui::SidePanel::left("toc_panel")
            .default_width(220.0)
            .show(ctx, |ui| {
                ui.heading("Table of Contents");
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, entry) in self.toc_entries.iter().enumerate() {
                        let indent = ((entry.level as f32 - 1.0) * 12.0).max(0.0);
                        ui.horizontal(|ui| {
                            ui.add_space(indent);
                            let text = match entry.level {
                                1 => egui::RichText::new(&entry.text).strong(),
                                2 => egui::RichText::new(&entry.text).strong().size(13.0),
                                3 => egui::RichText::new(&entry.text).size(13.0),
                                _ => egui::RichText::new(&entry.text).size(12.0).weak(),
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

        // Main content - render each section with scroll anchors
        let scroll_to = self.scroll_to_section.take();

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, section) in self.sections.iter().enumerate() {
                    // Place an invisible anchor widget before the section
                    let response = ui.allocate_response(
                        egui::vec2(0.0, 0.0),
                        egui::Sense::hover(),
                    );

                    // If this is the target section, scroll to the anchor
                    if scroll_to == Some(i) {
                        response.scroll_to_me(Some(egui::Align::TOP));
                    }

                    // Render the section
                    let anchor_id = ui.id().with(format!("section_{}", i));
                    ui.push_id(anchor_id, |ui| {
                        CommonMarkViewer::new()
                            .show(ui, &mut self.caches[i], section);
                    });
                }
            });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

/// Resolve relative image paths in markdown to absolute file:// URLs.
/// Handles ![alt](relative/path.png) syntax.
fn resolve_local_image_paths(markdown: &str, base_dir: &std::path::Path) -> String {
    use regex::Regex;
    let re = Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)").unwrap();
    re.replace_all(markdown, |caps: &regex::Captures| {
        let alt = &caps[1];
        let src = &caps[2];
        // Skip URLs and data URIs
        if src.starts_with("http://") || src.starts_with("https://")
            || src.starts_with("data:") || src.starts_with("file://")
        {
            return caps[0].to_string();
        }
        let abs_path = base_dir.join(src);
        if abs_path.exists() {
            format!("![{}](file://{})", alt, abs_path.display())
        } else {
            caps[0].to_string()
        }
    })
    .to_string()
}
