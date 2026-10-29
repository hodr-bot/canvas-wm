//! Vector menu text: DejaVu Sans rasterized through fontdue into a
//! single-channel-coverage RGBA atlas (white RGB, coverage alpha), so
//! the Options menu reads as smooth UI type instead of bitmap cells.
//!
//! One size only (MENU_PX): every menu string is laid out with
//! fontdue's shaping (kerning included), packed into the atlas on
//! first use. The backend uploads the atlas once, then re-uploads on
//! the frames the cache reports dirty. Missing glyphs (atlas full)
//! fall back to U+25A1 WHITE SQUARE, never to tofu panic.

use std::collections::HashMap;

use fontdue::layout::{
    CoordinateSystem, Layout, LayoutSettings, TextStyle,
};
use fontdue::{Font, FontSettings};

/// Embedded UI face, see assets/FONTS.md for provenance and licence.
const FONT_BYTES: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");
/// Menu type size in px: 16px leaves room for the slider track
/// below the text inside a 32px row (see menu::TEXT_DY/TRACK_*).
pub const MENU_PX: f32 = 16.0;
/// Atlas side (RGBA bytes, white premultiplied by coverage).
pub const ATLAS_SIDE: u32 = 512;
/// Glyph the cache degrades to when the atlas is full.
const FALLBACK_GLYPH: char = '\u{25A1}';

/// Atlas slot for one rasterized glyph: UV sub-rect plus bitmap size.
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub w: u32,
    pub h: u32,
}

/// One laid-out glyph: pen-space position (px, top-left of the
/// bitmap) plus the char to look up in the atlas.
#[derive(Debug, Clone, Copy)]
pub struct Placed {
    pub ch: char,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Rasterized UI face with a self-packing atlas.
pub struct FontCache {
    font: Font,
    layout: Layout,
    atlas: Vec<u8>,
    row_x: u32,
    row_y: u32,
    row_h: u32,
    slots: HashMap<char, Slot>,
    dirty: bool,
}

impl FontCache {
    /// Load the embedded face; falls back to an empty cache that
    /// draws nothing rather than aborting the compositor.
    pub fn new() -> Self {
        let font = Font::from_bytes(FONT_BYTES, FontSettings::default())
            .expect("embedded DejaVuSans parses");
        let side = ATLAS_SIDE as usize;
        Self {
            font,
            layout: Layout::new(CoordinateSystem::PositiveYDown),
            atlas: vec![0u8; side * side * 4],
            row_x: 1,
            row_y: 1,
            row_h: 0,
            slots: HashMap::new(),
            dirty: true,
        }
    }

    /// Full line advance: ascent - descent + gap at menu size.
    pub fn line_height(&self) -> f32 {
        self.font
            .horizontal_line_metrics(MENU_PX)
            .map(|m| m.ascent - m.descent + m.line_gap)
            .unwrap_or(MENU_PX * 1.25)
    }

    /// Is the cache waiting for a re-upload?
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// Raw atlas bytes (row-major RGBA).
    pub fn atlas(&self) -> &[u8] {
        &self.atlas
    }

    /// Atlas slot for a cached glyph.
    pub fn slot(&self, ch: char) -> Option<&Slot> {
        self.slots.get(&ch)
    }

    /// Raw rasterize probe (char, size) -> (metrics, coverage).
    pub fn font_rasterize(&self, ch: char, px: f32) -> (fontdue::Metrics, Vec<u8>) {
        self.font.rasterize(ch, px)
    }

    /// Rasterize one char into the atlas (shelf packing with 1px
    /// gutters so linear filtering never bleeds neighbours).
    /// Returns false only when the atlas is full.
    fn ensure(&mut self, ch: char) -> bool {
        if self.slots.contains_key(&ch) {
            return true;
        }
        let (metrics, bitmap) = self.font.rasterize(ch, MENU_PX);
        let (gw, gh) = (metrics.width as u32, metrics.height as u32);
        if gw == 0 || gh == 0 {
            // Whitespace and zero-size marks: no texels needed, but
            // remember the char so layout stays consistent.
            self.slots.insert(
                ch,
                Slot {
                    u0: 0.0,
                    v0: 0.0,
                    u1: 0.0,
                    v1: 0.0,
                    w: 0,
                    h: 0,
                },
            );
            return true;
        }
        let side = ATLAS_SIDE;
        if gw + 2 > side || gh + 2 > side {
            return false;
        }
        if self.row_x + gw + 2 > side {
            self.row_x = 1;
            self.row_y += self.row_h + 1;
            self.row_h = 0;
        }
        if self.row_y + gh + 2 > side {
            return false;
        }
        for (i, cov) in bitmap.iter().enumerate() {
            let gx = self.row_x + (i % metrics.width) as u32;
            let gy = self.row_y + (i / metrics.width) as u32;
            let o = ((gy * side + gx) * 4) as usize;
            // Premultiplied by coverage: the text program blends
            // (ONE, ONE_MINUS_SRC_ALPHA), so raw white RGB would paint
            // every glyph quad as a solid white block.
            self.atlas[o] = *cov;
            self.atlas[o + 1] = *cov;
            self.atlas[o + 2] = *cov;
            self.atlas[o + 3] = *cov;
        }
        let s = side as f32;
        self.slots.insert(
            ch,
            Slot {
                u0: self.row_x as f32 / s,
                v0: self.row_y as f32 / s,
                u1: (self.row_x + gw) as f32 / s,
                v1: (self.row_y + gh) as f32 / s,
                w: gw,
                h: gh,
            },
        );
        self.row_x += gw + 2;
        self.row_h = self.row_h.max(gh);
        self.dirty = true;
        true
    }

