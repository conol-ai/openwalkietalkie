//! Shared, allocation-free Chinese/Japanese fallback. See tools/generate-cjk-font.py.
//! Records and pixels are byte arrays so the atlas needs no aligned reads.
use super::font_data::Glyph;

const PX: i32 = 26;
const RECORD: usize = 12;
// Named arrays keep the image in upper NOR, read in place. Desktop
// previews retain their normal read-only sections.
#[cfg_attr(target_os = "none", unsafe(link_section = ".flash_assets.cjk_metrics"))]
static METRICS: [u8; include_bytes!("../../assets/fonts/gen/cjk-metrics.bin").len()] =
    *include_bytes!("../../assets/fonts/gen/cjk-metrics.bin");
#[cfg_attr(target_os = "none", unsafe(link_section = ".flash_assets.cjk_atlas"))]
static ATLAS: [u8; include_bytes!("../../assets/fonts/gen/cjk.bin").len()] =
    *include_bytes!("../../assets/fonts/gen/cjk.bin");

pub fn source(cp: u32) -> Option<Glyph> {
    if !(0x2E80..=0xFFEF).contains(&cp) {
        return None;
    }
    let (mut lo, mut hi) = (0, METRICS.len() / RECORD);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let row = &METRICS[mid * RECORD..(mid + 1) * RECORD];
        let key = u16::from_le_bytes([row[0], row[1]]) as u32;
        if key < cp {
            lo = mid + 1;
        } else if key > cp {
            hi = mid;
        } else {
            return Some(Glyph {
                cp: key as u16,
                advance: row[2], left: row[3] as i8, top: row[4] as i8,
                w: row[5], h: row[6],
                offset: u32::from_le_bytes([row[8], row[9], row[10], row[11]]),
            });
        }
    }
    None
}

pub fn scaled(source: Glyph, px: u8) -> Glyph {
    let scale = |n: i32| (n * px as i32 + PX / 2).div_euclid(PX);
    Glyph {
        advance: scale(source.advance as i32) as u8,
        left: scale(source.left as i32) as i8,
        top: scale(source.top as i32) as i8,
        w: scale(source.w as i32) as u8,
        h: scale(source.h as i32) as u8,
        ..source
    }
}

/// Conservative line-box bounds, including CJK punctuation and kana.
pub fn above(px: u8) -> i32 { (px as i32 * 29 + 25) / PX }
pub fn below(px: u8) -> i32 { (px as i32 * 9 + 25) / PX }

fn alpha(g: &Glyph, x: usize, y: usize) -> u32 {
    let stride = (g.w as usize + 3) / 4;
    let byte = ATLAS[g.offset as usize + y * stride + x / 4];
    ((byte >> (6 - (x % 4) * 2)) & 3) as u32 * 5
}

/// Bilinear sampling at pixel centres, returning the renderer's 4-bit alpha.
/// All coordinates are glyph-local, so band clipping cannot change pixels.
pub fn coverage(source: &Glyph, dest: &Glyph, x: usize, y: usize) -> u8 {
    let axis = |p: usize, from: u8, to: u8| {
        let fixed = (((2 * p + 1) * from as usize * 128 / to as usize) as i32 - 128)
            .clamp(0, (from as i32 - 1) * 256) as usize;
        (fixed >> 8, (fixed & 255) as u32)
    };
    let (sx, fx) = axis(x, source.w, dest.w);
    let (sy, fy) = axis(y, source.h, dest.h);
    let nx = (sx + 1).min(source.w as usize - 1);
    let ny = (sy + 1).min(source.h as usize - 1);
    let top = alpha(source, sx, sy) * (256 - fx) + alpha(source, nx, sy) * fx;
    let bottom = alpha(source, sx, ny) * (256 - fx) + alpha(source, nx, ny) * fx;
    ((top * (256 - fy) + bottom * fy + 32768) >> 16) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::RAMP;

    #[test]
    fn cjk_atlas_is_sorted_bounded_and_covers_both_chinese_scripts() {
        assert_eq!(METRICS.len() % RECORD, 0);
        assert!(METRICS.len() / RECORD > 28_000);
        assert!(METRICS.len() + ATLAS.len() < 5_200_000);
        let mut previous = 0;
        for row in METRICS.chunks_exact(RECORD) {
            let cp = u16::from_le_bytes([row[0], row[1]]) as u32;
            assert!(cp > previous);
            previous = cp;
            let g = source(cp).unwrap();
            assert!(g.offset as usize + (g.w as usize + 3) / 4 * g.h as usize <= ATLAS.len());
            for spec in RAMP {
                let scaled = scaled(g, spec.px);
                assert!(scaled.top as i32 <= above(spec.px), "U+{cp:04X}");
                assert!(scaled.h as i32 - scaled.top as i32 <= below(spec.px), "U+{cp:04X}");
            }
        }
        for c in "中文字体测试简体繁體臺灣香港龍龜，。！？：；（）【】《》、１２３ㄅ㐀".chars() {
            assert_eq!(source(c as u32).unwrap().cp as u32, c as u32);
        }
        assert!(source('😀' as u32).is_none());
        assert!(source(0xFFFD).is_none());
    }

    #[test]
    fn japanese_kana_and_kanji_resolve_to_real_glyphs() {
        // Assigned hiragana (including combining / spacing voicing marks),
        // katakana, phonetic extensions, and halfwidth kana / punctuation.
        let kana = (0x3041..=0x3096)
            .chain(0x3099..=0x30FF)
            .chain(0x31F0..=0x31FF)
            .chain(0xFF61..=0xFF9F);
        assert_eq!(kana.clone().count(), 268);
        for cp in kana {
            let glyph = source(cp).unwrap_or_else(|| panic!("U+{cp:04X} missing"));
            assert_eq!(glyph.cp as u32, cp);
            assert!(glyph.w > 0 && glyph.h > 0, "U+{cp:04X} is blank");
        }
        for c in "日本語設定言語時計休憩集中開始停止戻る".chars() {
            assert_eq!(source(c as u32).unwrap().cp as u32, c as u32);
        }
        assert!(source(0x3040).is_none());
        assert!(source(0x3097).is_none());
        assert!(source(0x3098).is_none());
    }
}
