# MDR — Markdown Reader

## Pitch (Shape Up Format)

### Problem

Developers and technical writers who use LLMs (Claude, GPT, etc.) regularly receive Markdown output containing Mermaid diagrams, code blocks, tables, and images. There is **no lightweight, standalone tool** to simply view this Markdown with proper rendering.

Current options all fail:
- **Typora / Obsidian**: Electron-based, 300MB+, bloated editors when you just need a viewer
- **glow / mdcat**: Terminal-only, zero Mermaid support, no images
- **VS Code**: Requires extensions, it's an IDE not a viewer
- **mermaid-cli**: Requires Node.js + Puppeteer + headless Chromium, 900ms+ per render

There is no `mdr file.md` command that opens a lightweight native window with live-updating rendered Markdown, Mermaid diagrams, syntax-highlighted code, and images.

### Appetite

**3-4 weeks** for MVP. Incremental feature additions after that.

### Solution

A single Rust binary (`mdr`) with **two rendering backends** that the user can choose:

1. **`--backend egui`** (default) — Pure Rust GPU rendering via egui/eframe. Zero JS, zero WebView, single static binary.
2. **`--backend webview`** — OS-native WebView (WebKit on macOS) for GitHub-quality HTML/CSS rendering. Still a Rust binary, but uses the OS's built-in browser engine.

Both backends share the same core pipeline:
1. Takes a Markdown file path as argument
2. Opens a native window with the rendered Markdown
3. Renders with full GFM support, syntax highlighting, Mermaid diagrams (as SVG), and images
4. Watches the file for changes and live-reloads, preserving scroll position

This dual-backend approach lets users pick the trade-off: **pure Rust performance** vs **polished HTML/CSS visuals**.

### Rabbit Holes

- **Mermaid rendering completeness**: `mermaid-rs-renderer` (mmdr) supports 23 diagram types but may not have 100% visual parity with Mermaid.js for complex diagrams. Accept this trade-off for MVP; fallback to Mermaid.js in v2.
- **egui text layout for large documents**: egui's immediate-mode architecture re-lays-out the entire UI each frame. Very large Markdown files (500KB+) may have performance issues. No optimization planned for MVP — render everything.
- **egui visual polish**: egui renders with system sans-serif fonts, basic margins, simple tables. It looks "dev-toolish" rather than GitHub-polished. This is an accepted trade-off for the 100% Rust stack.
- **OS theme detection**: egui supports dark/light mode but theme switching at runtime (when OS theme changes) needs to be verified.

### No-Gos

1. **NOT an editor** — Read-only viewer. No editing mode, no save, no cursor, ever.
2. **NOT cloud-connected** — No sync, no accounts, no telemetry, no network calls (except fetching remote images referenced in the Markdown).
3. **NOT Electron/Node** — Zero Electron, zero Node.js, zero npm. The WebView backend uses the OS's built-in WebKit engine (already installed on macOS), not an embedded browser. The egui backend is 100% pure Rust.

---

## Technical Specification

### Architecture

```
fichier.md
  │
  ├→ notify (watch file changes, 300ms debounce)
  │
  ├→ comrak (parse GFM → AST)
  │
  ├→ mermaid-rs-renderer (mermaid fences → SVG)
  │
  ├→ syntect (code blocks → highlighted HTML/spans)
  │
  └→ Backend (user choice via --backend flag)
        │
        ├─ egui (default)
        │    ├→ egui_commonmark (MD → egui widgets)
        │    ├→ resvg (SVG → texture for Mermaid)
        │    └→ eframe window (GPU rendered)
        │
        └─ webview
             ├→ comrak (MD → HTML)
             ├→ CSS (GitHub-like, OS theme aware)
             ├→ SVG inline (Mermaid diagrams)
             └→ wry window (OS native WebView)
```

The core logic (file watching, Markdown parsing, Mermaid rendering, syntax highlighting) is shared. Only the final rendering step differs between backends.

### Crate Dependencies

#### Core (shared by both backends)

| Crate | Purpose | Version |
|-------|---------|---------|
| `comrak` | GFM Markdown parser | 0.55 |
| `syntect` | Syntax highlighting | 5.x |
| `mermaid-rs-renderer` | Mermaid → SVG (native Rust) | 0.3 |
| `notify` | Filesystem watching (FSEvents on macOS) | 8.x |
| `image` | Image decoding (PNG, JPEG, etc.) | 0.25+ |
| `resvg` | SVG → raster | 0.48 |
| `clap` | CLI argument parsing | 4.x |

#### egui backend

| Crate | Purpose | Version |
|-------|---------|---------|
| `eframe` | Native window + egui integration | 0.36 |
| `egui_commonmark` | Markdown → egui widgets | 0.25 |

#### WebView backend

| Crate | Purpose | Version |
|-------|---------|---------|
| `wry` | OS-native WebView | 0.57 |
| `tao` | Window management | 0.37 |

