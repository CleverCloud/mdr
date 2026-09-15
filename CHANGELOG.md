# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.1] - 2026-09-15

### Security

- `rustls` is updated from 0.23.43 to 0.23.45, which closes
  [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285):
  TLS 1.3 handshake messages were accepted across encryption level boundaries
  (CVSS 5.3). It reaches mdr through `ureq`, which is what fetches remote
  images over HTTPS, so it sits on the only network path mdr has.

  The advisory was published on 2026-09-14, the day the 0.6.0 pull request was
  opened, so no dependency update made before the release could have carried
  the fix. Binaries are statically linked: 0.6.0 ships the vulnerable version,
  and only a new release moves users off it.

- `clap` is updated from 4.6.6 to 4.6.7 in passing.

Lockfile only — no manifest change, and the resolver stays within versions
compatible with the declared MSRV of 1.95.

## [0.6.0] - 2026-09-14

The crate moves to Rust edition 2024, the configuration file finds itself, the
`gui` backend is brought much closer to `web`, `--theme` finally reaches every
backend, and the backends are renamed.

### Changed

- **Rust edition 2024**, with 1.95 as the minimum supported version — the
  highest `rust-version` declared anywhere in the tree, and confirmed by
  building and testing on a pinned 1.95 toolchain rather than by reading
  manifests. Many crates declare none at all, which is why the build is the
  proof and the declaration only the starting point.

- **Dependencies brought up to date**: comrak 0.55, the egui stack at 0.36, wry
  0.57 and tao 0.37, resvg and usvg 0.48, mermaid-rs-renderer 0.3, base64 0.23.
  comrak's `syntect-onig` feature is now selected explicitly: from 0.53 comrak
  stopped enabling a regex engine on its own, which would have broken a
  webview-only build.

- **A selected set of clippy lints is enforced in `Cargo.toml`.** They were all
  fixed across the crate first, so the list is a floor the CI holds rather than
  a wish. `clippy::pedantic` is deliberately not switched on in bulk: the CI
  lints on a floating stable toolchain, and a lint added upstream would break
  unrelated pull requests.

- **The config file is written on first run, and its location is resolved
  rather than hard-coded.** In order, on every platform: an existing
  `~/.config/mdr/config.kdl`, then `$XDG_CONFIG_HOME/mdr/config.kdl` when that
  variable holds an absolute path, then `%APPDATA%\mdr\config.kdl` on Windows,
  then `~/.config/mdr/config.kdl`. `%USERPROFILE%` gives the home directory on
  Windows, because Git Bash sets `HOME` to a POSIX path a native binary cannot
  resolve.

  Putting an existing `~/.config/mdr/config.kdl` ahead of `XDG_CONFIG_HOME` is
  a deliberate departure from the spec, which says the variable wins: every mdr
  before this release read that one path and nothing else, so deferring to the
  variable would silently ignore the config of everyone who has both. A
  relative `XDG_CONFIG_HOME` is ignored with a warning, as the spec requires. Being unable to write
  the file is a warning, not a failure: mdr runs on the defaults the file would
  have carried anyway.

  The generated file selects `backend auto`, which every build has — it used to
  hard-code `webview`, which a `--no-default-features --features tui-backend`
  binary cannot run. When nothing names a home directory mdr still reads
  `./.config/mdr/config.kdl` if it is there — and treats it like any other
  config, including correcting an old backend name in it — but does not create
  one, so no `.config/` is left behind in the directory it was started from.

- **The backends are named `gui`, `tui` and `web`.** `egui` and
  `webview` said which library draws the window, which is not what someone
  choosing a backend is picking between: a native window, a terminal, or the
  system webview. `mdr --backend web`, `backend gui` in `config.kdl`.

  A config file written before this release is **corrected in place** the first
  time it is read: `backend egui` becomes `backend gui`, comments and every
  other setting are preserved, and the run says so once. A file mdr cannot
  write — read-only, or a symlink — is left as it is, with a warning, and the
  run carries on. On the command line there is no such mapping: `--backend egui` is simply not a
  backend any more, and the error lists the names that are.

  The Cargo features keep the crate names (`egui-backend`, `webview-backend`,
  `tui-backend`), so documented build commands still work.

### Added

