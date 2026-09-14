use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::path::PathBuf;

use crate::core::mermaid::preprocess_mermaid_for_egui;
use crate::core::toc::{self, TocEntry};
use crate::core::watcher::Watch;

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

/// Families kept only to cover scripts the chosen UI font does not.
///
/// egui's own embedded fonts already carry Latin and a monochrome emoji set, so
/// this list exists for the rest — mostly CJK. It is deliberately short and
/// deliberately excludes the colour emoji fonts: `Apple Color Emoji.ttc` alone
/// is 183 MB on macOS, for glyphs egui can already draw.
const FALLBACK_FONT_FAMILIES: &[&str] = &[
    // macOS
    "PingFang SC",
    "Hiragino Sans",
    "Hiragino Sans GB",
    "Apple SD Gothic Neo",
    // Windows
    "Microsoft YaHei",
    "Yu Gothic",
    "Malgun Gothic",
    // Linux
    "Noto Sans CJK SC",
    "Noto Sans CJK JP",
    "Noto Sans CJK KR",
];

/// What the font loader is allowed to keep resident.
///
/// `epaint::FontData` holds its file as a `Cow<'static, [u8]>`, so every face
/// keeps its own copy and nothing is shared between two faces of the same
/// collection; `blob_from_font_data` then clones those bytes again when the
/// fonts are built, so a loaded file is resident roughly twice. Loading every
/// installed face — which is what this function used to do — retained 4.57 GB
/// of font data on the machine this was measured on, from 890 faces across 473
/// files. Another machine has another font collection: the shape of the problem
/// carries over, the number does not.
///
/// These numbers are a product choice, not a measured RSS ceiling: they bound
/// what mdr reads, not what egui then builds out of it.
struct FontBudget {
    /// Total bytes of system font files.
    bytes: u64,
    /// How many faces may be added, primaries included.
    faces: usize,
}

const FONT_BUDGET: FontBudget = FontBudget {
    bytes: 64 * 1024 * 1024,
    faces: 8,
};

/// One face picked out of the system database, before its file is read.
struct Candidate {
    /// Position in the preference list; lower is better.
    rank: usize,
    family: String,
    path: std::path::PathBuf,
    /// Index of the face inside a `.ttc` collection.
    index: u32,
}

/// Install the system fonts mdr draws with.
///
/// Selection happens on metadata alone and only the handful of files that come
/// out of it are read. The previous version read *every* installed font file
/// into memory — the whole file, once per face it contained — and pushed all of
/// them into both fallback chains; on the machine this was measured on that is
/// 890 faces and several gigabytes resident, on a document of any size.
fn load_system_fonts(ctx: &egui::Context) {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();

    // Ranked, not reduced to a single winner: the best face can turn out to be
    // unreadable or larger than the budget, and the loader has to be able to
    // reach for the next one. Only the first that fits is ever read.
    let mut ui: Vec<Candidate> = Vec::new();
    let mut mono: Vec<Candidate> = Vec::new();
    let mut fallbacks: Vec<Candidate> = Vec::new();

    // First pass: metadata only. Nothing is read from disk here.
    for face in db.faces() {
        let source = match &face.source {
            fontdb::Source::Binary(_) => continue,
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => path,
        };

        // Only upright regular faces are candidates: egui has no font weights —
        // `RichText::strong()` resolves to a colour — so a bold or italic face
        // would not be used as a weight, it would simply become the body font.
        // Weight and style alone do not separate a condensed or expanded face
        // from the plain one, and either would be a surprising body font.
        if face.weight != fontdb::Weight::NORMAL
            || face.style != fontdb::Style::Normal
            || face.stretch != fontdb::Stretch::Normal
        {
            continue;
        }

        let Some((family, _)) = face.families.first() else {
            continue;
        };
        let candidate = |rank: usize| Candidate {
            rank,
            family: family.clone(),
            path: source.clone(),
            index: face.index,
        };

        // One face per family in each list: a collection lists the same family
        // once per weight, and keeping them all is exactly the cost being
        // avoided here.
        let offer = |list: &mut Vec<Candidate>, preferences: &[&str]| {
            if let Some(rank) = preference(preferences, family) {
                match list.iter().position(|c| c.family == *family) {
                    Some(i) if rank < list[i].rank => list[i] = candidate(rank),
                    Some(_) => {}
                    None => list.push(candidate(rank)),
                }
            }
        };
        offer(&mut ui, UI_FONT_FAMILIES);
        offer(&mut mono, MONO_FONT_FAMILIES);
        offer(&mut fallbacks, FALLBACK_FONT_FAMILIES);
    }

    // The body and code fonts come first, so a tight budget spends itself on
    // what the document is actually set in rather than on a fallback.
    for list in [&mut ui, &mut mono, &mut fallbacks] {
        list.sort_by_key(|c| c.rank);
    }
    let mut ordered: Vec<(&Candidate, Role)> = Vec::new();
    ordered.extend(ui.iter().map(|c| (c, Role::Ui)));
    ordered.extend(mono.iter().map(|c| (c, Role::Mono)));
    ordered.extend(fallbacks.iter().map(|c| (c, Role::Fallback)));

    ctx.set_fonts(build_font_definitions(&ordered, &FONT_BUDGET));
}

/// What a selected face is there for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    /// The body font.
    Ui,
    /// The code font.
    Mono,
    /// Kept only to cover scripts the two above do not.
    Fallback,
}

/// Read the selected faces, within budget, and build the font definitions.
///
/// Split out from the selection above so the budget can be exercised against
/// files of known size: everything here works off `candidates`, in the order
/// given, and stops reading when either limit is reached.
fn build_font_definitions(
    candidates: &[(&Candidate, Role)],
    budget: &FontBudget,
) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let mut spent = 0_u64;
    let mut loaded: Vec<String> = Vec::new();
    let mut ui_key = None;
    let mut mono_key = None;

    for (candidate, role) in candidates {
        // A body and a code font, once each. The lists are ranked, so anything
        // after the one that fit is a worse choice for the same job — but a
        // fallback is not a choice between rivals, and every one that fits is
        // kept.
        let filled = match role {
            Role::Ui => ui_key.is_some(),
            Role::Mono => mono_key.is_some(),
            Role::Fallback => false,
        };
        if filled {
            continue;
        }

        // The file path and the index together are what actually name a face:
        // two files of the same family both report index 0, so keying on the
        // family alone made the second evict the first and the body font became
        // whichever loaded last.
        let key = format!("{}#{}", candidate.path.display(), candidate.index);

        // The same face can serve two roles — a family listed as both the UI
        // and the monospace preference — and then it must not be read, counted
        // or queued twice. Checked before the budget, so sharing is free.
        if !fonts.font_data.contains_key(&key) {
            if loaded.len() >= budget.faces {
                // Out of slots. Nothing later can fit either, but a face
                // already loaded could still take on a second role, so the loop
                // carries on rather than breaking.
                continue;
            }
            // Read under an explicit bound rather than trusting `metadata` and
            // calling `fs::read`: the size is taken from the open handle and
            // the read is capped by the same number, so a file that grows in
            // between cannot spend more than what was budgeted for it.
            let Some(data) = read_within(&candidate.path, budget.bytes - spent) else {
                // Over budget or unreadable: try the next candidate rather than
                // giving up. If none fits, egui's embedded fonts remain.
                continue;
            };
            spent += data.len() as u64;

            // `from_owned` always sets index 0. Inside a collection (.ttc) that
            // is a different face from the one selected, so the real index has
            // to be put back.
            let mut font_data = egui::FontData::from_owned(data);
            font_data.index = candidate.index;
            fonts.font_data.insert(key.clone(), font_data.into());

            // A fallback has to sit in both chains to do its job; a primary is
            // pushed to both as well and then moved to the front of its own
            // below, which also leaves it available as a fallback for the other.
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push(key.clone());
            }
            loaded.push(candidate.family.clone());
        }

        match role {
            Role::Ui => ui_key = Some(key),
            Role::Mono => mono_key = Some(key),
            Role::Fallback => {}
        }
    }

    crate::vlog!(
        "fonts: {:.1} MB of a {:.0} MB budget for {} of at most {} system face(s): {}",
        spent as f64 / 1_048_576.0,
        budget.bytes as f64 / 1_048_576.0,
        loaded.len(),
        budget.faces,
        loaded.join(", ")
    );

    // Move the chosen faces to the front of their family, ahead of egui's own.
    for (family, chosen) in [
        (egui::FontFamily::Proportional, ui_key),
        (egui::FontFamily::Monospace, mono_key),
    ] {
        if let Some(key) = chosen
            && let Some(list) = fonts.families.get_mut(&family)
        {
            list.retain(|existing| existing != &key);
            list.insert(0, key);
        }
    }

    fonts
}

/// Read `path`, or nothing at all if it is larger than `limit`.
///
/// The size is taken from the open handle, so the file that is measured is the
/// file that is read. What this guarantees is the budget: no more than `limit`
/// bytes are ever accepted. It does not guarantee a complete file — one that
/// shrinks between the two calls comes back short, and epaint rejects a
/// truncated font when it parses it.
fn read_within(path: &std::path::Path, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read as _;

    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > limit {
        return None;
    }
    // `len + 1` so a file that grew past the budget between the two calls comes
    // back longer than it was allowed to be, and is dropped below.
    let mut data = Vec::new();
    file.take(len + 1).read_to_end(&mut data).ok()?;
    (data.len() as u64 <= len).then_some(data)
}

