//! Shared palette and type scale for the two graphical backends.
//!
//! The `gui` and `web` backends read their colours and their sizes from here,
//! so a document comes out in the same palette, at the same body size, in both.
//! Before this, `web` rendered from a stylesheet and `gui` ran on egui's
//! defaults — a 13 pt body with an 18 pt heading, which put `h1` through `h6`
//! within five points of each other.
//!
//! Sharing the constants is not the same as sharing the rendering: see
//! [`heading_scale`] for the one place `gui` cannot follow, because
//! `egui_commonmark` interpolates its own heading sizes.
//!
//! Not to be confused with [`crate::core::Theme`], which is the *colour scheme*
//! (`auto`, `dark`, `light`) the terminal backend picks. This module is the
//! palette and the sizes; that one is which of the two palettes to use.
//!
//! Colours follow the GitHub markdown palette. The pairs listed in the tests at
//! the bottom of this file — the document, the code backgrounds and the sidebar
//! states — are checked against the WCAG 2.2 contrast minimum (SC 1.4.3, 4.5:1
//! for body text). Colours that come from elsewhere are not: syntax
//! highlighting, search highlights and error text are outside this palette and
//! outside that check.

/// An opaque sRGB colour.
pub type Rgb = [u8; 3];

/// The colours a backend needs to render a document.
pub struct Palette {
    /// Page background.
    pub bg: Rgb,
    /// Body text.
    pub fg: Rgb,
    /// Bold text and headings.
    ///
    /// egui has no font weights — `RichText::strong()` only changes the colour —
    /// so this has to differ from [`Palette::fg`] for `**bold**` to be visible
    /// at all in the `gui` backend. The `web` backend has real weights and uses
    /// it only for the extra contrast.
    pub strong: Rgb,
    /// De-emphasised text: blockquotes, deep table-of-contents levels.
    pub muted: Rgb,
    /// Links.
    pub link: Rgb,
    /// Rules and table borders.
    pub border: Rgb,
    /// Background of fenced code blocks.
    pub code_bg: Rgb,
    /// Background of inline `code` spans.
    ///
    /// Deliberately more contrasted than [`Palette::code_bg`]: a chip has to
    /// stay visible inside a line of prose, while a block already stands out by
    /// its size.
    pub inline_code_bg: Rgb,
    /// Table of contents background.
    ///
    /// The sidebar colours reach the page through the stylesheet, so a build
    /// without the `web` backend never reads them.
    #[cfg_attr(not(feature = "webview-backend"), allow(dead_code))]
    pub sidebar_bg: Rgb,
    /// Table of contents hover background.
    #[cfg_attr(not(feature = "webview-backend"), allow(dead_code))]
    pub sidebar_hover: Rgb,
    /// Background of the entry the reader jumped to. Only the `web` sidebar
    /// marks the active entry.
    #[cfg_attr(not(feature = "webview-backend"), allow(dead_code))]
    pub sidebar_active: Rgb,
}

pub const DARK: Palette = Palette {
    bg: [0x0d, 0x11, 0x17],
    fg: [0xe6, 0xed, 0xf3],
    strong: [0xff, 0xff, 0xff],
    muted: [0x8b, 0x94, 0x9e],
    link: [0x58, 0xa6, 0xff],
    border: [0x30, 0x36, 0x3d],
    code_bg: [0x16, 0x1b, 0x22],
    inline_code_bg: [0x26, 0x2c, 0x36],
    sidebar_bg: [0x01, 0x04, 0x09],
    sidebar_hover: [0x16, 0x1b, 0x22],
    // GitHub's translucent #1f6feb33, flattened over `sidebar_bg` so the
    // contrast test below sees the colour a reader actually gets.
    sidebar_active: [0x07, 0x19, 0x36],
};

pub const LIGHT: Palette = Palette {
    bg: [0xff, 0xff, 0xff],
    fg: [0x1f, 0x23, 0x28],
    strong: [0x00, 0x00, 0x00],
    // One step darker than GitHub's #656d76: that value sits at 4.4999:1 against
    // the hovered sidebar entry, which the stylesheet keeps muted — just under
    // the 4.5:1 floor. #646c75 clears every pair it is used in.
    muted: [0x64, 0x6c, 0x75],
    link: [0x09, 0x69, 0xda],
    border: [0xd0, 0xd7, 0xde],
    code_bg: [0xf6, 0xf8, 0xfa],
    inline_code_bg: [0xef, 0xf1, 0xf3],
    sidebar_bg: [0xf6, 0xf8, 0xfa],
    sidebar_hover: [0xea, 0xee, 0xf2],
    sidebar_active: [0xdd, 0xf4, 0xff],
};

/// Body text size, in CSS pixels for `web` and in points for `gui`.
///
/// 16 is the browser default and the size GitHub renders Markdown at. egui's
/// own default is 13, which is what used to make the two backends disagree.
pub const BASE_FONT_SIZE: f32 = 16.0;

/// Code is set at 85 % of the surrounding prose, as GitHub does.
pub const CODE_FONT_SCALE: f32 = 0.85;

/// Line height for body text, used by `web`; egui exposes no equivalent knob.
///
/// 1.6 is what GitHub renders Markdown at. It is not a WCAG requirement —
/// SC 1.4.12 is about surviving a *user* raising line height to 1.5, which is a
/// different test than picking a default.
#[cfg_attr(not(feature = "webview-backend"), allow(dead_code))]
pub const LINE_HEIGHT: f32 = 1.6;