### Cargo Features

Both backends are compiled by default. Users can opt out via feature flags:

```toml
[features]
default = ["egui-backend", "webview-backend"]
egui-backend = ["eframe", "egui_commonmark"]
webview-backend = ["wry", "tao"]
```

### egui_commonmark Features

```toml
egui_commonmark = { version = "0.25", features = [
    "better_syntax_highlighting",  # syntect-based highlighting
    "load-images",                 # local image loading
    "svg",                         # SVG rendering
    "fetch",                       # remote URL image fetching
    "embedded_image",              # base64 inline images
] }
```

### CLI Interface

```
USAGE:
    mdr [OPTIONS] <FILE>

ARGS:
    <FILE>    Path to a Markdown file to view

OPTIONS:
    -b, --backend <BACKEND>    Rendering backend [default: egui] [possible: egui, webview]
    -h, --help                 Print help information
    -V, --version              Print version information

EXAMPLES:
    mdr README.md                          # egui backend (default)
    mdr --backend webview README.md        # WebView backend
    mdr -b webview docs/architecture.md    # short form
```

### User Stories & Acceptance Criteria

#### US-1: View a Markdown file

> As a developer, I want to run `mdr file.md` and see the rendered Markdown in a native window, so that I can read LLM-generated documentation comfortably.

**Acceptance Criteria:**
- Given a valid .md file, when I run `mdr file.md`, then a window opens with the rendered content
- Given the file contains GFM (tables, task lists, strikethrough, footnotes), when rendered, then all GFM extensions display correctly
- Given the file does not exist, when I run `mdr nonexistent.md`, then an error message is shown in the window

#### US-2: Live reload on file changes

> As a developer, I want the rendered view to update automatically when the Markdown file changes, so that I can see LLM output in real-time.

**Acceptance Criteria:**
- Given the file is open in mdr, when the file is modified externally, then the rendered view updates within 500ms
- Given I am scrolled to a specific position, when the file updates, then my scroll position is preserved
- Given rapid successive changes (LLM writing), when debounced at 300ms, then the view updates smoothly without flickering

#### US-3: Mermaid diagram rendering

> As a developer, I want Mermaid code fences to render as visual diagrams, so that I can understand architecture and flow diagrams generated by LLMs.