/// Apply the shared palette and type scale.
///
/// Without this the backend runs on egui's defaults: a 13 pt body with an 18 pt
/// heading, so `h1` through `h6` all land within five points of each other,
/// while `web` renders the same document on a 16 px body and a 2 em `h1`.
/// Render the small set of HTML blocks that Markdown documents actually use.
///
/// `web` hands raw HTML to a real engine. `gui` has no engine: `egui_commonmark`
/// passes an HTML block through as text, so a README that centres its logo and
/// title with `<p>` and `<h1>` showed its own markup at the top of the window.
///
/// The answer is not an HTML renderer. It is a short, explicit list of tags —
/// headings, paragraphs and images — rewritten as the Markdown that means the
/// same thing. `<br>` becomes a space, since a cell of this conversion is one
/// run of text, so they go on to travel the existing pipeline: image
/// paths are resolved and SVGs rasterised exactly as for `![](…)`, and headings
/// land in the table of contents. `align="center"` has no Markdown equivalent
/// and is dropped; the content comes back, its layout does not.
///
/// Anything outside that list keeps its text and loses its tags, which is worse
/// than a browser and better than printing angle brackets at the reader.
///
/// Only whole HTML blocks are touched, and they are located by parsing rather
/// than by matching lines, so a `<p>` inside a fenced code block stays the code
/// it was written as.
///
/// Only blocks at the top level, at that. The replacement works on whole lines,
/// which cannot carry back the `>` of a block quote or the indent of a list
/// item — a block nested inside one is left exactly as it was written.
fn render_simple_html(markdown: &str, base_dir: &std::path::Path) -> String {
    use comrak::nodes::NodeValue;
    use comrak::{Arena, Options, parse_document};

    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.front_matter_delimiter = Some("---".to_owned());

    let root = parse_document(&arena, markdown, &options);

    // Line ranges are 1-based and inclusive, and collected before any edit so
    // the positions stay those of the document that was parsed.
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for node in root.children() {
        let data = node.data.borrow();
        if let NodeValue::HtmlBlock(block) = &data.value {
            let converted = html_to_markdown(&block.literal, base_dir);
            if !converted.trim().is_empty() {
                replacements.push((
                    data.sourcepos.start.line,
                    data.sourcepos.end.line,
                    converted,
                ));
            }
        }
    }
    if replacements.is_empty() {
        return markdown.to_string();
    }
    replacements.sort_by_key(|(start, _, _)| *start);

    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = String::with_capacity(markdown.len());
    let mut line_no = 1usize;
    let mut next = replacements.into_iter().peekable();
    while line_no <= lines.len() {
        match next.peek() {
            Some((start, end, _)) if *start == line_no => {
                let (_, end, converted) = next.next().expect("peeked");
                out.push_str(converted.trim_end());
                out.push('\n');
                line_no = end + 1;
            }
            _ => {
                out.push_str(lines[line_no - 1]);
                out.push('\n');
                line_no += 1;
            }
        }
    }
    out
}

/// Rewrite one HTML block as Markdown, for the tags listed in
/// [`render_simple_html`].
fn html_to_markdown(html: &str, base_dir: &std::path::Path) -> String {
    let mut out = String::new();
    let mut text = String::new();
    // The heading level currently open, so `</h2>` knows what it closes.
    let mut heading: Option<usize> = None;

    let flush = |out: &mut String, text: &mut String, heading: &mut Option<usize>| {
        let body = decode_entities(&text.split_whitespace().collect::<Vec<_>>().join(" "));
        text.clear();
        if body.is_empty() {
            return;
        }
        if let Some(level) = heading.take() {
            out.push_str(&"#".repeat(level));
            out.push(' ');
        }
        // What was between the tags is text. Without this, `<p>*a*</p>` came
        // out as emphasis and `<p># t</p>` became a heading — and, being a
        // heading, went into the table of contents.
        out.push_str(&escape_markdown(&body));
        out.push_str("\n\n");
    };

    let bytes: Vec<char> = html.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != '<' {
            text.push(bytes[i]);
            i += 1;
            continue;
        }
        let Some((tag, consumed)) = parse_tag(&bytes[i..]) else {
            // An unterminated `<` is text, not a tag.
            text.push('<');
            i += 1;
            continue;
        };
        i += consumed;
        let (name, closing) = (tag.name.as_str(), tag.closing);

        match name {
            "img" if !closing => {
                flush(&mut out, &mut text, &mut heading);
                if let Some(src) = tag.attribute("src") {
                    let alt = tag.attribute("alt").unwrap_or_default();
                    let width = tag
                        .attribute("width")
                        .and_then(|w| w.trim().parse::<f32>().ok());
                    // Resolved here, through the very path an `![](…)` takes:
                    // inside the image root, a real image of the type its name
                    // claims, remote only when mdr is allowed on the network.
                    // Markdown carries no width, so a declared one is passed
                    // along and honoured when the drawing is rasterised — which
                    // is possible for a vector image and not for a bitmap, so
                    // that is where it applies.
                    let alt = escape_alt(&alt);
                    let original = format!("![{alt}]({src})");
                    let resolved = rewrite_image_sized(
                        &alt,
                        &src,
                        &original,
                        base_dir,
                        width.filter(|w| *w > 0.0),
                        &crate::core::net::remote_image_data_uri,
                    );
                    out.push_str(&resolved);
                    out.push_str("\n\n");
                }
            }
            "br" => text.push(' '),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                flush(&mut out, &mut text, &mut heading);
                if !closing {
                    heading = name[1..].parse::<usize>().ok();
                }
            }
            "p" | "div" => flush(&mut out, &mut text, &mut heading),
            // An inline tag we do not handle: drop it, keep what it wrapped.
            _ => {}
        }
    }
    flush(&mut out, &mut text, &mut heading);
    out
}

/// One tag, read from `<` to its matching `>`.
struct Tag {
    /// Lowercased element name.
    name: String,
    /// Whether this is a closing tag.
    closing: bool,
    /// Attributes in the order they were written, names lowercased and values
    /// already decoded.
    attributes: Vec<(String, String)>,
}

impl Tag {
    /// The value of `name`, if the tag carries it.
    fn attribute(&self, name: &str) -> Option<String> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

/// Read the tag that starts at `chars[0]`, and how many characters it spans.
///
/// Attributes are read one after another rather than searched for, and the
/// closing `>` is only recognised outside a quoted value. Looking for the name
/// anywhere in the tag picked the wrong one out of
/// `<img title="old src='a.png'" src="b.png">`, and cutting at the first `>`
/// ended the tag inside a value that contained one.
///
/// Only quoted values are supported; an unquoted one is skipped, which is a
/// limit and not a guess.
fn parse_tag(chars: &[char]) -> Option<(Tag, usize)> {
    let mut i = 1usize; // past the `<`
    let closing = chars.get(i) == Some(&'/');
    if closing {
        i += 1;
    }
    let start = i;
    // The whole name, hyphens and underscores included. Stopping at the first
    // non-alphanumeric turned the custom element `<h1-title>` into an `h1` and
    // `<img-icon>` into an `img`.
    while chars
        .get(i)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
    {
        i += 1;
    }
    if i == start {
        return None;
    }
    let name: String = chars[start..i]
        .iter()
        .collect::<String>()
        .to_ascii_lowercase();

    let mut attributes = Vec::new();
    loop {
        while chars.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        match chars.get(i) {
            None => return None, // unterminated
            Some('>') => {
                return Some((
                    Tag {
                        name,
                        closing,
                        attributes,
                    },
                    i + 1,
                ));
            }
            Some('/') => {
                i += 1;
                continue;
            }
            Some(_) => {}
        }

        let key_start = i;
        while chars
            .get(i)
            .is_some_and(|c| !c.is_whitespace() && *c != '=' && *c != '>')
        {
            i += 1;
        }
        if i == key_start {
            // Something we do not understand: step over it rather than loop.
            i += 1;
            continue;
        }
        let key: String = chars[key_start..i]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase();

        while chars.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        if chars.get(i) != Some(&'=') {
            // A valueless attribute, such as `hidden`.
            attributes.push((key, String::new()));
            continue;
        }
        i += 1;
        while chars.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        let Some(quote) = chars.get(i).copied().filter(|c| *c == '"' || *c == '\'') else {
            // Unquoted: skip to the next separator without reading a value.
            while chars
                .get(i)
                .is_some_and(|c| !c.is_whitespace() && *c != '>')
            {
                i += 1;
            }
            continue;
        };
        i += 1;
        let value_start = i;
        while chars.get(i).is_some_and(|c| *c != quote) {
            i += 1;
        }
        // An unterminated value means an unterminated tag.
        chars.get(i)?;
        let value: String = chars[value_start..i].iter().collect();
        i += 1;
        attributes.push((key, decode_entities(&value)));
    }
}

/// The handful of entities a hand-written README actually contains.
fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// What a refused image leaves behind: a note, and no link.
///
/// The destination stops being a destination. When `egui_extras`' file loader
/// was built in, a link left in place was read from disk whatever mdr had
/// decided about it; that loader is gone, and this keeps the refusal legible
/// rather than silent. The source is still named when there is no alt text to
/// name instead — it is text at that point, not something a loader can follow.
fn refused_image(alt: &str, src: &str) -> String {
    // `alt` arrives ready for Markdown — every caller has already escaped what
    // it holds — so escaping again here would show `&amp;` to a reader whose
    // alt said `&`.
    let what = if alt.trim().is_empty() {
        escape_markdown(src)
    } else {
        alt.to_string()
    };
    // The brackets are escaped: an unescaped `[…]` can still be picked up as a
    // reference link if the document happens to define one by that name.
    format!("\\[⚠ image not shown: {what}\\]")
}

/// Prepare an HTML `alt` for use as Markdown link text.
///
/// The author's words are kept, brackets included: they are escaped, not
/// dropped. The image is resolved here rather than by the later pass over
/// `![](…)`, so an alt that expression could not read is no longer a problem.
fn escape_alt(text: &str) -> String {
    escape_markdown(text)
}

/// Escape the characters that would turn HTML text into Markdown markup.
///
/// The result is read once more, by comrak, so everything that means something
/// to it has to be neutralised here.
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // `1.` and `1)` open an ordered list, so the separator after a leading run
    // of digits has to be escaped too.
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    for (i, c) in text.chars().enumerate() {
        // A block is flattened to one line before this runs, so the only line
        // start to guard is the first character — and the marker a leading
        // number would make.
        let opens_a_block = (i == 0 && matches!(c, '>' | '-' | '+' | '=' | '|'))
            || (digits > 0 && i == digits && matches!(c, '.' | ')'));
        if opens_a_block
            || matches!(
                c,
                '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '#' | '!'
            )
        {
            out.push('\\');
        }
        // `&` and `<` would start an entity or a tag on the second reading.
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            _ => out.push(c),
        }
    }
    out
}