- **A `LICENSE` file.** MIT was declared in `Cargo.toml`, in the README, in the
  Homebrew formula, in the Scoop manifest and in the Chocolatey `licenseUrl` —
  which pointed at a 404 — but the text was nowhere in the repository, so
  GitHub reported no licence at all. Every packaging recipe is now configured
  to install it: the `.deb` and `.rpm`, the release archives (which is where
  Homebrew and the AUR package pick it up), and the Nix build. None of those
  formats has been built from this branch.

- `-s, --set-default-backend <BACKEND>` writes the backend into the config
  file and exits. Only that value is replaced — comments and every other
  setting are preserved — and mdr refuses to write a backend this binary was
  not built with, which would only fail on the next start.
- The publishing setup documents every channel the release workflow drives:
  `PACKAGING.md` named four of the seven secrets and three of the six
  variables, leaving Chocolatey, Scoop and the Snap Store undocumented even
  though their jobs run. The README gained the `cargo install mdr` /
  `cargo binstall mdr` route, which was never written down.
- Short forms for the options that lacked them: `-t` for `--theme`, `-c` for
  `--config` and `-l` for `--list-backends`. `--theme` was also missing from the
  man page's option list, though it was already documented as a config key.

- **The `gui` and `web` backends share one palette, one body size and one h1**
  (`src/core/style.rs`): a 16 px body, headings at 2 / 1.5 / 1.25 / 1 / 0.875 /
  0.85 em, code at 85 %, GitHub colours. `gui` used to run on egui's defaults —
  a 13 pt body with an 18 pt heading, so `h1` through `h6` all landed within
  five points of each other — and `web` rendered `h3` to `h6` at the browser's
  own sizes, which were a third scale again. The stylesheet is generated from those
  constants, so the palette and the body size stay in step.

  `h2` to `h6` are the stylesheet's in `web` only. `gui` renders through
  `egui_commonmark`, which interpolates its own sizes between the heading and
  body styles; the API it exposes today takes those two ends and no table, so
  setting them makes `h1` and the body agree while the levels between come out
  larger.

  Thirteen named pairs from this palette — the document, the code backgrounds
  and the sidebar states — are checked against the WCAG 2.2 contrast minimum by
  unit tests, including muted text on a hovered sidebar entry, which
  GitHub's own `#656d76` misses at 4.4999:1, so the light palette uses `#646c75`.
  egui's default inline-code chip put body text at 3.1:1.

- **`gui` picks the platform's UI and monospace faces** (SF Pro / SF Mono,
  Segoe UI / Cascadia Mono, Cantarell / Noto Sans Mono…), matching the
  `system-ui` and `ui-monospace` stacks the stylesheet asks for, plus a short,
  bounded list of fallbacks for the scripts those two do not cover.

- **`**bold**` is visible in `gui` again.** egui has no font weights —
  `strong()` only changes the colour — so bold and body text were drawn
  identically. Strong text has its own palette entry now, with a test that they
  cannot be equal.

- **The `gui` table of contents follows the `web` sidebar**: a small uppercase
  muted label instead of a document-sized heading, and entries graded by depth.
  Its scroll area no longer shrinks to its widest entry, which is what put
  egui's floating scrollbar on top of the text (the rest of #27).

### Fixed

- **`--theme dark|light` only ever reached the terminal backend.** It was
  accepted everywhere and applied nowhere else: `gui` and `web` followed the
  desktop's colour scheme whatever the flag said, so `--theme light` on a dark
  desktop rendered dark. Both now honour it, and `auto` still asks the
  environment. The toggle — see below — continues to flip whatever the window
  is currently showing, forced theme included.

  Mermaid is the exception. A diagram rendered natively to SVG carries its own
  colours, whatever the setting; only the `web` fallback, drawn by the bundled
  Mermaid library, follows it, and only when the page loads — one already on
  screen is not recoloured by the toggle.

- **The theme toggle is now `t`, and works in all three backends.** It was
  `Ctrl/Cmd+D` in `web` only — the combination Ghostty, iTerm2 and Terminal.app
  bind to splitting a pane, and one the other two backends had no equivalent
  for. `t` carries no modifier, which is why it was chosen.

  The terminal's bottom bar offers it too. That bar was one fixed string clipped
  to the width it was given, which on an 80-column terminal cut it mid-item and
  hid everything past the search hint; it now drops whole hints instead, keeping
  the most useful ones.

