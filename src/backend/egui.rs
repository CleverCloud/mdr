use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::core::mermaid::preprocess_mermaid_for_egui;
use crate::core::toc::{self, TocEntry};

pub fn run(file_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let raw_markdown = std::fs::read_to_string(&file_path)
        .unwrap_or_else(|e| format!("# Error\nCould not read `{}`: {}", file_path.display(), e));

    let toc_entries = toc::extract_toc(&raw_markdown);
    let markdown = preprocess_mermaid_for_egui(&raw_markdown);

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
                cache: CommonMarkCache::default(),
                file_path: file_path_clone,
                watcher_rx,
                toc_entries,
            }))
        }),
    )
    .map_err(|e| e.to_string().into())
}

struct MdrApp {
    markdown: String,
    cache: CommonMarkCache,
    file_path: PathBuf,
    watcher_rx: Receiver<()>,
    toc_entries: Vec<TocEntry>,
}

impl eframe::App for MdrApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check for file changes
        if self.watcher_rx.try_recv().is_ok() {
            while self.watcher_rx.try_recv().is_ok() {}
            if let Ok(content) = std::fs::read_to_string(&self.file_path) {
                self.toc_entries = toc::extract_toc(&content);
                self.markdown = preprocess_mermaid_for_egui(&content);
                self.cache = CommonMarkCache::default();
            }
        }

        // TOC sidebar
        egui::SidePanel::left("toc_panel")
            .default_width(220.0)
            .show(ctx, |ui| {
                ui.heading("Table of Contents");
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for entry in &self.toc_entries {
                        let indent = ((entry.level as f32 - 1.0) * 12.0).max(0.0);
                        ui.horizontal(|ui| {
                            ui.add_space(indent);
                            let text = match entry.level {
                                1 => egui::RichText::new(&entry.text).strong(),
                                2 => egui::RichText::new(&entry.text).strong().size(13.0),
                                3 => egui::RichText::new(&entry.text).size(13.0),
                                _ => egui::RichText::new(&entry.text).size(12.0).weak(),
                            };
                            ui.label(text);
                        });
                    }
                });
            });

        // Main content
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                CommonMarkViewer::new().show(ui, &mut self.cache, &self.markdown);
            });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}