/// Heading size relative to the body, by level. `heading_size(1)` is `h1`.
///
/// The GitHub scale: 2, 1.5, 1.25, 1, 0.875, 0.85 em. Levels outside 1..=6 are
/// clamped rather than panicking — a malformed document should not take the
/// renderer with it.
///
/// **Only `web` renders the whole scale.** `gui` goes through
/// `egui_commonmark`, which interpolates its own sizes between
/// `TextStyle::Heading` and `TextStyle::Body` and offers no way to supply a
/// table. Setting those two ends makes `h1` and the body agree between the
/// backends; `h2` to `h6` follow that interpolation and come out larger than
/// this scale. Changing it needs a change upstream, not here.
pub fn heading_scale(level: u8) -> f32 {
    match level {
        0 | 1 => 2.0,
        2 => 1.5,
        3 => 1.25,
        4 => 1.0,
        5 => 0.875,
        _ => 0.85,
    }
}

/// Absolute heading size, in the same unit as [`BASE_FONT_SIZE`].
///
/// The stylesheet works in `em`, so only `gui` needs the absolute value.
#[cfg_attr(not(feature = "egui-backend"), allow(dead_code))]
pub fn heading_size(level: u8) -> f32 {
    BASE_FONT_SIZE * heading_scale(level)
}

/// `#rrggbb`, for the stylesheet — so only the `web` backend needs it.
#[cfg_attr(not(feature = "webview-backend"), allow(dead_code))]
pub fn hex(colour: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", colour[0], colour[1], colour[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relative luminance, per WCAG 2.2.
    ///
    /// <https://www.w3.org/TR/WCAG22/#dfn-relative-luminance>
    fn luminance(colour: Rgb) -> f64 {
        fn channel(v: u8) -> f64 {
            let v = f64::from(v) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(colour[0]) + 0.7152 * channel(colour[1]) + 0.0722 * channel(colour[2])
    }

    /// Contrast ratio between two opaque colours, per WCAG 2.2.
    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn check(palette: &Palette, name: &str) {
        // SC 1.4.3: 4.5:1 for body text.
        let pairs: &[(&str, Rgb, Rgb)] = &[
            ("body on page", palette.fg, palette.bg),
            ("strong on page", palette.strong, palette.bg),
            ("muted on page", palette.muted, palette.bg),
            ("link on page", palette.link, palette.bg),
            ("body on a code block", palette.fg, palette.code_bg),
            (
                "body on an inline code chip",
                palette.fg,
                palette.inline_code_bg,
            ),
            ("body on the sidebar", palette.fg, palette.sidebar_bg),
            ("body on a hovered entry", palette.fg, palette.sidebar_hover),
            (
                "body on the active entry",
                palette.fg,
                palette.sidebar_active,
            ),
            ("muted on the sidebar", palette.muted, palette.sidebar_bg),
            // The stylesheet keeps deep TOC levels muted on hover too, and marks
            // the active entry with the link colour — so those are the pairs a
            // reader actually meets, not `fg` on those backgrounds.
            (
                "muted on a hovered entry",
                palette.muted,
                palette.sidebar_hover,
            ),
            (
                "a link on the active entry",
                palette.link,
                palette.sidebar_active,
            ),
            (
                "muted on the active entry",
                palette.muted,
                palette.sidebar_active,
            ),
        ];
        for (what, fg, bg) in pairs {
            let ratio = contrast(*fg, *bg);
            assert!(
                ratio >= 4.5,
                "{name}: {what} is {ratio:.2}:1, below the 4.5:1 minimum"
            );
        }
    }

    #[test]
    fn the_dark_palette_meets_the_contrast_minimum() {
        check(&DARK, "dark");
    }

    #[test]
    fn the_light_palette_meets_the_contrast_minimum() {
        check(&LIGHT, "light");
    }

    #[test]
    fn bold_is_distinguishable_from_body_text() {
        // egui draws bold by changing the colour and nothing else, so `strong`
        // being equal to `fg` would make `**bold**` invisible there.
        for (palette, name) in [(&DARK, "dark"), (&LIGHT, "light")] {
            assert_ne!(
                palette.strong, palette.fg,
                "{name}: strong text must differ from body text"
            );
        }
    }

    #[test]
    fn an_inline_chip_stands_out_more_than_a_code_block() {
        // A chip sits inside a line of prose and has only its background to
        // separate it; a block has its size.
        for (palette, name) in [(&DARK, "dark"), (&LIGHT, "light")] {
            let chip = contrast(palette.inline_code_bg, palette.bg);
            let block = contrast(palette.code_bg, palette.bg);
            assert!(
                chip > block,
                "{name}: the inline chip ({chip:.2}) must stand out more than a block ({block:.2})"
            );
        }
    }

    #[test]
    fn the_heading_scale_descends_and_is_bounded() {
        let sizes: Vec<f32> = (1..=6).map(heading_size).collect();
        for pair in sizes.windows(2) {
            assert!(
                pair[0] > pair[1],
                "each heading level must be smaller than the one above: {sizes:?}"
            );
        }
        assert_eq!(heading_size(1), BASE_FONT_SIZE * 2.0);
        // A level outside the range must not panic, and must not grow.
        assert_eq!(heading_scale(7), heading_scale(6));
        assert_eq!(heading_scale(0), heading_scale(1));
    }

    #[test]
    fn hex_round_trips_the_palette() {
        assert_eq!(hex(DARK.bg), "#0d1117");
        assert_eq!(hex(LIGHT.link), "#0969da");
    }
}