/// A GitHub-style table, rendered here rather than by the viewer.
///
/// `egui_commonmark` draws a table as a `Frame::group` around a striped `Grid`
/// (`parsers/pulldown.rs`): no cell borders, no padding, and a header row drawn
/// exactly like any other, with each box sized to its own content rather than
/// to its column. The stylesheet gives `web` `border: 1px solid` and
/// `padding: 6px 13px` on every cell, and a header that stands out — so the
/// only way to bring the two together is to draw it.
///
/// The cells come from comrak, which has already settled what GFM means by a
/// row, a column and an alignment. Nothing here re-reads the pipe syntax.
struct MarkdownTable {
    header: Vec<Cell>,
    rows: Vec<Vec<Cell>>,
    alignments: Vec<comrak::nodes::TableAlignment>,
    /// The column count comrak derived from the header row, which is what
    /// bounds a GFM table — a longer body row is truncated, a shorter one
    /// padded.
    columns: usize,
}

/// The inline content of one cell, already flattened out of the parse tree.
type Cell = Vec<CellPiece>;

/// One run of a cell, with the formatting that applies to it.
#[derive(Clone, Default)]
struct CellPiece {
    text: String,
    code: bool,
    strong: bool,
    emph: bool,
    strikethrough: bool,
    link: Option<String>,
}

impl MarkdownTable {
    /// Read a table out of the node comrak produced for it.
    fn from_node<'a>(
        node: &'a comrak::arena_tree::Node<'a, std::cell::RefCell<comrak::nodes::Ast>>,
    ) -> Option<Self> {
        use comrak::nodes::NodeValue;

        let NodeValue::Table(table) = &node.data.borrow().value else {
            return None;
        };
        let (alignments, columns) = (table.alignments.clone(), table.num_columns);

        let mut header = Vec::new();
        let mut rows = Vec::new();
        for row in node.children() {
            let NodeValue::TableRow(is_header) = row.data.borrow().value else {
                continue;
            };
            let cells: Vec<Cell> = row
                .children()
                .map(|cell| {
                    let mut pieces = Vec::new();
                    collect_inline(cell, &CellPiece::default(), &mut pieces);
                    pieces
                })
                .collect();
            if is_header {
                header = cells;
            } else {
                rows.push(cells);
            }
        }
        Some(Self {
            header,
            rows,
            alignments,
            columns,
        })
    }
}

/// Flatten a cell's inline nodes into runs of formatted text.
fn collect_inline<'a>(
    node: &'a comrak::arena_tree::Node<'a, std::cell::RefCell<comrak::nodes::Ast>>,
    inherited: &CellPiece,
    out: &mut Vec<CellPiece>,
) {
    use comrak::nodes::NodeValue;

    let value = &node.data.borrow().value;
    let mut style = inherited.clone();
    match value {
        NodeValue::Text(text) => {
            out.push(CellPiece {
                text: text.to_string(),
                ..style
            });
            return;
        }
        NodeValue::Code(code) => {
            out.push(CellPiece {
                text: code.literal.clone(),
                code: true,
                ..style
            });
            return;
        }
        // A cell is one line, so either kind of break is a space in it.
        NodeValue::SoftBreak | NodeValue::LineBreak => {
            out.push(CellPiece {
                text: " ".to_string(),
                ..style
            });
            return;
        }
        // An image has no place in a row of text; its alt text does, and that
        // is in the children. The `title` is the tooltip, not the alt, and
        // pushing it here put it in front of the text it belongs to.
        NodeValue::Image(_) => {}
        NodeValue::Strong => style.strong = true,
        NodeValue::Emph => style.emph = true,
        // Dropping it changed what a sentence meant, not just how it looked.
        NodeValue::Strikethrough => style.strikethrough = true,
        NodeValue::Link(link) => style.link = Some(link.url.clone()),
        _ => {}
    }
    for child in node.children() {
        collect_inline(child, &style, out);
    }
}

/// Lay a cell out as one run of text.
fn cell_layout(
    cell: &[CellPiece],
    header: bool,
    palette: &crate::core::style::Palette,
) -> egui::text::LayoutJob {
    use crate::core::style::{BASE_FONT_SIZE, CODE_FONT_SCALE};
    use egui::{FontFamily, FontId, TextFormat};
    let colour = |c: crate::core::style::Rgb| egui::Color32::from_rgb(c[0], c[1], c[2]);

    let body = FontId::new(BASE_FONT_SIZE, FontFamily::Proportional);
    let mono = FontId::new(BASE_FONT_SIZE * CODE_FONT_SCALE, FontFamily::Monospace);
    // egui has no font weights, so bold is a colour here as it is everywhere
    // else in this backend.
    let plain = colour(if header { palette.strong } else { palette.fg });

    let mut job = egui::text::LayoutJob::default();
    for piece in cell {
        let colour_for = if piece.link.is_some() {
            colour(palette.link)
        } else if piece.strong {
            colour(palette.strong)
        } else {
            plain
        };
        job.append(
            &piece.text,
            0.0,
            TextFormat {
                font_id: if piece.code {
                    mono.clone()
                } else {
                    body.clone()
                },
                color: colour_for,
                background: if piece.code {
                    colour(palette.inline_code_bg)
                } else {
                    egui::Color32::TRANSPARENT
                },
                italics: piece.emph,
                underline: if piece.link.is_some() {
                    egui::Stroke::new(1.0, colour(palette.link))
                } else {
                    egui::Stroke::NONE
                },
                strikethrough: if piece.strikethrough {
                    egui::Stroke::new(1.0, colour_for)
                } else {
                    egui::Stroke::NONE
                },
                ..Default::default()
            },
        );
    }
    job
}

/// The stylesheet's `padding: 6px 13px`, in points.
const CELL_PADDING_X: f32 = 13.0;
const CELL_PADDING_Y: f32 = 6.0;

/// How wide a cell would like to be, padding included.
fn cell_width(ui: &egui::Ui, cell: &[CellPiece], palette: &crate::core::style::Palette) -> f32 {
    let galley = ui.fonts_mut(|f| f.layout_job(cell_layout(cell, false, palette)));
    galley.size().x + 2.0 * CELL_PADDING_X
}

/// Draw a whole table: bordered, padded cells on a fixed column grid.
///
/// `egui::Grid` sizes a cell to its content rather than to its column, so the
/// boxes came out ragged. The widths are measured here instead and every cell
/// in a column is given the same one, which is what makes it read as a table.
fn show_table(ui: &mut egui::Ui, table: &MarkdownTable) {
    use comrak::nodes::TableAlignment;

    let palette = if ui.visuals().dark_mode {
        &crate::core::style::DARK
    } else {
        &crate::core::style::LIGHT
    };
    let colour = |c: crate::core::style::Rgb| egui::Color32::from_rgb(c[0], c[1], c[2]);
    let columns = table.columns;
    if columns == 0 {
        return;
    }

    fn cell_at(row: &[Cell], column: usize) -> &[CellPiece] {
        row.get(column).map_or(&[][..], Vec::as_slice)
    }

    let mut widths: Vec<f32> = (0..columns)
        .map(|column| {
            std::iter::once(cell_at(&table.header, column))
                .chain(table.rows.iter().map(|row| cell_at(row, column)))
                .map(|cell| cell_width(ui, cell, palette))
                .fold(0.0_f32, f32::max)
        })
        .collect();

    // A table wider than the column is scaled to fit rather than clipped: the
    // text inside a cell then wraps, as it does in `web`. No column is allowed
    // below its own padding, or the text would have nowhere to go.
    let floor = 2.0 * CELL_PADDING_X + 1.0;
    let total: f32 = widths.iter().sum();
    let available = ui.available_width();
    if total > available && total > 0.0 {
        let ratio = available / total;
        for width in &mut widths {
            *width = (*width * ratio).max(floor);
        }
    }

    let stroke = egui::Stroke::new(1.0, colour(palette.border));
    let row_ui = |ui: &mut egui::Ui, row: &[Cell], header: bool| {
        // Every cell of a row is given the height of the tallest, so a cell that
        // wraps onto three lines does not leave its neighbours' borders short.
        let height = (0..columns)
            .map(|column| {
                let job = cell_layout(cell_at(row, column), header, palette);
                let width = widths[column] - 2.0 * CELL_PADDING_X;
                let galley = ui.fonts_mut(|f| {
                    let mut job = job;
                    job.wrap.max_width = width;
                    f.layout_job(job)
                });
                galley.size().y
            })
            .fold(0.0_f32, f32::max);

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (column, width) in widths.iter().enumerate() {
                let mut frame = egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(
                        CELL_PADDING_X as i8,
                        CELL_PADDING_Y as i8,
                    ))
                    .stroke(stroke);
                if header {
                    // egui has no font weights, so a header is set apart by its
                    // background rather than by being bold.
                    frame = frame.fill(colour(palette.code_bg));
                }
                frame.show(ui, |ui| {
                    ui.set_width(width - 2.0 * CELL_PADDING_X);
                    ui.set_min_height(height);
                    let align = match table.alignments.get(column) {
                        Some(TableAlignment::Center) => egui::Align::Center,
                        Some(TableAlignment::Right) => egui::Align::Max,
                        _ => egui::Align::Min,
                    };
                    ui.with_layout(egui::Layout::top_down(align), |ui| {
                        ui.add(egui::Label::new(cell_layout(
                            cell_at(row, column),
                            header,
                            palette,
                        )));
                    });
                });
            }
        });
    };

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        row_ui(ui, &table.header, true);
        for row in &table.rows {
            row_ui(ui, row, false);
        }
    });
}