- **A light syntax theme was barely legible in the terminal.** syntect picks its
  foregrounds for its own background, and the terminal's is whatever the reader
  set, so `--theme light` drew dark text straight onto a dark terminal. Code
  blocks now paint the theme's background and pad to the frame width, which
  makes the block self-contained — the same thing `gui` and `web` do with their
  code background.

- **The `gui` backend read every installed font into memory.** It loaded the
  whole of each font file, once per face that file contained, and pushed all of
  them into both fallback chains — 890 faces across 473 files on the machine
  this was measured on, 4.57 GB of font data retained before egui had built
  anything out of it, on a document of any size. Another machine has another
  font collection; the shape of the problem carries over, the number does not. Faces are now chosen from the
  font database's metadata and only the handful selected is read: the body font,
  the code font, and a short list of fallbacks for scripts those two do not
  cover, under a budget of 64 MB and eight faces. The same document now holds
  three system faces and 13 MB.

  `mdr --verbose` reports what was loaded and what it cost.

- **A document opened wherever the last one had been left.** eframe's
  `persistence` feature restores egui's memory, and a scroll area's offset is
  part of it, so the `gui` window came up scrolled — on this repository's README
  that is 3630 points down, past the logo and the title, which reads as the top
  of the document simply being absent. Every document now opens at its top.

- **`gui` came closer to `web`.** A rule under `h1` and `h2`, as the stylesheet
  draws; code blocks highlighted with the same syntax themes as the terminal;
  a table of contents graded by size and indent instead of by an accident of
  styling — `RichText::strong()` sets a colour of its own, so the top two levels
  rendered as plain text and the third as a blue link, in one list, while the
  deepest stay deliberately muted — and long entries wrap instead of being cut
  to an ellipsis.

- **`gui` printed raw HTML at the reader.** `web` hands HTML to a real engine;
  `gui` has none, and `egui_commonmark` passes an HTML block through as text, so
  a README that centres its logo and title with `<p>` and `<h1>` showed its own
  markup. A short, explicit set of tags — headings, paragraphs, images — is now
  rewritten as the Markdown that means the same thing, so it travels the
  pipeline that was already there: the image paths are resolved, the file is
  validated and the SVG rasterised exactly as for `![](…)`, and a heading is
  listed in the table of contents. A declared `width` is honoured for vector
  images. Anything outside that set keeps its text and loses its tags, and that
  text is escaped, so a `*` or a `#` an author wrote inside a tag stays the
  character it was.

  Only blocks at the top level are converted: the replacement works on whole
  lines and cannot carry back the `>` of a quote or the indent of a list item.
  This is not an HTML engine — `align="center"` has no Markdown equivalent and
  is dropped, `<br>` becomes a space, so the content comes back but its layout
  does not. A heading written in HTML therefore appears in the `gui` table of
  contents, where `web` does not list it.

- **A refused image was still read from disk in `gui`.** An image outside the
  project root was left as it was written rather than inlined, which is not the
  same as refusing it: `egui_commonmark` enables `egui_extras`' file loader,
  which gives a schemeless destination a `file://` prefix and reads it, without
  passing through mdr's resolver at all. Neither that loader nor the HTTP one is
  built in any more — the decoder is pulled in without them — so no image is
  read from disk or fetched behind mdr's back: a local or remote one reaches the
  page only through mdr's own resolver, and a `data:` URI written into the
  document is still shown as the author wrote it. A refusal also replaces the
  image with a note, so the path is no longer something a loader could read.

  For the same reason, the four SVG rasterisers now share one set of usvg
  options that refuses an `xlink:href` pointing at a file. Checking the drawing
  mdr was asked for said nothing about the files that drawing referenced. (They
  also had four copies of the system font database between them; there is one.)

  The SVG decoder is gone from the window too. It built its own options, so it
  had none of this — mdr rasterises every drawing itself instead, including the
  ones that arrive as `data:` URIs, which is what a remote badge usually is.

- **Searching for "Copy" found one match per code block.** The button sits
  inside the content so it can be positioned over its block, and the search
  walked every text node there — so mdr's own control turned up as if it were
  part of the document, highlight included. The walker skips it now.

