# MDR - Test Document

This is a **test document** for comparing rendering approaches.
Here's some *italic text* and ~~strikethrough~~.

## Code Example

```rust
fn main() {
    let greeting = "Hello, MDR!";
    println!("{}", greeting);

    for i in 0..5 {
        println!("Count: {}", i);
    }
}
```

## Table

| Feature | egui | WebView |
|---------|------|---------|
| Pure Rust | ✅ | ❌ |
| HTML/CSS rendering | ❌ | ✅ |
| Accessibility | ❌ | ✅ |
| Zero dependencies | ✅ | WebKit |

## Task List

- [x] Parse Markdown
- [x] Render headings
- [ ] Mermaid support
- [ ] File watching

## Mermaid Diagram

```mermaid
graph TD
    A[Markdown File] --> B[Parser]
    B --> C[Check Type]
    C --> D[Mermaid Renderer]
    C --> E[HTML/egui Renderer]
    D --> F[SVG Output]
    E --> G[Display]
    F --> G
```

### Sequence Diagram

```mermaid
sequenceDiagram
    User->>MDR: Open file.md
    MDR->>Comrak: Parse markdown
    Comrak-->>MDR: HTML output
    MDR->>Watcher: Watch file
    Watcher-->>MDR: File changed
    MDR->>MDR: Re-render
```

### Pie Chart

```mermaid
pie title Backend Usage
    "egui" : 45
    "WebView" : 35
    "TUI" : 20
```

## Blockquote

> "The best tool is the one you actually use."
> — Someone wise

## Image Test

### Remote URL image

![Rust Logo](https://www.rust-lang.org/logos/rust-logo-256x256.png)

### Base64 inline image

![tiny red dot](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAoAAAAKCAYAAACNMs+9AAAAFklEQVQYV2P8z8BQz0BBwMgwqpBihQAA9QMBE/k5hgAAAABJRU5ErkJggg==)

### Link and inline code

Here's an inline reference to a [link](https://example.com) and some `inline code`.

---

*End of test document*
