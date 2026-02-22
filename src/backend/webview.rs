use std::path::PathBuf;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

use crate::core::markdown::{parse_markdown, GITHUB_CSS};
use crate::core::toc;

pub fn run(file_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let markdown_content = std::fs::read_to_string(&file_path)?;
    let html_body = parse_markdown(&markdown_content);
    let toc_entries = toc::extract_toc(&markdown_content);
    let full_html = build_html(&html_body, &toc_entries);

    let watcher_rx = crate::core::watcher::watch_file(&file_path)?;

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(format!("mdr - {}", file_path.display()))
        .with_inner_size(tao::dpi::LogicalSize::new(1100.0, 900.0))
        .build(&event_loop)?;

    let webview = WebViewBuilder::new()
        .with_html(&full_html)
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // Check for file changes
        if watcher_rx.try_recv().is_ok() {
            while watcher_rx.try_recv().is_ok() {}
            if let Ok(content) = std::fs::read_to_string(&file_path) {
                let new_html = parse_markdown(&content);
                let new_toc = toc::extract_toc(&content);
                let toc_html = build_toc_html(&new_toc);

                let escaped_body = new_html.replace('\\', "\\\\").replace('`', "\\`");
                let escaped_toc = toc_html.replace('\\', "\\\\").replace('`', "\\`");
                let js = format!(
                    "document.querySelector('.content').innerHTML = `{}`; document.querySelector('.sidebar ul').innerHTML = `{}`;",
                    escaped_body, escaped_toc
                );
                let _ = webview.evaluate_script(&js);
            }
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            _ => {}
        }
    });
}

fn build_toc_html(entries: &[toc::TocEntry]) -> String {
    let mut toc = String::new();
    for entry in entries {
        toc.push_str(&format!(
            "<li class=\"toc-h{}\"><a href=\"#{}\">{}</a></li>",
            entry.level, entry.anchor, entry.text
        ));
    }
    toc
}

fn build_html(body: &str, toc_entries: &[toc::TocEntry]) -> String {
    let toc_html = build_toc_html(toc_entries);

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>{css}</style>
</head>
<body>
<nav class="sidebar">
<p class="sidebar-title">Table of Contents</p>
<ul>{toc}</ul>
</nav>
<div class="content">
{body}
</div>
<script>
document.querySelector('.sidebar').addEventListener('click', function(e) {{
    if (e.target.tagName === 'A') {{
        e.preventDefault();
        var id = e.target.getAttribute('href').substring(1);
        var el = document.getElementById(id);
        if (el) {{
            el.scrollIntoView({{ behavior: 'smooth', block: 'start' }});
            document.querySelectorAll('.sidebar a').forEach(a => a.classList.remove('active'));
            e.target.classList.add('active');
        }}
    }}
}});
</script>
</body>
</html>"#,
        css = GITHUB_CSS,
        toc = toc_html,
        body = body
    )
}