- **The copy button said "Copied" when nothing had been copied.** Where the
  asynchronous clipboard is unavailable, the fallback goes through
  `execCommand`, which reports a refusal by returning `false` rather than by
  throwing — so the `try` around it caught nothing and the button claimed
  success anyway. All three outcomes now reach the state that matches them, and
  the hidden textarea is removed even when the attempt raises.

- **`--offline` did not hold in the terminal.** The README and the man page
  promise that it stops every network access; `tui` fetched a remote image
  anyway, because `core::offline` was not even compiled for it. It now refuses,
  and a test counts the calls to prove none is made.

- **A piped document could not find its own images.** `cat README.md | mdr`
  writes the document to a temp file, and every backend then looked for
  `assets/logo.svg` next to *that* — so a piped README showed a broken image
  where its logo should be. Relative **image** paths now resolve from the
  directory mdr was run in. That is a convention, not a deduction: nothing tells
  mdr where the file handed to `cat` came from.

- **`gui` opened at whatever size the last document had been left at.** eframe's
  `persistence` feature stores the window geometry and egui's memory in one file
  shared by every document, and mdr never asked for any of it — `MdrApp` has no
  `save`. It also explains the two defects above it: the restored scroll offset,
  and a theme preference outliving the run that set it. The feature is off, and
  `gui` and `web` now both open at the 1100x900 they have always declared.

- **Tables in `gui` were not laid out as tables.** The viewer draws one as a
  striped grid inside a frame: no cell borders, no padding, and a header row
  drawn exactly like any other, with each box sized to its own content rather
  than to its column. They are now measured and drawn here — bordered, padded
  cells on a fixed column grid, every cell of a row the height of the tallest,
  with the header set apart by its background and the delimiter row's alignments
  honoured. A cell keeps its inline code, emphasis and links, read from the same
  comrak parse as the rest of the document rather than from the pipe syntax
  again.

- **The document column is capped at 900 points in `gui`**, the width the
  stylesheet gives `web`. Prose used to run the full width of the window and a
  code block was stretched to it.

- **`web` code blocks have a copy button**, which `gui` has always had. It
  survives a live reload, and falls back to a selection where the asynchronous
  clipboard is unavailable.

- **Every document in `web` opened with an empty band above it.** The content
  box has padding, so the first element's own top margin could not collapse into
  it and was added to it instead.

- **A `.webp` file was accepted on its container alone.** `RIFF` also begins a
  WAV and an AVI; the check for the `WEBP` signature that follows it sat inside
  a branch reached only when the file did *not* start with `RIFF`, so it never
  ran.

- **Following a link in `web` left the previous document's watcher running.**
  The watcher was leaked so that it would outlive the call that started it, so
  every document opened added an OS watch that nothing could stop; a long
  browsing session accumulated them. `watch_file` now hands the caller a `Watch`
  that owns the watcher, and the web backend assigns over it — which drops the
  one it is leaving.

- **A terminal reporting no rows made the terminal backend panic**, on the
  subtraction that places the bottom bar. `script -q /dev/null mdr --backend
  tui` is one way to get there.

- **A failure between entering raw mode and the cleanup left the shell raw**,
  and on the alternate screen. The restore now happens on every way out,
  including a panic.

- **`cat doc.md | mdr --backend tui` is usable again.** The document was drawn
  and the first key press then killed it with "Failed to initialize input
  reader", leaving the terminal on the alternate screen.

  The cause is specific to macOS. With the document on stdin, crossterm falls
  back to `/dev/tty` for the keyboard — but that is a *clone* device, and the
  kernel refuses to register it with kqueue: `EVFILT_READ` returns `EINVAL`.
  mio's registration fails, the event source is never built, and crossterm
  swallows the error until the first read. mdr now reopens the real terminal
  device (`/dev/ttys004` rather than `/dev/tty`) onto stdin before starting, so
  crossterm takes its ordinary path.

- **A document taller than 65535 rows no longer wraps around.** An element's
  height was narrowed to `u16` while a wrapped paragraph is as tall as its line
  count, which nothing bounds to the terminal. Everything derived from it — the
  document height, search offsets, scrolling — went wrong past that point; a
  65545-row document reported 9.
- **The terminal backend caps the SVGs it rasterises**, like the three other
  rendering paths already did. The size declared by the document decided the
  buffer to allocate outright, so an SVG claiming 40000x20000 asked for one that
  size. It is now scaled down to fit 8192 per side, never up.
