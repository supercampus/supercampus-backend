//! Renders the card a visitor receives on their phone.
//!
//! A visitor has no account and no app, so the pass has to survive as a plain
//! image in a WhatsApp thread: QR, wordmark, nothing else. The two tiers differ
//! only in the colour of the wordmark — gold for a guest, silver for a parent —
//! which is why the tier is derived from the visitor kind rather than stored.
//!
//! Geometry is taken from the reference artwork (370 × 665) and held as
//! fractions, so the card can be rendered at any resolution without redrawing:
//!
//! | element      | reference        | fraction        |
//! | ------------ | ---------------- | --------------- |
//! | canvas       | 370 × 665        | aspect 0.5564   |
//! | QR left      | 85               | 0.230 of width  |
//! | QR top       | 233              | 0.351 of height |
//! | QR size      | 199              | 0.536 of width  |
//! | wordmark     | baseline 606     | 0.912 of height |
//!
//! The wordmark stroke colours were sampled from the artwork rather than
//! guessed: #8E8E8E silver, #E3B632 gold.

use ab_glyph::{Font, FontRef, Glyph, PxScale, ScaleFont};
use anyhow::{Context, Result};
use image::{ImageEncoder, Rgba, RgbaImage, codecs::png::PngEncoder};
use qrcode::QrCode;

/// The two visitor tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassTier {
    /// A guest of the institution.
    Gold,
    /// A student's own guardian.
    Silver,
}

impl PassTier {
    /// Sampled from the reference cards.
    fn wordmark(self) -> Rgba<u8> {
        match self {
            PassTier::Gold => Rgba([0xE3, 0xB6, 0x32, 0xFF]),
            PassTier::Silver => Rgba([0x8E, 0x8E, 0x8E, 0xFF]),
        }
    }

    pub fn for_visitor_kind(kind: &str) -> Self {
        match kind {
            "guest" => PassTier::Gold,
            _ => PassTier::Silver,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PassTier::Gold => "gold",
            PassTier::Silver => "silver",
        }
    }
}

const FONT: &[u8] = include_bytes!("../assets/BrittanySignature.ttf");
const WORDMARK: &str = "SuperCampus";

// Reference proportions.
const ASPECT: f32 = 370.0 / 665.0;
const QR_LEFT: f32 = 85.0 / 370.0;
const QR_TOP: f32 = 233.0 / 665.0;
const QR_SIZE: f32 = 199.0 / 370.0;
const MARK_BASELINE: f32 = 606.0 / 665.0;
/// The wordmark measures 102px across on a 370px card. Fitting to that width is
/// the only reliable way to size a script face: its em size says very little
/// about how wide eleven joined letters actually run.
const MARK_WIDTH: f32 = 102.0 / 370.0;
const CORNER_RADIUS: f32 = 26.0 / 370.0;

/// Renders one pass as a PNG.
///
/// `payload` is the QR's contents — the opaque token, never anything about the
/// visitor. Anyone photographing the card over someone's shoulder learns a
/// random string and nothing else.
pub fn render(payload: &str, tier: PassTier, width: u32) -> Result<Vec<u8>> {
    let width = width.clamp(320, 2048);
    let height = (width as f32 / ASPECT).round() as u32;
    let mut canvas = RgbaImage::from_pixel(width, height, Rgba([255, 255, 255, 255]));

    round_corners(&mut canvas, CORNER_RADIUS * width as f32);
    draw_qr(&mut canvas, payload, width, height)?;
    draw_wordmark(&mut canvas, tier, width, height)?;

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            canvas.as_raw(),
            canvas.width(),
            canvas.height(),
            image::ExtendedColorType::Rgba8,
        )
        .context("failed to encode the pass card")?;
    Ok(png)
}