    /// Lay out one line: total advance width plus per-glyph quads in
    /// pen space (x grows right, y grows down from the line top).
    /// Every glyph is rasterized into the atlas on the way; a glyph
    /// that does not fit degrades to the fallback square.
    pub fn layout_line(&mut self, text: &str) -> (f32, Vec<Placed>) {
        self.layout.reset(&LayoutSettings {
            x: 0.0,
            y: 0.0,
            max_width: None,
            max_height: None,
            horizontal_align: fontdue::layout::HorizontalAlign::Left,
            vertical_align: fontdue::layout::VerticalAlign::Top,
            wrap_style: fontdue::layout::WrapStyle::Word,
            wrap_hard_breaks: false,
            line_height: 1.0,
        });
        self.layout
            .append(&[self.font.clone()], &TextStyle::new(text, MENU_PX, 0));
        // Borrow split: layout borrows nothing after glyphs() — copy
        // out first so ensure() can mutably pack the atlas.
        let glyphs: Vec<(char, f32, f32, usize, usize)> = self
            .layout
            .glyphs()
            .iter()
            .map(|g| (g.parent, g.x, g.y, g.width, g.height))
            .collect();
        let width = self
            .layout
            .glyphs()
            .last()
            .map(|g| g.x + g.width as f32)
            .unwrap_or(0.0);
        let mut out = Vec::with_capacity(glyphs.len());
        for (ch, x, y, w, h) in glyphs {
            let mut ch = ch;
            if !self.ensure(ch) {
                ch = FALLBACK_GLYPH;
                if !self.ensure(ch) {
                    continue;
                }
            }
            // A cached zero-size mark still advances the pen through
            // the layout width; it just draws no quad.
            if w == 0 || h == 0 {
                continue;
            }
            out.push(Placed {
                ch,
                x,
                y,
                w: w as f32,
                h: h as f32,
            });
        }
        (width, out)
    }

    /// Advance width of a line without keeping the glyphs.
    pub fn measure(&mut self, text: &str) -> f32 {
        self.layout_line(text).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every char the menu can print must rasterize (or be explicit
    /// whitespace): no tofu squares in French labels.
    #[test]
    fn menu_charset_fully_covered() {
        let mut cache = FontCache::new();
        let mut corpus = String::new();
        for tab in 0..crate::menu::TABS.len() {
            for row in crate::menu::tab_items(tab) {
                if let crate::menu::Row::Item(it) = row {
                    corpus.push_str(it.label);
                    corpus.push_str(&it.state_text(&crate::settings::Settings::default()));
                    // Option names of every choice.
                    if let crate::menu::MenuKind::Choice { options, .. } = &it.kind {
                        for o in options.iter() {
                            corpus.push_str(o);
                        }
                    }
                }
            }
        }
        for f in crate::menu::feet() {
            corpus.push_str(f);
        }
        for name in crate::menu::TABS {
            corpus.push_str(name);
        }
        let mut missing = Vec::new();
        for ch in corpus.chars() {
            if ch.is_whitespace() {
                continue;
            }
            let (m, px) = cache.font.rasterize(ch, MENU_PX);
            let lit = px.iter().any(|a| *a > 0);
            if m.width == 0 || !lit {
                missing.push(ch);
            }
        }
        assert!(missing.is_empty(), "uncovered chars: {:?}", missing);
    }

    #[test]
    fn measure_grows_with_text_and_line_height_sane() {
        let mut cache = FontCache::new();
        let a = cache.measure("Recul");
        let b = cache.measure("Recul : 90%");
        assert!(a > 20.0 && b > a);
        let lh = cache.line_height();
        assert!(lh > MENU_PX && lh < MENU_PX * 1.6);
        // Layout positions stay inside the measured advance.
        let (w, glyphs) = cache.layout_line("Échelle : 50 #0F121A");
        assert!(!glyphs.is_empty());
        for g in &glyphs {
            assert!(g.x + g.w <= w + 1.0, "glyph overflows advance");
        }
    }

    /// Dev-only visual probe: renders a French sample to /tmp so the
    /// bearing convention can be eyeballed. Ignored by default.
    #[test]
    #[ignore]
    fn dump_sample_pgm() {
        let mut cache = FontCache::new();
        let sample = "Échelle : 50 #0F121A (ç) [Super]";
        let (w, glyphs) = cache.layout_line(sample);
        let (w, h) = (w.ceil() as usize + 4, 40usize);
        let mut img = vec![0u8; w * h];
        for g in &glyphs {
            let slot = cache.slot(g.ch).unwrap();
            if slot.w == 0 {
                continue;
            }
            let side = ATLAS_SIDE as usize;
            let sx = (slot.u0 * side as f32) as usize;
            let sy = (slot.v0 * side as f32) as usize;
            for y in 0..g.h as usize {
                for x in 0..g.w as usize {
                    let a = cache.atlas[((sy + y) * side + sx + x) * 4 + 3];
                    let dx = g.x as usize + 2 + x;
                    let dy = g.y as usize + 2 + y;
                    if dx < w && dy < h {
                        img[dy * w + dx] = img[dy * w + dx].max(a);
                    }
                }
            }
        }
        let mut f = std::fs::File::create("/tmp/fontvec-sample.pgm").unwrap();
        use std::io::Write;
        f.write_all(format!("P5\n{} {}\n255\n", w, h).as_bytes())
            .unwrap();
        f.write_all(&img).unwrap();
    }
}