- `--help` now lists every value the parsers accept. `--theme` named none at
  all, and `--backend` left out `auto` — the one the generated config file
  selects. A test ties both lists to what the parsers take, so they cannot
  drift apart again.

### Tests

- The file watcher — live reload, used by all three backends — had no tests at
  all. It now has five, driven through real edits and through the write-then-
  rename an editor performs, with a neighbouring file that must stay ignored and
  a watch replaced by another the way following a link replaces it.
- The terminal backend is driven through a real pseudo-terminal, which is the
  only way to cover starting it, pressing a key and leaving cleanly; that is
  what catches the piped-document defect above.
- `--set-default-backend` and the config resolution are covered through the
  command line, not only through their functions. The WebP signature check is
  covered where it lives, by calling the validator with a `RIFF` container that
  is a WAV and an AVI.
- The tables and the HTML `gui` renders itself are covered against what they
  replaced: a `_` inside a word, an escaped `*`, a link whose destination holds
  parentheses, an empty cell at the edge of a row, the delimiter row's
  alignments, and — for HTML — text that would otherwise become a heading, a
  list or emphasis on the second reading.
- `--offline` is shown to make no request at all, by counting the calls to an
  injected fetch rather than by pointing at a URL that would fail anyway.

### Removed

- **`--init`.** The config file is created at startup, so the flag had nothing
  left to do. A file the program can write for itself is not worth an option,
  and needing to run it first was the only thing keeping new users from
  discovering that mdr is configurable at all.

## [0.5.1] - 2026-09-07

Both of these come from finally looking at the 0.5.0 rendering in an actual
terminal rather than at its tests.

### Added

- The terminal backend picks its syntax-highlighting palette from the terminal
  instead of always assuming a dark one. `0.5.0` hard-coded a dark theme, which
  renders as pale grey on a light background — unreadable. `auto` reads
  `COLORFGBG` and falls back to dark; `--theme dark|light`, or `theme "light"`
  in the config file, overrides it.

  `COLORFGBG` is the only signal available without asking the terminal and
  waiting for an answer, which is exactly the two-second stall #58 removed.
  Terminals that do not set it — Terminal.app and Alacritty among them — give no
  answer, which is why the explicit setting exists rather than being a nicety.

### Fixed

- The frame around a code block is closed again. Naming a language left the box
  open on the right (`┌─ rust ─────────` with no corner) while an unnamed block
  and the bottom edge were closed. Both edges now come from one helper and are
  the same width, so they cannot drift apart. This dates back well before the
  terminal rewrite.

## [0.5.0] - 2026-09-07

One change, and a large one: the terminal backend no longer has a Markdown
parser of its own.

### Changed