fn draw_qr(canvas: &mut RgbaImage, payload: &str, width: u32, height: u32) -> Result<()> {
    let code = QrCode::new(payload.as_bytes()).context("failed to encode the pass QR")?;
    let modules = code.width();

    let target = (QR_SIZE * width as f32).round() as u32;
    // A module has to be a whole number of pixels or the QR develops uneven
    // gaps that confuse scanners at an angle.
    let module_px = (target / modules as u32).max(1);
    let drawn = module_px * modules as u32;

    // Re-centre on the reference position after rounding down.
    let left = (QR_LEFT * width as f32).round() as i64
        + ((target as i64 - drawn as i64) / 2);
    let top = (QR_TOP * height as f32).round() as i64
        + ((target as i64 - drawn as i64) / 2);

    let dark = Rgba([0, 0, 0, 255]);
    for y in 0..modules {
        for x in 0..modules {
            if code[(x, y)] == qrcode::Color::Light {
                continue;
            }
            for dy in 0..module_px {
                for dx in 0..module_px {
                    let px = left + (x as u32 * module_px + dx) as i64;
                    let py = top + (y as u32 * module_px + dy) as i64;
                    if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < height {
                        canvas.put_pixel(px as u32, py as u32, dark);
                    }
                }
            }
        }
    }
    Ok(())
}

/// How wide the wordmark runs at a given scale, kerning included.
fn advance_of(font: &FontRef<'_>, scale: PxScale) -> f32 {
    let scaled = font.as_scaled(scale);
    let mut advance = 0.0_f32;
    let mut previous: Option<char> = None;
    for ch in WORDMARK.chars() {
        if let Some(prev) = previous {
            advance += scaled.kern(font.glyph_id(prev), font.glyph_id(ch));
        }
        advance += scaled.h_advance(font.glyph_id(ch));
        previous = Some(ch);
    }
    advance
}

fn draw_wordmark(canvas: &mut RgbaImage, tier: PassTier, width: u32, height: u32) -> Result<()> {
    let font = FontRef::try_from_slice(FONT).context("the wordmark font is unreadable")?;
    let colour = tier.wordmark();

    // Measure at a probe size, then scale so the drawn width matches the
    // reference. Sizing by em instead produced a wordmark twice the width of
    // the card.
    let probe = PxScale::from(100.0);
    let probe_advance = advance_of(&font, probe);
    let scale = PxScale::from(100.0 * (MARK_WIDTH * width as f32) / probe_advance.max(1.0));
    let scaled = font.as_scaled(scale);
    let advance = advance_of(&font, scale);

    let baseline = MARK_BASELINE * height as f32;
    let mut pen = (width as f32 - advance) / 2.0;
    let mut previous: Option<char> = None;

    for ch in WORDMARK.chars() {
        if let Some(prev) = previous {
            pen += scaled.kern(font.glyph_id(prev), font.glyph_id(ch));
        }
        let glyph: Glyph = font
            .glyph_id(ch)
            .with_scale_and_position(scale, ab_glyph::point(pen, baseline));
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                if coverage <= 0.0 {
                    return;
                }
                let px = bounds.min.x as i64 + gx as i64;
                let py = bounds.min.y as i64 + gy as i64;
                if px < 0 || py < 0 || px as u32 >= width || py as u32 >= height {
                    return;
                }
                let existing = canvas.get_pixel(px as u32, py as u32).0;
                let alpha = coverage.clamp(0.0, 1.0);
                // Blend rather than overwrite: a script face is mostly edge, and
                // hard pixels would read as a jagged signature.
                let blended = Rgba([
                    blend(existing[0], colour.0[0], alpha),
                    blend(existing[1], colour.0[1], alpha),
                    blend(existing[2], colour.0[2], alpha),
                    255,
                ]);
                canvas.put_pixel(px as u32, py as u32, blended);
            });
        }
        pen += scaled.h_advance(font.glyph_id(ch));
        previous = Some(ch);
    }
    Ok(())
}

fn blend(under: u8, over: u8, alpha: f32) -> u8 {
    (under as f32 * (1.0 - alpha) + over as f32 * alpha).round() as u8
}