/// Split a section into the runs the viewer renders and the tables mdr draws.
///
/// Located by parsing rather than by matching lines, so a pipe character inside
/// a fenced code block is never mistaken for a table.
fn split_tables(section: &str) -> Vec<Segment<'_>> {
    use comrak::nodes::NodeValue;
    use comrak::{Arena, Options, parse_document};

    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;

    let root = parse_document(&arena, section, &options);
    let mut ranges: Vec<(usize, usize, MarkdownTable)> = Vec::new();
    for node in root.children() {
        let data = node.data.borrow();
        if matches!(data.value, NodeValue::Table(_)) {
            drop(data);
            if let Some(table) = MarkdownTable::from_node(node) {
                let data = node.data.borrow();
                ranges.push((data.sourcepos.start.line, data.sourcepos.end.line, table));
            }
        }
    }
    if ranges.is_empty() {
        return vec![Segment::Markdown(section)];
    }

    // Line offsets, so each run can be handed back as a slice of the original
    // rather than a copy.
    let mut starts = Vec::with_capacity(section.lines().count() + 1);
    let mut at = 0usize;
    for line in section.split_inclusive('\n') {
        starts.push(at);
        at += line.len();
    }
    starts.push(section.len());

    let line_start = |line: usize| starts.get(line - 1).copied().unwrap_or(section.len());
    let line_end = |line: usize| starts.get(line).copied().unwrap_or(section.len());

    let mut segments = Vec::new();
    let mut cursor = 0usize;
    for (start, end, table) in ranges {
        let from = line_start(start);
        let to = line_end(end);
        if from > cursor && !section[cursor..from].trim().is_empty() {
            segments.push(Segment::Markdown(&section[cursor..from]));
        }
        segments.push(Segment::Table(table));
        cursor = to;
    }
    if cursor < section.len() && !section[cursor..].trim().is_empty() {
        segments.push(Segment::Markdown(&section[cursor..]));
    }
    segments
}

/// A run of a section, and who draws it.
enum Segment<'a> {
    Markdown(&'a str),
    Table(MarkdownTable),
}

/// The widest the document column is drawn, in points.
///
/// The same 900 the stylesheet gives `web`.
const CONTENT_WIDTH: f32 = 900.0;

/// A viewer configured the same way everywhere it is used.
///
/// The syntax themes are the pair the terminal backend uses, so a code block
/// comes out in the same colours in `gui` and `tui`. With
/// `better_syntax_highlighting` the crate takes the block's background from the
/// syntect theme rather than from `Visuals::extreme_bg_color`, so naming the
/// theme is how that background is chosen.
///
/// There is no image width here on purpose: `max_image_width` reads as a cap
/// and is a floor — `CommonMarkOptions::max_width` returns
/// `max_image_width.max(available_width)` — so it can never make an image
/// smaller. An image declared at a width in the source is resized when it is
/// rasterised instead; see `html_to_markdown`.
fn viewer<'a>() -> CommonMarkViewer<'a> {
    CommonMarkViewer::new()
        .syntax_theme_dark("base16-ocean.dark")
        .syntax_theme_light("InspiredGitHub")
}

/// Split a section into the heading that opens it and the rest, when `web`
/// would draw a rule under that heading.
///
/// Only `h1` and `h2` get one, matching the stylesheet. A section that does not
/// open with one — the preamble, or a deeper heading — comes back as `None` and
/// is rendered in one piece.
///
/// Both spellings count: `# Title` and a title underlined with `===` or `---`.
/// `split_by_headings` already opens a section on either, so recognising only
/// the first left the other without its rule.
fn underlined_heading(section: &str) -> Option<(&str, &str)> {
    use comrak::nodes::NodeValue;
    use comrak::{Arena, Options, parse_document};

    // Asked of the parser, not of the lines. Both spellings count — `# Title`
    // and a title underlined with `===` or `---` — and a fenced block whose
    // first line happens to be dashes is not one of them, which is exactly what
    // a line-by-line reader got wrong.
    let arena = Arena::new();
    let root = parse_document(&arena, section, &Options::default());
    let first = root.first_child()?;
    let data = first.data.borrow();
    let NodeValue::Heading(heading) = data.value else {
        return None;
    };
    // Only `h1` and `h2` are underlined, matching the stylesheet.
    if heading.level > 2 {
        return None;
    }

    // `sourcepos` lines are 1-based and inclusive.
    let end_line = data.sourcepos.end.line;
    let mut offset = 0usize;
    for (n, line) in section.split_inclusive('\n').enumerate() {
        offset += line.len();
        if n + 1 == end_line {
            let head = section[..offset].trim_end_matches('\n');
            return Some((head, &section[offset..]));
        }
    }
    None
}

/// Flip the colour scheme the window is currently drawn in.
///
/// Reads the *resolved* theme rather than the preference, so one press flips
/// whatever the reader is looking at — whether it came from the desktop or from
/// `--theme`. This is the same contract as the `web` backend's toggle.
fn toggle_theme(ctx: &egui::Context) {
    ctx.set_theme(match ctx.theme() {
        egui::Theme::Dark => egui::ThemePreference::Light,
        egui::Theme::Light => egui::ThemePreference::Dark,
    });
}