- **The terminal backend renders from the comrak AST** instead of its own
  line-based parser (#59). The parser recognised fifteen hard-coded prefixes,
  which is why `##### Title` printed its hashes, code blocks were a flat green,
  table cells were not aligned and footnotes never appeared: four symptoms of
  one cause. Deriving the output from the same parse the table of contents and
  the two graphical backends use removes the class of difference rather than
  the four cases.

  What that brings, beyond the four reported symptoms: `h5`/`h6`, real syntax
  highlighting (syntect, already built as part of comrak and now used
  directly), tables padded per column and honouring `:---`/`:---:`/`---:`,
  footnotes collected at the end, inline emphasis, bold, strikethrough, inline
  code and links styled rather than printed as raw markup, nested lists, task
  items, ordered lists and nested block quotes.

  Tight and loose lists are now distinguished as CommonMark defines them: a
  list written without blank lines between its items no longer gains any.

  The rewrite fits behind the existing seam, so image and mermaid extraction,
  line wrapping, search, the startup gate of #58 and table-of-contents
  scrolling are untouched — the twenty-five existing terminal tests pass
  unchanged. Three helpers of the old parser, 190 lines including its inline
  formatter, are deleted rather than left behind.

  syntect is declared directly rather than only through comrak's `syntect`
  feature: it was already compiled, so this adds no build cost and no new crate
  to the lock. It loads lazily — a document with no code block never pays for
  it. Measured here: 1.28 ms to render an eleven-line document, 26 ms for the
  first document that does contain a code block (syntax-set load included),
  2-8 ms after that. Nothing that touches the startup latency fixed in #58.

### Not verified

The terminal backend cannot be driven headlessly in this environment, so the
rendering was verified through the `Line` structures it produces and by reading
them, not by looking at a terminal.

## [0.4.0] - 2026-09-04

Keyboard, images, safety and packaging. Every backend is now fully
keyboard-driven, a document can no longer run scripts in the webview, images
resolve the way people actually lay out their repositories, and the Linux
packages install like real desktop applications.

Nineteen reported issues are closed.

### Security

- **Webview: a `<script>` written inside a Markdown document no longer runs.**
  Raw document HTML is stripped of scripts, event-handler attributes and
  `javascript:` URLs before it reaches the page, and the CSP now refuses
  navigation, framing, form submission and outbound connections. A document
  could previously exfiltrate its own text to a remote host — which matters
  because mdr is typically pointed at Markdown that was generated or
  downloaded rather than written by hand (#62).
- **Piped input is no longer world-readable.** `<temp>/mdr/stdin-*.md` was
  created with default permissions, so on a shared `/tmp` anyone on the machine
  could read it. It is now created `0600`, inside a `0700` directory, and mdr
  refuses a temp directory that is a symlink or owned by another user (#64).

### Added

- **Webview: full keyboard navigation** — quit (`Ctrl/Cmd+Q`), scrolling
  (`j`/`k`, `Space`, `g`/`G`), zoom (`Ctrl/Cmd` + `+`/`-`/`0`), table of
  contents toggle (`Ctrl/Cmd+B`), light/dark theme toggle (`Ctrl/Cmd+D`), print
  and PDF export (`Ctrl/Cmd+P`), search match navigation (`n`/`N`) and a `?`
  overlay listing every shortcut. Plus a native application menu, so `Cmd+Q`
  and `Cmd+W` behave as expected on macOS.
- **GUI: keyboard navigation to match.** `q`, `Esc`, `Ctrl/Cmd+Q` and
  `Ctrl/Cmd+W` close the window, and the content scrolls with `j`/`k`, the
  arrows, `Space`, PageUp/PageDown, `g`/`G` and Home/End (#63).
- **Webview: links behave.** `http(s)` links open in the system browser instead
  of replacing the document, links to a local `.md` file open that file in mdr,
  and `#anchor` links scroll (#55).
- **Remote images are displayed** in the GUI and webview backends. They are
  downloaded once, capped at 16 MB, cached for the lifetime of the process so
  live reload does not re-download them, and inlined as `data:` URIs — which
  keeps the strict `img-src data:` policy intact (#60).
- **`--offline`** disables every network access; remote images are then left
  unresolved. Also settable as `offline #true` in the config file.
- **`.deb` and `.rpm` install a desktop entry, an icon and a man page**, not
  just the binary (#52).
- **Linux `aarch64` binaries, `.deb` and `.rpm`**, built natively on ARM
  runners (#51).

### Fixed

- **Images in a parent directory are displayed.** The allowed root is the
  enclosing project — the nearest ancestor holding `.git`, `.hg`, `.svn` or
  `.jj` — so the usual `docs/page.md` → `![](../images/schema.png)` layout
  resolves. Traversal outside the project, and above the home directory, is
  still refused (#61).
- **Repeated headings get distinct anchors** (`setup`, `setup-1`, …), so the
  second entry of a table of contents no longer scrolls to the first section.
  The renderer and the TOC now share one anchor generator and cannot drift
  apart (#65).
- **YAML front matter is treated as metadata**: no longer rendered as document
  text, no longer listed in the table of contents (#56).
- **GUI: `Cmd+F` opens the search on macOS.** It was testing the physical Ctrl
  key, which ⌘ never sets (#63).
- **GUI: table of contents entries scroll to the right section** when the
  document contains setext headings. Sections are now cut at the heading
  positions of the same comrak parse the TOC is built from (#57).
- **TUI: the first frame is drawn immediately.** The terminal image-capability
  query ran before the first draw for every document, even one without images,
  and cost a full timeout on terminals that never answer — 7.7 s to first frame
  on an eleven-line file over a plain pty. It now happens only for documents
  that have an image or a diagram to show (#58).
- **TUI: long lines wrap** instead of being cut at the right edge with no way
  to reach the end of the sentence. Styles, gutters and list indentation are
  preserved on continuation rows, and scroll offsets account for the folded
  height (#54).
- **Stdin temp files are removed on exit**, and files older than a day are
  swept at startup to recover what a killed run left behind. They used to
  accumulate for the life of the machine (#64).
- **Snap launches on Wayland.** The snap shipped the Wayland, xkb and Mesa
  assets but exported none of the environment needed to reach them inside
  confinement (#47).
- **Fedora: `.rpm` dependency resolution** is documented — `dnf install
  ./mdr-*.rpm` resolves `libxdo.so.3` (from `xdotool-libs`), `rpm -i` does not
  (#44).

### Changed

- **Release notes are the matching `CHANGELOG.md` section** instead of an
  auto-generated list of merged pull requests, and re-running the workflow for
  a tag replaces the body instead of appending to it (#49).
- **CI: clippy is blocking**, across all features and each backend alone;
  `cargo fmt --check` runs; and a job pinned to the declared MSRV (Rust 1.95,
  the floor asked for by `kdl 6.7.1`) checks and tests the crate (#50).
- **`flake.nix` reads its metadata from `Cargo.toml`**, so the Nix derivation
  can no longer claim 0.1.0 while the crate is at 0.4.0. The
  `darwin.apple_sdk.frameworks` block was removed: the attribute no longer
  exists in the nixpkgs the flake tracks (#48).
- **`Cargo.lock` is tracked**, making builds reproducible (#53), and
  semver-compatible dependencies were refreshed.
- Removed `core::search`, an unused module no backend ever called.

### Not verified

Stated plainly, because the release ships them anyway: the *release* workflow
has never been executed — only the CI one, on the release PR — `nix` is not
available to evaluate the flake, `mdr.desktop` could not be linted
(`desktop-file-validate` is absent; the man page *was* checked, `mandoc -T
lint` is clean), the snap was not built, and the native `Cmd+Q` menu item was
not clicked in a real macOS window. Everything else in this list is covered by
the test suite or was checked by hand.

The hardened CI paid for itself on its first run: Windows failed three
integration tests because they isolated the temp directory through `TMPDIR`
alone, which `std::env::temp_dir()` ignores on Windows in favour of `TMP` and
`TEMP`. The tests were writing to the real temp directory and asserting against
an empty one. Fixed in the tests; the product code was correct.

## [0.3.2] - 2026-06-22

### Added
- TOC panel in egui is now **resizable** by dragging its edge (#27)
- HTTP image fetching in TUI now has a **30-second timeout**
- `file_to_data_uri()` now rejects image files larger than **100 MB** to prevent OOM

### Fixed
- Replaced `std::mem::forget` with explicit `Box::leak` in `watcher.rs` (#16)
- Mermaid diamond-node panic reported upstream; workaround already in place (#4)
- `cargo binstall mdr` confirmed working (#22)

## [0.3.1] - 2026-06-22

### Added
- TOC toggle in egui backend: press **F10** or click *Hide TOC / Show TOC* in the search bar (#32, #41)
- Window size and position persistence in egui backend via eframe's `persistence` feature (#43)
- System font loading in egui backend for non-Latin script support (CJK, etc.) (#31)
- Image file validation by magic bytes across all backends — invalid/mislabeled images render a visible placeholder instead of failing silently (#14)

### Fixed
- Headings inside fenced code blocks are no longer treated as section boundaries in egui (#30)

### Changed
- Cargo dependencies bumped:
  - `eframe` 0.33 → 0.34
  - `egui_commonmark` 0.22 → 0.23
  - `ratatui` 0.29 → 0.30
  - `ratatui-image` 4.2 → 11.0

## [0.3.0] - 2026-05-20

### Added
- Highlight.js syntax highlighting for code blocks in the webview backend, with `prefers-color-scheme` GitHub light/dark themes embedded via `include_str!`. Injected only when fenced code blocks are present (#35) — thanks @njreid
- Custom KDL v2 grammar definition for highlight.js (#35)
- Fullscreen expand overlay for images and Mermaid diagrams in the webview backend, with hover button, double-click, and `Esc` to close (#34) — thanks @njreid
- User config file at `~/.config/mdr/config.kdl` (KDL v2) with `--init` to scaffold and `--config PATH` to override (#36) — thanks @njreid
- `system-ui` added to the default font stack for a native desktop look (#36) — thanks @njreid
- `with_devtools(true)` on the webview to enable in-app dev tools (#34)

### Fixed
- Webview backend crash on Linux/Wayland (`the window handle kind is not supported`). Switch to wry's GTK-native API (`WebViewBuilderExtUnix::build_gtk`) on Linux so the same binary works on X11 and Wayland (#33, closes #28) — thanks @njreid
- TUI backend now exits early when stdout is not a terminal. Without this, `enable_raw_mode()` succeeds on Windows pipes and the event loop spins forever — this was the cause of every `main` CI run being cancelled at the 6h timeout since February (#39)

### Changed
- Cargo dependencies bumped — major versions where drop-in (#40):
  - `comrak` 0.50 → 0.52
  - `mermaid-rs-renderer` 0.1 → 0.2
  - `resvg`, `usvg` 0.45 → 0.47
  - `tiny-skia` 0.11 → 0.12
  - `wry` 0.54 → 0.55
  - `tao` 0.34 → 0.35
  - `muda` 0.15 → 0.19
  - `ratatui-image` 4.1 → 4.2

### Internal
- `timeout-minutes: 90` added on the CI check job so a future regression of the kind that caused the 6-month outage fails fast

## [0.2.6] - 2026-02-23

### Added
- Application window icon: both egui and webview backends now display the mdr logo as window icon

## [0.2.4] - 2026-02-23

### Added
- `--list-backends` flag to display available backends at runtime
- Backend names shown in CLI help output

## [0.2.3] - 2026-02-22

### Fixed
- Homebrew tap release workflow: remove broken copy action, fix heredoc syntax

## [0.2.2] - 2026-02-22

### Security
- Path traversal protection for local file access
- Content Security Policy (CSP) headers in webview backend
- Regex caching to prevent ReDoS
- HTML encoding of user-controlled content (`html_encode`)

## [0.2.1] - 2026-02-22

### Fixed
- Windows build: move `[target.'cfg(unix)'.dependencies]` section after optional deps in `Cargo.toml`

## [0.2.0] - 2026-02-22

### Added
- Mouse scroll support in TUI backend
- Mermaid diagram rendering as terminal images in TUI backend
- SVG image support in TUI backend
- Offline Mermaid rendering via embedded `mermaid.js`
- Project logo in README and documentation
- Verbose mode (`-v`) for debug output across all backends

### Fixed
- Image rendering consistency across egui, webview, and TUI backends
- Local image path resolution in TUI backend
- SVG images not displaying in webview backend (#10)
- SVG rendering in egui: rasterize to PNG instead of inline embedding (security fix)
- Mermaid rendering improvements in TUI and webview backends

### Changed
- SVG images rasterized to PNG in egui backend for consistency
- README updated with logo and LLM-era motivation section

## [0.1.1] - 2026-02-22

### Added
- WinGet package publishing job in release workflow
- AUR (Arch User Repository) package publishing job in release workflow
- crates.io publish job in release workflow

## [0.1.0] - 2026-02-22

### Added
- In-document search (`/` to open, `n`/`N` to navigate matches)
- Packaging support: Homebrew, Nix, pre-built binaries
- Release infrastructure: GitHub Actions CI/CD pipeline

## [0.9] - 2026-02-22

### Added
- Initial project bootstrap with egui and webview dual rendering backends
- TUI backend using Ratatui for terminal rendering
- TOC (Table of Contents) sidebar navigation
- Mermaid diagram rendering support
- Image rendering in TUI backend with local image path resolution
- Auto-detection of rendering backend based on environment
- Security hardening for file access and rendering pipeline
- GitHub Actions CI/CD workflow
- 58 unit tests

### Fixed
- Mermaid diagram text rendering
- TOC scroll navigation in egui backend
- Mermaid rendering robustness in egui backend

[0.3.2]: https://github.com/CleverCloud/mdr/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/CleverCloud/mdr/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/CleverCloud/mdr/compare/v0.2.8...v0.3.0
[0.2.6]: https://github.com/CleverCloud/mdr/compare/v0.2.5...v0.2.6
[0.2.4]: https://github.com/CleverCloud/mdr/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/CleverCloud/mdr/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/CleverCloud/mdr/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/CleverCloud/mdr/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/CleverCloud/mdr/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/CleverCloud/mdr/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/CleverCloud/mdr/compare/0.9...v0.1.0
[0.9]: https://github.com/CleverCloud/mdr/releases/tag/0.9