**Acceptance Criteria:**
- Given a ````mermaid` code fence with valid syntax, when rendered, then a visual diagram (SVG rendered as image) appears
- Given a Mermaid block with invalid syntax, when rendered, then an error message is shown inline along with the raw source code
- Given diagram types supported by mmdr (flowchart, sequence, class, state, ER, pie, gantt, etc.), when rendered, then they display correctly

#### US-4: Syntax-highlighted code blocks

> As a developer, I want code blocks to have syntax highlighting, so that code examples are readable.

**Acceptance Criteria:**
- Given a fenced code block with a language tag (```rust, ```python, etc.), when rendered, then the code is syntax-highlighted
- Given a code block without a language tag, when rendered, then it displays as plain monospace text
- Given inline code (`code`), when rendered, then it appears in a monospace font with a distinct background

#### US-5: Image rendering

> As a developer, I want images referenced in the Markdown to display, so that I can see diagrams and screenshots.

**Acceptance Criteria:**
- Given `![alt](./path/to/image.png)` with a relative local path, when rendered, then the image displays (path resolved relative to the .md file)
- Given `![alt](https://example.com/image.png)` with a remote URL, when rendered, then the image is fetched and displayed
- Given `![alt](data:image/png;base64,...)` with base64 data, when rendered, then the image displays inline
- Given a broken image reference, when rendered, then alt text is shown instead

#### US-6: OS theme support

> As a developer, I want the viewer to respect my OS dark/light mode setting, so that it doesn't strain my eyes.

**Acceptance Criteria:**
- Given macOS is in dark mode, when mdr opens, then the background is dark and text is light
- Given macOS is in light mode, when mdr opens, then the background is light and text is dark

#### US-7: HTML passthrough

> As a developer, I want raw HTML in my Markdown to render, so that complex formatting from LLM output works.

**Acceptance Criteria:**
- Given the Markdown contains raw HTML tags (div, span, table, img), when rendered, then they are passed through and rendered
- Given the Markdown contains script tags, when rendered, then they are passed through (trusted local context)

### Non-Functional Requirements

| Category | Requirement |
|----------|-------------|
| **Startup time** | Window visible in < 500ms for files under 100KB |
| **Binary size** | < 30MB (release build with LTO) |
| **Memory** | < 100MB RSS for typical Markdown files (< 500KB) |
| **Platform** | macOS (ARM + x86_64) primary. Linux/Windows secondary (v2) |
| **Dependencies** | Zero runtime dependencies. Single static binary. |
| **Rendering** | Full GFM compliance (tables, task lists, strikethrough, autolinks, footnotes) |

---

## Architecture Decision Records

### ADR-001: Dual backend architecture (egui + WebView)

**Context:** We need a window to display rendered Markdown. Options: egui (pure Rust GPU rendering), wry (OS native WebView), terminal TUI (ratatui). Both egui and WebView were prototyped and compared visually.

**Decision:** Support both backends, selectable via `--backend` CLI flag. Default to egui.

**Rationale:**
- **egui** (default): Zero JavaScript, zero WebView = true single binary with no runtime deps. GPU-accelerated. egui_commonmark provides production-ready Markdown rendering. Trade-off: less visually polished, no accessibility.
- **WebView** (optional): GitHub-quality HTML/CSS rendering via OS-native WebKit (macOS). Accessibility, rich typography, elegant layout. Trade-off: depends on OS WebView engine.
- Both were prototyped and compiled. Letting users choose gives the best of both worlds.
- The core pipeline (file watching, Markdown parsing, Mermaid rendering, syntax highlighting) is shared — only the final rendering layer differs.
- Cargo features allow compiling with only one backend if desired (`--no-default-features --features egui-backend`).

**Trade-offs accepted:**
- Slightly larger binary when both backends are compiled
- Two rendering code paths to maintain

### ADR-002: mermaid-rs-renderer for Mermaid diagrams

**Context:** Mermaid.js requires a browser/JS engine. Native Rust alternatives exist.

**Decision:** Use `mermaid-rs-renderer` (mmdr) for native Mermaid → SVG rendering.

**Rationale:**
- 2-6ms render time (100-1400x faster than mermaid-cli)
- Supports 23 diagram types
- Pure Rust, no JS engine
- SVG output can be rasterized via resvg and displayed as egui texture

**Trade-offs accepted:**
- Not 100% visual parity with Mermaid.js for complex diagrams
- Fallback to Mermaid.js deferred to v2

### ADR-003: comrak for Markdown parsing

**Context:** Two main Rust Markdown parsers: pulldown-cmark (pull/iterator) and comrak (AST, GFM).

**Decision:** comrak, via egui_commonmark's integration.

**Rationale:**
- Full GFM compliance out of the box (tables, task lists, strikethrough, autolinks, footnotes)
- egui_commonmark uses pulldown-cmark internally, but comrak provides the GFM features we need
- AST-based parsing enables future features (TOC generation, Mermaid block extraction)

### ADR-004: notify for file watching

**Context:** Need to detect file changes and trigger re-render.

**Decision:** `notify` crate v8 with 300ms debounce.

**Rationale:**
- Industry standard (used by cargo, mdBook)
- Uses FSEvents on macOS (efficient, no polling)
- Built-in debouncing prevents excessive re-renders during rapid writes (LLM streaming to file)

### ADR-005: Performance benchmarking between backends

**Context:** With two backends, users need data to make an informed choice. We need to measure and compare performance.

**Decision:** Include a built-in benchmark mode and publish performance comparison.

**Metrics to measure:**
- **Startup time**: Time from `mdr` invocation to window visible with rendered content
- **Render time**: Time to parse + render a Markdown file (excluding window creation)
- **Reload time**: Time from file change detection to updated render visible
- **Memory usage**: RSS at steady state after rendering
- **Binary size**: Release build size per backend and combined

**Benchmark files:**
- `bench/small.md` — 5KB, typical README
- `bench/medium.md` — 50KB, documentation page with tables and code
- `bench/large.md` — 500KB, large LLM output with many code blocks
- `bench/mermaid-heavy.md` — 20KB with 10+ Mermaid diagrams of different types

**Benchmark command:**

```
mdr --benchmark bench/medium.md              # benchmark default backend
mdr --benchmark --backend webview bench/medium.md  # benchmark webview
```

Output: startup_ms, render_ms, memory_mb, binary_size_mb

---

## Roadmap

### MVP (v0.1 — 3-4 weeks)

- [x] ~~Prototype validated (egui vs WebView comparison)~~
- [ ] Project bootstrap (`cargo init`, CI, README)
- [ ] Single file mode: `mdr file.md`
- [ ] GFM Markdown rendering (egui_commonmark + comrak)
- [ ] Syntax highlighting (syntect)
- [ ] Image support (local + URL + base64)
- [ ] Mermaid → SVG → egui image (mmdr integration)
- [ ] Mermaid error handling (inline error + source code)
- [ ] File watching + live reload (notify, 300ms debounce)
- [ ] Scroll position preservation on reload
- [ ] OS dark/light theme support
- [ ] Release build + binary distribution

### v0.2 — Post-MVP

- [ ] Directory mode: `mdr ./docs/` with sidebar file browser
- [ ] Table of Contents in sidebar (auto-generated from headings)
- [ ] stdin/pipe mode: `cat file.md | mdr`
- [ ] Internal navigation for .md links
- [ ] Mermaid.js fallback for unsupported diagram types
- [ ] Homebrew formula
- [ ] CSS custom override for theming
- [ ] Linux + Windows support