/// Tell egui which of the two palettes to draw with.
///
/// Installing both and never choosing is what made `--theme light` a no-op
/// here: eframe stays on `ThemePreference::System`, so the OS had the last word
/// whatever the flag said.
///
/// `Auto` writes `System` rather than leaving the preference alone, so the
/// setting is always stated rather than inherited from whatever the context
/// happened to hold. It runs once per document rather than once per frame, so
/// it never fights `t`.
fn apply_theme_preference(ctx: &egui::Context, setting: crate::core::Theme) {
    ctx.set_theme(match setting {
        crate::core::Theme::Dark => egui::ThemePreference::Dark,
        crate::core::Theme::Light => egui::ThemePreference::Light,
        crate::core::Theme::Auto => egui::ThemePreference::System,
    });
}

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

    apply_theme_preference(ctx, crate::core::theme());

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
    // A piped document lives in a temp file; its images do not.
    let base_dir = crate::core::document_base_dir(&canonical_file);
    let raw_markdown = std::fs::read_to_string(&file_path)
        .unwrap_or_else(|e| format!("# Error\nCould not read `{}`: {}", file_path.display(), e));

    let markdown = preprocess_mermaid_for_egui(&raw_markdown);
    // Before the image paths are resolved, so an `<img>` is rewritten into the
    // `![](…)` the resolver understands and takes the same route as any other.
    let markdown = render_simple_html(&markdown, &base_dir);
    let markdown = resolve_local_image_paths(&markdown, &base_dir);
    // The TOC and the sections are both derived from the *rendered* markdown,
    // so a preprocessing step can never shift one against the other (#57).
    let toc_entries = toc::extract_toc(&markdown);
    let (has_preamble, sections) = split_by_headings(&markdown);

    let watch = crate::core::watcher::watch_file(&file_path)?;

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
                watch,
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
    ToggleTheme,
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
        Key::T => Some(Action::ToggleTheme),
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
    /// Kept for its lifetime, not only its channel: dropping it stops the watch.
    watch: Watch,
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
        if self.watch.changes().try_recv().is_ok() {
            while self.watch.changes().try_recv().is_ok() {}
            if let Ok(content) = std::fs::read_to_string(&self.file_path) {
                self.markdown = preprocess_mermaid_for_egui(&content);
                self.markdown = render_simple_html(&self.markdown, &self.base_dir);
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
                Action::ToggleTheme => toggle_theme(&ctx),
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
                    let colour = |c: style::Rgb| egui::Color32::from_rgb(c[0], c[1], c[2]);
                    let muted = colour(palette.muted);
                    let fg = colour(palette.fg);

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
                            // A long entry wraps, as it does in the `web`
                            // sidebar. Truncating it to an ellipsis hid exactly
                            // the words that tell two sibling sections apart.
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                            for (i, entry) in self.toc_entries.iter().enumerate() {
                                let indent = ((f32::from(entry.level) - 1.0) * 12.0).max(0.0);
                                ui.horizontal_top(|ui| {
                                    ui.add_space(indent);
                                    // Depth shows in the size and the indent, and
                                    // never in the colour: `RichText::strong()`
                                    // sets a colour of its own, which overrode the
                                    // link colour — so the top two levels came out
                                    // as plain white text and the third as a blue
                                    // link, in one list. Every entry is given the
                                    // same colour explicitly, and the deepest ones
                                    // are muted the way the `web` sidebar mutes
                                    // them.
                                    let text = egui::RichText::new(&entry.text);
                                    let text = match entry.level {
                                        1 => text.color(fg),
                                        2 | 3 => text.size(BASE_FONT_SIZE * 0.875).color(fg),
                                        _ => text.size(BASE_FONT_SIZE * 0.8125).color(muted),
                                    };
                                    if ui.link(text).clicked() {
                                        // Map TOC index to section index
                                        let section_idx = if has_preamble { i + 1 } else { i };
                                        *scroll_target = Some(section_idx);
                                    }
                                });
                                ui.add_space(2.0);
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
                // A column, not the whole window. The stylesheet caps `web` at
                // 900 px for the reason every book has margins: a line that
                // runs the width of a wide monitor is hard to come back from at
                // the end of it. Without this a code block was stretched to the
                // window and prose ran edge to edge.
                ui.set_max_width(CONTENT_WIDTH.min(ui.available_width()));
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
                        // `web` draws a rule under `h1` and `h2`
                        // (`border-bottom` in the stylesheet). The viewer has no
                        // hook for it, but a section always opens with its own
                        // heading — so the heading is rendered on its own, the
                        // rule is drawn, and the body follows.
                        let cache = &mut self.caches[i];
                        let body = match underlined_heading(section) {
                            Some((heading, body)) => {
                                viewer().show(ui, cache, heading);
                                ui.add_space(2.0);
                                ui.separator();
                                ui.add_space(2.0);
                                body
                            }
                            None => section,
                        };
                        for segment in split_tables(body) {
                            match segment {
                                Segment::Markdown(text) => {
                                    viewer().show(ui, cache, text);
                                }
                                Segment::Table(table) => show_table(ui, &table),
                            }
                        }
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
    rewrite_image_sized(alt, src, original, base_dir, None, fetch_remote)
}

/// Turn a `data:image/svg+xml` URI into a PNG one; leave anything else alone.
///
/// A remote badge is usually an SVG, and a document may embed one directly.
/// Both used to be handed to `egui_extras`' SVG loader, which builds its own
/// usvg options — so the resolver that refuses an SVG's file references did not
/// apply to them. Rasterising here means that loader is no longer needed.
fn raster_data_uri(data_uri: &str, width: Option<f32>) -> String {
    use base64::Engine;

    let Some(rest) = data_uri.strip_prefix("data:image/svg+xml") else {
        return data_uri.to_string();
    };
    let Some(payload) = rest.strip_prefix(";base64,") else {
        return data_uri.to_string();
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload) else {
        return data_uri.to_string();
    };
    let Ok(svg) = String::from_utf8(bytes) else {
        return data_uri.to_string();
    };
    // A drawing that cannot be rasterised is left as it was: it will simply not
    // be displayed, which is the same outcome as before and never a file read.
    rasterize_svg_data(&svg, width).unwrap_or_else(|_| data_uri.to_string())
}

/// [`rewrite_image`], with a width for the vector images that declare one.
///
/// The width arrives from HTML, which has one and Markdown does not. It changes
/// only the size the drawing is rasterised at: every check on the way — remote
/// or local, inside the image root, a real image of the type its name claims —
/// is the one an ordinary `![](…)` goes through, and that is the point of
/// routing it here rather than rasterising it on the side.
fn rewrite_image_sized(
    alt: &str,
    src: &str,
    original: &str,
    base_dir: &std::path::Path,
    width: Option<f32>,
    fetch_remote: &dyn Fn(&str) -> Option<String>,
) -> String {
    // #60: remote images are downloaded and inlined as `data:` URIs, which
    // egui_commonmark renders through its own data-URL loader
    // (`egui_commonmark_backend/src/data_url_loader.rs`, pulled in by the
    // `embedded_image` feature).
    if crate::core::net::is_remote_url(src) {
        return match fetch_remote(src) {
            Some(data_uri) => format!("![{alt}]({})", raster_data_uri(&data_uri, width)),
            None => original.to_string(),
        };
    }
    if src.starts_with("data:") {
        // Including one the document wrote itself.
        return format!("![{alt}]({})", raster_data_uri(src, width));
    }
    if src.starts_with("file://") {
        return original.to_string();
    }

    let abs_path = base_dir.join(src);
    // #61: images may live anywhere inside the enclosing project, not only next
    // to the Markdown file — but never outside of it.
    if !crate::core::paths::is_within_image_root(&abs_path, base_dir) {
        // Refused, and it has to stop being an image link. Leaving the original
        // in place does not refuse anything here: `egui_commonmark` installs
        // `egui_extras`' loaders, which prefix a schemeless destination with
        // `file://` and read it off disk without asking mdr. The refusal is
        // only a refusal if the path never reaches them.
        return refused_image(alt, src);
    }
    if !abs_path.exists() {
        return refused_image(alt, src);
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
        if let Ok(data_uri) = rasterize_svg_at(&abs_path, width) {
            return format!("![{alt}]({data_uri})");
        }
        // No fallback to the file as a `data:image/svg+xml`: nothing renders
        // SVG in this window any more, on purpose, and a drawing mdr could not
        // rasterise is one it could not check either.
        return refused_image(alt, src);
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
/// Rasterise an SVG, optionally to an exact width in points.
///
/// Without a width the drawing is rendered at twice its own size, which is
/// sharp on a high-density display. With one it is rendered at exactly that
/// width instead: the viewer sizes an image from its pixels, and there is no
/// way to tell it that a bitmap is meant to be drawn at half its resolution —
/// so a doubled logo would simply be a logo twice the size the document asked
/// for.
fn rasterize_svg_at(
    path: &std::path::Path,
    target_width: Option<f32>,
) -> Result<String, Box<dyn std::error::Error>> {
    rasterize_svg_data(&std::fs::read_to_string(path)?, target_width)
}

/// The same, for an SVG mdr already holds rather than one on disk.
///
/// Every SVG reaching the window goes through here, including the ones that
/// arrive as `data:` URIs — a remote badge, or one written into the document.
/// They used to be handed to `egui_extras`' SVG loader, which builds its own
/// usvg options and so did not have the resolver that refuses an SVG's file
/// references. Turning them into PNG here means that loader is not needed at
/// all, and mdr decides what every drawing is allowed to reach.
fn rasterize_svg_data(
    svg_data: &str,
    target_width: Option<f32>,
) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;

    const MAX_DIM: f32 = 8192.0;

    // Reject data that isn't actually SVG (e.g. an HTML page saved as `.svg`)
    let trimmed = svg_data.trim_start();
    if (!trimmed.starts_with('<')
        || trimmed.starts_with("<!DOCTYPE html")
        || trimmed.starts_with("<html"))
        && !trimmed.contains("<svg")
    {
        return Err("File is not a valid SVG (possibly an HTML page)".into());
    }

    // Shared, so the resolver that refuses an SVG's own file
    // references is the one every rasteriser uses.
    let options = crate::core::svg::options();
    let tree = usvg::Tree::from_str(svg_data, &options)?;
    let size = tree.size();
    let svg_w = size.width();
    let svg_h = size.height();

    if svg_w <= 0.0 || svg_h <= 0.0 {
        return Err("SVG has zero dimensions".into());
    }

    // Scale 2x for retina, but cap at MAX_DIM
    let ideal_scale = target_width.map_or(2.0_f32, |w| w / svg_w);
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

    /// The single table in `section`, as the renderer sees it.
    fn table_of(section: &str) -> MarkdownTable {
        for segment in split_tables(section) {
            if let Segment::Table(table) = segment {
                return table;
            }
        }
        panic!("no table in {section:?}");
    }

    /// A cell's text, with the formatting dropped.
    fn cell_text(cell: &[CellPiece]) -> String {
        cell.iter().map(|p| p.text.as_str()).collect()
    }

    #[test]
    fn ordinary_text_in_a_cell_is_left_alone() {
        // The hand-written scanner this replaced stripped every `_` and every
        // `*`, so `foo_bar` came out as `foobar` and an escaped `\*literal\*`
        // was rendered as emphasis. comrak decides what is a marker.
        let table = table_of("| a | b |\n|---|---|\n| foo_bar | \\*literal\\* |\n");
        assert_eq!(cell_text(&table.rows[0][0]), "foo_bar");
        assert_eq!(cell_text(&table.rows[0][1]), "*literal*");
        assert!(
            !table.rows[0][1].iter().any(|p| p.emph),
            "an escaped asterisk is not emphasis"
        );
    }

    #[test]
    fn a_link_keeps_its_label_and_its_destination() {
        // The old reader cut at the first `)`, so `docs/a(b).md` left `.md)`
        // showing, and the destination was thrown away entirely.
        let table = table_of("| a |\n|---|\n| [guide](docs/a(b).md) |\n");
        let cell = &table.rows[0][0];
        assert_eq!(cell_text(cell), "guide");
        assert_eq!(
            cell.iter().find_map(|p| p.link.clone()),
            Some("docs/a(b).md".to_string())
        );
    }

    #[test]
    fn an_empty_cell_at_the_edge_of_a_row_is_kept() {
        // Trimming the outer pipes lost it, which shifted every cell after it
        // into the wrong column.
        let table = table_of("| a | b | c |\n|---|---|---|\n| | x | |\n");
        assert_eq!(table.columns, 3);
        assert_eq!(cell_text(&table.rows[0][0]), "");
        assert_eq!(cell_text(&table.rows[0][1]), "x");
    }

    #[test]
    fn the_column_count_comes_from_the_header() {
        // GFM bounds a table to its header: a longer body row is truncated, a
        // shorter one padded. Taking the longest row instead grew the table.
        let table = table_of("| a | b |\n|---|---|\n| 1 | 2 | 3 |\n| 4 |\n");
        assert_eq!(table.columns, 2);
    }

    #[test]
    fn the_delimiter_row_alignments_are_kept() {
        // They were discarded, so a right-aligned column of numbers came out
        // left-aligned.
        use comrak::nodes::TableAlignment;
        let table = table_of("| a | b | c |\n|:--|:-:|--:|\n| 1 | 2 | 3 |\n");
        assert_eq!(
            table.alignments,
            vec![
                TableAlignment::Left,
                TableAlignment::Center,
                TableAlignment::Right
            ]
        );
    }

    #[test]
    fn a_cell_keeps_its_strikethrough() {
        // Losing it changed what a sentence meant: "~~not~~ supported" read as
        // "not supported".
        let table = table_of("| a |\n|---|\n| ~~not~~ supported |\n");
        let cell = &table.rows[0][0];
        assert_eq!(cell_text(cell), "not supported");
        assert!(
            cell.iter().any(|p| p.strikethrough && p.text == "not"),
            "only the struck run should be struck: {:?}",
            cell.iter()
                .map(|p| (&p.text, p.strikethrough))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_cell_keeps_its_inline_code_and_emphasis() {
        let table = table_of("| a |\n|---|\n| **`gui`** (default) |\n");
        let cell = &table.rows[0][0];
        assert_eq!(cell_text(cell), "gui (default)");
        assert!(
            cell.iter().any(|p| p.code && p.strong),
            "the code span is inside the bold: {:?}",
            cell.iter()
                .map(|p| (&p.text, p.code, p.strong))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_pipe_inside_a_code_block_is_not_a_table() {
        // The reason tables are located by parsing rather than by matching
        // lines.
        let section = "Text\n\n```sh\n| a | b |\n|---|---|\n```\n";
        let segments = split_tables(section);
        assert_eq!(segments.len(), 1);
        assert!(matches!(segments[0], Segment::Markdown(_)));
    }

    #[test]
    fn a_table_is_split_out_of_the_prose_around_it() {
        let section = "Before\n\n| a | b |\n|---|---|\n| c | d |\n\nAfter\n";
        let kinds: Vec<&str> = split_tables(section)
            .iter()
            .map(|s| match s {
                Segment::Markdown(_) => "markdown",
                Segment::Table(_) => "table",
            })
            .collect();
        assert_eq!(kinds, ["markdown", "table", "markdown"]);
    }

    fn html(markdown: &str) -> String {
        render_simple_html(markdown, std::path::Path::new("/nonexistent"))
    }

    /// A directory holding one real PNG, so an image can actually resolve.
    ///
    /// `b.png` exists and `a.png` does not, which is what tells the two apart
    /// in the tests that check *which* source was read.
    fn with_one_image() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("b.png"),
            [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00],
        )
        .unwrap();
        dir
    }

    #[test]
    fn both_spellings_of_a_heading_get_their_rule() {
        assert_eq!(
            underlined_heading("# Title\n\nBody\n"),
            Some(("# Title", "\nBody\n"))
        );
        assert_eq!(
            underlined_heading("## Title\n\nBody\n"),
            Some(("## Title", "\nBody\n"))
        );
        // Setext: `split_by_headings` opens a section on these too, so leaving
        // them out gave the same document two kinds of heading.
        assert_eq!(
            underlined_heading("Title\n=====\n\nBody\n"),
            Some(("Title\n=====", "\nBody\n"))
        );
        assert_eq!(
            underlined_heading("Title\n-----\n\nBody\n"),
            Some(("Title\n-----", "\nBody\n"))
        );
    }

    #[test]
    fn a_fence_that_looks_like_a_setext_heading_is_not_one() {
        // A line-by-line reader saw "first line, then only dashes" and split
        // the fence in two, which changed how the code came out.
        let section = "```text\n---\ncontent\n```\n";
        assert_eq!(underlined_heading(section), None);
        // A three-line title is a real Setext heading and the reader missed it.
        let section = "A long\ntitle\n======\n\nBody\n";
        let (head, body) = underlined_heading(section).expect("a Setext heading");
        assert_eq!(head, "A long\ntitle\n======");
        assert_eq!(body, "\nBody\n");
    }

    #[test]
    fn a_deeper_heading_and_a_preamble_get_no_rule() {
        // The stylesheet only underlines `h1` and `h2`.
        assert_eq!(underlined_heading("### Title\n\nBody\n"), None);
        assert_eq!(underlined_heading("Just prose\n\nmore\n"), None);
        // `#hashtag` is not a heading.
        assert_eq!(underlined_heading("#nothashtag\n\nBody\n"), None);
    }

    #[test]
    fn an_html_heading_becomes_a_markdown_heading() {
        // A README that centres its title with `<h1>` showed that markup at the
        // top of the window, because the viewer passes an HTML block through as
        // text.
        let out = html("<h1 align=\"center\">mdr — Markdown Reader</h1>\n");
        assert_eq!(out.trim(), "# mdr — Markdown Reader");
    }

    #[test]
    fn an_html_image_becomes_a_markdown_image() {
        let out = render_simple_html(
            "<p align=\"center\">\n  <img src=\"b.png\" alt=\"mdr logo\"/>\n</p>\n",
            with_one_image().path(),
        );
        assert!(out.starts_with("![mdr logo](data:image/png"), "got {out}");
    }

    #[test]
    fn an_html_paragraph_keeps_its_text() {
        let out = html("<p align=\"center\">\n  A fast Markdown viewer.\n</p>\n");
        assert_eq!(out.trim(), "A fast Markdown viewer.");
    }

    #[test]
    fn html_inside_a_code_block_is_left_alone() {
        // The whole reason blocks are located by parsing rather than by
        // matching lines: this is code, and it has to stay the code it was
        // written as.
        let source = "Before\n\n```html\n<h1 align=\"center\">Not a heading</h1>\n```\n\nAfter\n";
        assert_eq!(html(source).trim(), source.trim());
    }

    #[test]
    fn an_unhandled_tag_keeps_what_it_wrapped() {
        // Worse than a browser, better than showing angle brackets to a reader.
        let out = html("<div><span class=\"x\">kept</span></div>\n");
        assert_eq!(out.trim(), "kept");
    }

    /// The text comrak finds once the converted Markdown is read back.
    ///
    /// The conversion is only half the story: its output is parsed a second
    /// time, and a test on the intermediate string would miss anything that
    /// escaping got wrong on that second reading.
    fn rendered_text(markdown: &str) -> String {
        use comrak::nodes::NodeValue;
        use comrak::{Arena, Options, parse_document};
        let arena = Arena::new();
        let root = parse_document(&arena, &html(markdown), &Options::default());
        let mut out = String::new();
        for node in root.descendants() {
            match &node.data.borrow().value {
                NodeValue::Text(text) => out.push_str(text),
                NodeValue::Code(code) => out.push_str(&code.literal),
                _ => {}
            }
        }
        out
    }

    /// Every heading comrak finds in the converted Markdown.
    fn rendered_headings(markdown: &str) -> Vec<u8> {
        use comrak::nodes::NodeValue;
        use comrak::{Arena, Options, parse_document};
        let arena = Arena::new();
        let root = parse_document(&arena, &html(markdown), &Options::default());
        root.descendants()
            .filter_map(|n| match &n.data.borrow().value {
                NodeValue::Heading(h) => Some(h.level),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn markdown_characters_in_html_text_stay_literal() {
        // What is between the tags is text. Without escaping, `<p>*a*</p>` came
        // out as emphasis and a `#` opened a heading.
        assert_eq!(
            rendered_text("<p>*not emphasis* and _not either_</p>\n"),
            "*not emphasis* and _not either_"
        );
        assert_eq!(
            rendered_text("<p>[not a link](nowhere)</p>\n"),
            "[not a link](nowhere)"
        );
    }

    #[test]
    fn a_hash_in_html_text_does_not_become_a_heading() {
        // It would also have gone into the table of contents, which is built
        // from the same converted document.
        assert_eq!(rendered_text("<p># not a heading</p>\n"), "# not a heading");
        assert!(rendered_headings("<p># not a heading</p>\n").is_empty());
        // A real HTML heading still is one.
        assert_eq!(rendered_headings("<h2>a heading</h2>\n"), vec![2]);
    }

    #[test]
    fn an_html_entity_is_decoded_exactly_once() {
        // `&amp;lt;` is the HTML for the text `&lt;`. Decoding it twice would
        // show a `<` the author did not write.
        assert_eq!(rendered_text("<p>&amp;lt;</p>\n"), "&lt;");
        assert_eq!(rendered_text("<p>Tom &amp; Jerry</p>\n"), "Tom & Jerry");
    }

    #[test]
    fn a_number_in_html_text_does_not_open_a_list() {
        // `<p>1. texte</p>` became an ordered list on the second reading.
        assert_eq!(rendered_text("<p>1. not a list</p>\n"), "1. not a list");
        assert_eq!(
            rendered_text("<p>12) not a list either</p>\n"),
            "12) not a list either"
        );
        // A number in the middle of a sentence is not a marker.
        assert_eq!(rendered_text("<p>version 1. done</p>\n"), "version 1. done");
    }

    #[test]
    fn an_svg_data_uri_becomes_a_raster_one() {
        // A remote badge is usually an SVG, and a document may embed one. Both
        // used to be handed to `egui_extras`' SVG loader, which builds its own
        // usvg options and so never had the resolver that refuses an SVG's file
        // references.
        use base64::Engine;
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="red"/></svg>"#;
        let encoded = base64::engine::general_purpose::STANDARD.encode(svg);
        let out = raster_data_uri(&format!("data:image/svg+xml;base64,{encoded}"), None);
        assert!(out.starts_with("data:image/png;base64,"), "got {out}");
    }

    #[test]
    fn a_raster_data_uri_is_left_as_it_is() {
        let png = "data:image/png;base64,iVBORw0KGgo=";
        assert_eq!(raster_data_uri(png, None), png);
        // And anything that is not a data URI at all.
        assert_eq!(
            raster_data_uri("https://example.com/a.svg", None),
            "https://example.com/a.svg"
        );
    }

    #[test]
    fn a_declared_width_resizes_the_drawing() {
        // `max_image_width` is a floor, not a cap, so the only way to honour a
        // width is to rasterise at it. Without this the logo came out at the
        // SVG's own size, filling the window.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("logo.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="600" height="600"><rect width="600" height="600" fill="red"/></svg>"#,
        )
        .unwrap();

        // The pixels, not the length of the base64: a shorter string would only
        // suggest a smaller drawing.
        fn width_of(markdown: &str) -> u32 {
            use base64::Engine;
            let start = markdown.find("base64,").expect("a data URI") + "base64,".len();
            let end = markdown[start..].find(')').expect("a closing paren") + start;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&markdown[start..end])
                .expect("valid base64");
            image::load_from_memory(&bytes).expect("a PNG").width()
        }

        let sized = render_simple_html(
            r#"<p><img src="logo.svg" alt="l" width="60"/></p>"#,
            dir.path(),
        );
        let natural = render_simple_html(r#"<p><img src="logo.svg" alt="l"/></p>"#, dir.path());

        assert_eq!(
            width_of(&sized),
            60,
            "the declared width should be honoured"
        );
        // Without one, the drawing is rendered at twice its own size for a
        // high-density display.
        assert_eq!(width_of(&natural), 1200);
    }

    #[test]
    fn an_image_in_html_goes_through_the_ordinary_resolver() {
        // The width branch used to rasterise on the side, skipping the format
        // check and the image-root check that every `![](…)` goes through.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("not-really.svg"), b"<html>nope</html>").unwrap();
        let out = render_simple_html(
            r#"<p><img src="not-really.svg" alt="x" width="180"/></p>"#,
            dir.path(),
        );
        assert!(
            !out.contains("data:image"),
            "a file that is not the image it claims must not be embedded: {out}"
        );
    }

    #[test]
    fn an_alt_keeps_the_author_s_words() {
        // Brackets are escaped, not dropped: the alt is the author's text and
        // the image is resolved here, so the later pass over `![](…)` never
        // needs to read it back.
        let dir = with_one_image();
        let out = render_simple_html(r#"<p><img src="b.png" alt="a [b] c"/></p>"#, dir.path());
        assert!(out.starts_with(r"![a \[b\] c]"), "got {out}");
    }

    #[test]
    fn a_refused_image_shows_its_alt_once_decoded() {
        // The alt reaches the refusal already escaped, so escaping it again
        // showed `&amp;` to a reader whose alt said `&`.
        let out = render_simple_html(
            r#"<p><img src="missing.png" alt="Tom &amp; Jerry"/></p>"#,
            std::path::Path::new("/nonexistent"),
        );
        assert!(out.contains("image not shown"), "got {out}");
        assert_eq!(
            rendered_text(r#"<p><img src="missing.png" alt="Tom &amp; Jerry"/></p>"#),
            "[⚠ image not shown: Tom & Jerry]"
        );
    }

    #[test]
    fn html_text_cannot_reopen_a_block() {
        // A line starting with `>` or `-` would otherwise become a quote or a
        // list item on the second reading.
        assert_eq!(rendered_text("<p>&gt; not a quote</p>\n"), "> not a quote");
        assert_eq!(rendered_text("<p>- not a list</p>\n"), "- not a list");
    }

    #[test]
    fn a_custom_element_is_not_mistaken_for_a_known_one() {
        // Reading the name only as far as the first non-alphanumeric made
        // `<h1-title>` an `h1` and `<img-icon>` an `img`.
        let chars: Vec<char> = "<h1-title>".chars().collect();
        let (tag, _) = parse_tag(&chars).expect("a tag");
        assert_eq!(tag.name, "h1-title");

        // And through the conversion itself. The `<div>` is what makes comrak
        // treat this as an HTML *block*: without it the line stays a paragraph
        // with inline HTML, and `html_to_markdown` is never reached — so the
        // test would pass whatever `parse_tag` did.
        let source = "<div><h1-title>not a heading</h1-title></div>\n";
        assert_eq!(rendered_headings(source), Vec::<u8>::new());
        assert_eq!(rendered_text(source), "not a heading");
        assert_eq!(rendered_headings("<h1>a heading</h1>\n"), vec![1]);
    }

    #[test]
    fn an_attribute_is_read_as_an_attribute_not_searched_for() {
        // Looking for the name anywhere in the tag picked `a.png` out of this,
        // because it appears inside another attribute's value.
        // `a.png` does not exist and `b.png` does, so reading the wrong one
        // shows up as a refusal rather than an embedded image.
        let dir = with_one_image();
        let out = render_simple_html(
            r#"<p><img title="old src='a.png'" src="b.png" alt="x"/></p>"#,
            dir.path(),
        );
        assert!(out.contains("data:image/png"), "got {out}");
    }

    #[test]
    fn a_greater_than_inside_a_value_does_not_end_the_tag() {
        // Cutting at the first `>` ended the tag in the middle of the value.
        let dir = with_one_image();
        let out = render_simple_html(r#"<p><img alt="a > b" src="b.png"/></p>"#, dir.path());
        assert!(
            out.contains("data:image/png"),
            "the tag should have been read whole: {out}"
        );
        assert!(
            out.starts_with("![a > b]"),
            "the alt should be intact: {out}"
        );
    }

    #[test]
    fn an_html_block_inside_a_quote_is_left_alone() {
        // The replacement works on whole lines and cannot carry back the `>`
        // of the quote, so a nested block is not converted at all.
        let source = "> <div>quoted</div>\n";
        assert_eq!(html(source), source);
    }

    #[test]
    fn a_document_without_html_is_returned_unchanged() {
        let source = "# Title\n\nSome *text* and `code`.\n";
        assert_eq!(html(source), source);
    }

    /// A fixture font file. The bytes are never parsed — `FontData` only holds
    /// them, and nothing here builds the atlas — so what matters is the size.
    fn fixture(dir: &std::path::Path, name: &str, bytes: usize, index: u32) -> Candidate {
        let path = dir.join(name);
        std::fs::write(&path, vec![0_u8; bytes]).unwrap();
        Candidate {
            rank: 0,
            family: name.to_string(),
            path,
            index,
        }
    }

    #[test]
    fn the_font_budget_stops_reading_once_the_byte_ceiling_is_reached() {
        let dir = tempfile::tempdir().unwrap();
        let small = fixture(dir.path(), "small", 1_000, 0);
        let huge = fixture(dir.path(), "huge", 10_000, 0);
        let budget = FontBudget {
            bytes: 5_000,
            faces: 8,
        };

        let fonts = build_font_definitions(&[(&small, Role::Ui), (&huge, Role::Fallback)], &budget);

        let keys: Vec<&String> = fonts.font_data.keys().collect();
        assert!(
            keys.iter().any(|k| k.contains("small")),
            "the face that fits must be loaded: {keys:?}"
        );
        assert!(
            !keys.iter().any(|k| k.contains("huge")),
            "a face over the remaining budget must be skipped: {keys:?}"
        );
    }

    #[test]
    fn a_face_over_budget_does_not_block_the_ones_behind_it() {
        // The order is by preference, so a single large font early in the list
        // must not cost every fallback behind it.
        let dir = tempfile::tempdir().unwrap();
        let huge = fixture(dir.path(), "huge", 10_000, 0);
        let small = fixture(dir.path(), "small", 1_000, 0);
        let budget = FontBudget {
            bytes: 5_000,
            faces: 8,
        };

        let fonts = build_font_definitions(
            &[(&huge, Role::Fallback), (&small, Role::Fallback)],
            &budget,
        );

        assert!(
            fonts.font_data.keys().any(|k| k.contains("small")),
            "the loader should carry on past a face it cannot afford"
        );
    }

    #[test]
    fn the_font_budget_caps_how_many_faces_are_added() {
        let dir = tempfile::tempdir().unwrap();
        let faces: Vec<Candidate> = (0..5)
            .map(|i| fixture(dir.path(), &format!("face{i}"), 10, 0))
            .collect();
        let budget = FontBudget {
            bytes: 1_000_000,
            faces: 2,
        };

        let candidates: Vec<(&Candidate, Role)> =
            faces.iter().map(|c| (c, Role::Fallback)).collect();
        let fonts = build_font_definitions(&candidates, &budget);

        let added = fonts
            .font_data
            .keys()
            .filter(|k| k.contains("face"))
            .count();
        assert_eq!(added, 2, "the face count is a ceiling, not a suggestion");
    }

    #[test]
    fn nothing_is_read_when_the_budget_admits_nothing() {
        // Belt and braces on the selection order: a zero budget must leave
        // egui's embedded fonts in place rather than produce an empty page.
        let dir = tempfile::tempdir().unwrap();
        let face = fixture(dir.path(), "face", 10, 0);
        let budget = FontBudget { bytes: 0, faces: 0 };

        let fonts = build_font_definitions(&[(&face, Role::Ui)], &budget);

        assert!(
            !fonts.font_data.keys().any(|k| k.contains("face")),
            "no system face should have been read"
        );
        assert!(
            !fonts.font_data.is_empty(),
            "egui's own fonts must survive, or there is nothing left to draw with"
        );
    }

    #[test]
    fn a_face_inside_a_collection_keeps_its_index() {
        // `FontData::from_owned` always says index 0. For a `.ttc` that is a
        // different face than the one selected, so the index has to be put back
        // or the wrong face is drawn.
        let dir = tempfile::tempdir().unwrap();
        let face = fixture(dir.path(), "collection.ttc", 100, 3);
        let budget = FontBudget {
            bytes: 1_000,
            faces: 8,
        };

        let fonts = build_font_definitions(&[(&face, Role::Ui)], &budget);

        let (_, data) = fonts
            .font_data
            .iter()
            .find(|(k, _)| k.contains("collection"))
            .expect("the face should have been loaded");
        assert_eq!(
            data.index, 3,
            "the face index inside the collection is lost"
        );
    }

    #[test]
    fn two_faces_of_one_family_do_not_overwrite_each_other() {
        // The defect this keying replaced: two files of the same family both
        // report index 0, so a family-only key made the second evict the first
        // and the body font became whichever loaded last.
        let dir = tempfile::tempdir().unwrap();
        let mut regular = fixture(dir.path(), "Regular.ttf", 10, 0);
        let mut bold = fixture(dir.path(), "Bold.ttf", 10, 0);
        regular.family = "Shared".to_string();
        bold.family = "Shared".to_string();
        let budget = FontBudget {
            bytes: 1_000,
            faces: 8,
        };

        let fonts =
            build_font_definitions(&[(&regular, Role::Ui), (&bold, Role::Fallback)], &budget);

        assert_eq!(
            fonts.font_data.len(),
            egui::FontDefinitions::default().font_data.len() + 2,
            "both faces of the family must survive"
        );
    }

    #[test]
    fn the_chosen_faces_lead_their_family_and_fall_back_on_egui() {
        let dir = tempfile::tempdir().unwrap();
        let ui = fixture(dir.path(), "ui", 10, 0);
        let mono = fixture(dir.path(), "mono", 10, 0);
        let budget = FontBudget {
            bytes: 1_000,
            faces: 8,
        };

        let fonts = build_font_definitions(&[(&ui, Role::Ui), (&mono, Role::Mono)], &budget);

        let defaults = egui::FontDefinitions::default();
        for (family, leader) in [
            (egui::FontFamily::Proportional, "ui"),
            (egui::FontFamily::Monospace, "mono"),
        ] {
            let chain = &fonts.families[&family];
            assert!(
                chain[0].contains(leader),
                "the {leader} font must lead the {family:?} chain: {chain:?}"
            );
            // Naming the embedded keys rather than counting: two system faces
            // would satisfy a length check on their own, and the point here is
            // that egui's own fonts are still reachable behind them.
            let embedded = &defaults.families[&family];
            let kept: Vec<&String> = chain.iter().filter(|k| embedded.contains(k)).collect();
            assert_eq!(
                kept,
                embedded.iter().collect::<Vec<_>>(),
                "egui's embedded fallbacks must all survive, in order: {chain:?}"
            );
        }
    }

    #[test]
    fn a_primary_that_does_not_fit_falls_through_to_the_next_choice() {
        // The lists are ranked, so the best face is tried first — but it can be
        // unreadable or larger than the budget, and then the second has to get
        // its turn. Reducing each role to a single candidate before reading
        // would have made that impossible.
        let dir = tempfile::tempdir().unwrap();
        let first = fixture(dir.path(), "first-choice", 10_000, 0);
        let second = fixture(dir.path(), "second-choice", 100, 0);
        let budget = FontBudget {
            bytes: 5_000,
            faces: 8,
        };

        let fonts = build_font_definitions(&[(&first, Role::Ui), (&second, Role::Ui)], &budget);

        let leader = &fonts.families[&egui::FontFamily::Proportional][0];
        assert!(
            leader.contains("second-choice"),
            "the next choice should have taken the role: {leader}"
        );
    }

    #[test]
    fn one_face_serving_two_roles_is_read_once() {
        // A family can sit in both preference lists. Reading it twice would
        // spend the budget twice and queue it twice in each chain, for a single
        // entry that the second insert would simply overwrite.
        let dir = tempfile::tempdir().unwrap();
        let shared = fixture(dir.path(), "shared", 100, 0);
        let budget = FontBudget {
            bytes: 1_000,
            faces: 1,
        };

        let fonts = build_font_definitions(&[(&shared, Role::Ui), (&shared, Role::Mono)], &budget);

        let proportional = &fonts.families[&egui::FontFamily::Proportional];
        let monospace = &fonts.families[&egui::FontFamily::Monospace];
        assert!(
            proportional[0].contains("shared") && monospace[0].contains("shared"),
            "the one face should lead both chains: {proportional:?} / {monospace:?}"
        );
        assert_eq!(
            proportional.iter().filter(|k| k.contains("shared")).count(),
            1,
            "it should appear once in the chain, not once per role: {proportional:?}"
        );
        // A budget of one face proves it was not counted twice: a second charge
        // would have left the monospace role unfilled.
        assert_eq!(
            fonts
                .font_data
                .keys()
                .filter(|k| k.contains("shared"))
                .count(),
            1
        );
    }

    /// Tell the context what the desktop's colour scheme is.
    ///
    /// `Options::begin_pass` is the same call eframe's integration makes at the
    /// start of a pass, and it is the only thing that writes `system_theme`.
    /// Going through it sets the real field rather than a stand-in, without
    /// running a pass — `end_pass` wants a texture allocator, which a unit test
    /// has no business standing up.
    fn set_system_theme(ctx: &egui::Context, theme: egui::Theme) {
        let input = egui::RawInput {
            system_theme: Some(theme),
            ..Default::default()
        };
        ctx.options_mut(|o| o.begin_pass(&input));
    }

    #[test]
    fn a_forced_theme_outranks_the_system_one() {
        // The defect behind this: both palettes were installed and neither was
        // ever selected, so eframe stayed on `ThemePreference::System` and
        // `--theme light` changed nothing at all in this backend.
        for (setting, expected) in [
            (crate::core::Theme::Light, egui::Theme::Light),
            (crate::core::Theme::Dark, egui::Theme::Dark),
        ] {
            for system in [egui::Theme::Dark, egui::Theme::Light] {
                let ctx = egui::Context::default();
                apply_theme_preference(&ctx, setting);
                set_system_theme(&ctx, system);
                assert_eq!(
                    ctx.theme(),
                    expected,
                    "{setting:?} must hold whatever the desktop says ({system:?})"
                );
            }
        }
    }

    #[test]
    fn auto_hands_the_choice_back_to_the_system() {
        // `Auto` states the setting rather than leaving whatever the context
        // already held, so a run that asks for `auto` follows the desktop even
        // if something set a preference before it.
        for system in [egui::Theme::Dark, egui::Theme::Light] {
            let ctx = egui::Context::default();
            // A preference as a previous run could have left it behind.
            ctx.set_theme(egui::ThemePreference::Dark);

            apply_theme_preference(&ctx, crate::core::Theme::Auto);
            set_system_theme(&ctx, system);

            assert_eq!(
                ctx.theme(),
                system,
                "a stored preference must not outlive a run that asked for auto"
            );
        }
    }

    #[test]
    /// `apply_style` needs a context, not a window, so the palette and the type
    /// scale can be checked without opening anything.
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
    fn t_toggles_the_theme_but_not_while_typing() {
        // A bare key, so it has to stay typable in the search field — the same
        // rule as `j`, `k` and `q`.
        assert_eq!(
            key_action(egui::Key::T, egui::Modifiers::NONE, false),
            Some(Action::ToggleTheme)
        );
        assert_eq!(key_action(egui::Key::T, egui::Modifiers::NONE, true), None);
    }

    #[test]
    fn the_toggle_flips_whatever_the_window_is_showing() {
        // Including a theme the reader forced on the command line: one press
        // should change what is on screen, not quietly go back to the desktop.
        for forced in [crate::core::Theme::Light, crate::core::Theme::Dark] {
            let ctx = egui::Context::default();
            apply_theme_preference(&ctx, forced);
            let before = ctx.theme();

            toggle_theme(&ctx);
            assert_ne!(ctx.theme(), before, "{forced:?} should have flipped");

            toggle_theme(&ctx);
            assert_eq!(ctx.theme(), before, "a second press should come back");
        }
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
        let out = rewrite_image("x", "../../secret.png", original, &docs, &never_fetch);
        // Not left as a link: `egui_commonmark` installs loaders that would read
        // the path off disk regardless of what mdr decided about it, so a
        // refusal has to stop being an image.
        assert!(!out.contains("]("), "still a link: {out}");
        assert!(!out.starts_with("!["), "still an image: {out}");
        assert!(
            !out.contains("secret.png"),
            "the path is still there: {out}"
        );
        assert!(out.contains("image not shown"), "got {out}");
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