/// Clears the four corners so the card reads as a rounded rectangle once it is
/// sitting on a chat background rather than on white.
fn round_corners(canvas: &mut RgbaImage, radius: f32) {
    let (width, height) = (canvas.width() as f32, canvas.height() as f32);
    let radius = radius.max(0.0);
    if radius < 1.0 {
        return;
    }
    let clear = Rgba([255, 255, 255, 0]);
    for y in 0..canvas.height() {
        for x in 0..canvas.width() {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = if fx < radius {
                radius - fx
            } else if fx > width - radius {
                fx - (width - radius)
            } else {
                0.0
            };
            let dy = if fy < radius {
                radius - fy
            } else if fy > height - radius {
                fy - (height - radius)
            } else {
                0.0
            };
            if dx > 0.0 && dy > 0.0 && (dx * dx + dy * dy).sqrt() > radius {
                canvas.put_pixel(x, y, clear);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_png_at_the_reference_proportions() {
        let png = render("token", PassTier::Gold, 740).expect("render");
        assert_eq!(&png[1..4], b"PNG");

        let decoded = image::load_from_memory(&png).expect("decode").to_rgba8();
        assert_eq!(decoded.width(), 740);
        // 740 / (370/665) = 1330
        assert_eq!(decoded.height(), 1330);
    }

    #[test]
    fn the_tier_only_changes_the_wordmark() {
        let gold = render("token", PassTier::Gold, 370).expect("gold");
        let silver = render("token", PassTier::Silver, 370).expect("silver");
        // Same QR, different signature — so the bytes differ, but not by much.
        assert_ne!(gold, silver);

        let gold_image = image::load_from_memory(&gold).expect("decode").to_rgba8();
        let silver_image = image::load_from_memory(&silver).expect("decode").to_rgba8();
        assert_eq!(gold_image.dimensions(), silver_image.dimensions());

        // The QR band is identical; only the wordmark band differs.
        let qr_row = (QR_TOP * gold_image.height() as f32) as u32 + 10;
        for x in 0..gold_image.width() {
            assert_eq!(
                gold_image.get_pixel(x, qr_row),
                silver_image.get_pixel(x, qr_row),
                "the QR differs between tiers at x={x}"
            );
        }
    }

    #[test]
    fn the_wordmark_is_drawn_in_the_tier_colour() {
        for (tier, expected) in [
            (PassTier::Gold, [0xE3u8, 0xB6, 0x32]),
            (PassTier::Silver, [0x8E, 0x8E, 0x8E]),
        ] {
            let png = render("token", tier, 370).expect("render");
            let image = image::load_from_memory(&png).expect("decode").to_rgba8();
            let band = (MARK_BASELINE * image.height() as f32) as u32;

            // The nearest pixel to the tier colour anywhere in the wordmark band
            // should be the colour itself, at the heart of a stroke.
            let mut closest = u32::MAX;
            for y in (band.saturating_sub(40))..=band {
                for x in 0..image.width() {
                    let p = image.get_pixel(x, y).0;
                    let d = p[..3]
                        .iter()
                        .zip(expected.iter())
                        .map(|(a, b)| (*a as i32 - *b as i32).pow(2) as u32)
                        .sum::<u32>();
                    closest = closest.min(d);
                }
            }
            assert!(
                closest < 64,
                "{} wordmark never reached its colour (closest squared distance {closest})",
                tier.as_str()
            );
        }
    }

    #[test]
    fn the_qr_survives_a_round_trip_through_the_card() {
        // The point of the whole exercise: a scanner has to get the token back.
        let png = render("visitor-token-abc123", PassTier::Silver, 740).expect("render");
        let image = image::load_from_memory(&png).expect("decode").to_luma8();
        let mut prepared = rqrr::PreparedImage::prepare(image);
        let grids = prepared.detect_grids();
        assert_eq!(grids.len(), 1, "the card's QR was not detectable");
        let (_meta, content) = grids[0].decode().expect("decode the QR");
        assert_eq!(content, "visitor-token-abc123");
    }
}
