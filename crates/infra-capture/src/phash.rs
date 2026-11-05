//! Perceptual hash of a screen frame.
//!
//! The screen is reduced to an 8x8 grid of average luminances (every pixel position that
//! feeds a cell is sampled on a fixed lattice, so the hash is deterministic and cheap even
//! for 4K frames). The 64 bits are laid out row by row, eight bits per row:
//!
//! * bits 0..=6 of a row: **gradient** bits (dHash), `1` when a cell is clearly brighter than
//!   its right neighbour. They capture layout and content (text, windows, slides).
//! * bit 7 of a row: **brightness** bit, `1` when the whole row is brighter than mid-grey.
//!   A pure dHash cannot tell a black screen from a white one (every comparison ties);
//!   these eight bits make such a change count as a full threshold-8 change.
//!
//! Small changes (a blinking cursor, a clock) move a cell's average by less than
//! [`TIE_MARGIN`] and flip nothing; a new window or slide flips many bits. The Hamming
//! distance between two hashes is compared against `AppSettings::frame_change_threshold`
//! by the pure frame selector in the application layer.

use tan_application::ports::{FrameHasher, RawFrame};

const GRID: usize = 8;
const GRID_U32: u32 = 8;
/// Luminance is accumulated at 4 bits of extra precision (cell values range `0..=4080`).
const LUMA_SCALE: u32 = 16;
/// Two cells closer than this (about two grey levels) count as equal: noise stays quiet.
const TIE_MARGIN: u32 = 2 * LUMA_SCALE;
/// Rows brighter than this (mid-grey) set their brightness bit.
const BRIGHT_ROW: u32 = 128 * LUMA_SCALE;
/// At most this many sample points per cell and axis.
const SAMPLES_PER_AXIS: usize = 32;

/// The [`FrameHasher`] used by the recording service.
#[derive(Debug, Default, Clone, Copy)]
pub struct DHasher;

impl FrameHasher for DHasher {
    fn hash(&self, frame: &RawFrame) -> u64 {
        dhash(frame.width, frame.height, &frame.rgba)
    }
}

/// Hash of an RGBA image (`width * height * 4` bytes, row-major). An empty or inconsistent
/// buffer hashes to `0`.
#[must_use]
pub fn dhash(width: u32, height: u32, rgba: &[u8]) -> u64 {
    let (Ok(w), Ok(h)) = (usize::try_from(width), usize::try_from(height)) else {
        return 0;
    };
    if w == 0 || h == 0 || rgba.len() < w.saturating_mul(h).saturating_mul(4) {
        return 0;
    }

    let mut cells = [[0_u32; GRID]; GRID];
    for (row, cells_row) in cells.iter_mut().enumerate() {
        for (col, cell) in cells_row.iter_mut().enumerate() {
            *cell = mean_luma(rgba, w, h, row, col);
        }
    }

    let mut hash = 0_u64;
    for (row, cells_row) in cells.iter().enumerate() {
        let mut bits = 0_u64;
        for col in 0..GRID - 1 {
            if cells_row[col] > cells_row[col + 1] + TIE_MARGIN {
                bits |= 1 << col;
            }
        }
        let row_mean = cells_row.iter().sum::<u32>() / GRID_U32;
        if row_mean > BRIGHT_ROW {
            bits |= 1 << (GRID - 1);
        }
        hash |= bits << (row * GRID);
    }
    hash
}

/// Average luminance of one grid cell, scaled by [`LUMA_SCALE`].
fn mean_luma(rgba: &[u8], width: usize, height: usize, row: usize, col: usize) -> u32 {
    let x0 = col * width / GRID;
    let x1 = ((col + 1) * width / GRID).clamp(x0 + 1, width);
    let y0 = row * height / GRID;
    let y1 = ((row + 1) * height / GRID).clamp(y0 + 1, height);
    let step_x = ((x1 - x0) / SAMPLES_PER_AXIS).max(1);
    let step_y = ((y1 - y0) / SAMPLES_PER_AXIS).max(1);

    let mut sum = 0_u64;
    let mut count = 0_u64;
    for y in (y0..y1).step_by(step_y) {
        let line = y * width;
        for x in (x0..x1).step_by(step_x) {
            let i = (line + x) * 4;
            // Rec. 601 luma in 8.8 fixed point: (77 R + 150 G + 29 B) / 256.
            let luma = 77 * u32::from(rgba[i])
                + 150 * u32::from(rgba[i + 1])
                + 29 * u32::from(rgba[i + 2]);
            sum += u64::from(luma);
            count += 1;
        }
    }
    // `luma` carries 8 extra bits; keep 4 of them (LUMA_SCALE = 2^4).
    u32::try_from(sum * u64::from(LUMA_SCALE) / 256 / count.max(1)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hamming(a: u64, b: u64) -> u32 {
        (a ^ b).count_ones()
    }

    /// A `w x h` image filled by `pixel(x, y) -> [r, g, b]`.
    #[allow(clippy::many_single_char_names)]
    fn image(w: u32, h: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = pixel(x, y);
                data.extend_from_slice(&[r, g, b, 255]);
            }
        }
        data
    }

    fn hash(w: u32, h: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> u64 {
        dhash(w, h, &image(w, h, pixel))
    }

    /// A "document": white page, dark text lines in the top-left.
    fn page(w: u32, h: u32, lines: u32) -> u64 {
        hash(w, h, |x, y| {
            let in_text = x > w / 10
                && x < w * 6 / 10
                && y > h / 10
                && y < h / 10 + lines * (h / 40) * 2
                && (y / (h / 40)).is_multiple_of(2);
            if in_text {
                [20, 20, 20]
            } else {
                [250, 250, 250]
            }
        })
    }

    #[test]
    fn identical_images_hash_identically() {
        assert_eq!(page(640, 360, 6), page(640, 360, 6));
    }

    #[test]
    fn the_same_picture_at_another_resolution_stays_close() {
        let small = page(640, 360, 6);
        let large = page(1920, 1080, 6);
        assert!(hamming(small, large) <= 4, "{}", hamming(small, large));
    }

    #[test]
    fn black_and_white_screens_differ_by_the_brightness_bits() {
        let black = hash(320, 180, |_, _| [0, 0, 0]);
        let white = hash(320, 180, |_, _| [255, 255, 255]);
        assert_eq!(black, 0);
        assert_eq!(hamming(black, white), 8);
    }

    #[test]
    fn a_dark_theme_differs_from_a_light_one_with_the_same_layout() {
        let light = page(640, 360, 8);
        let dark = hash(640, 360, |x, y| {
            let in_text = x > 64 && x < 384 && y > 36 && y < 36 + 8 * 18 && (y / 9) % 2 == 0;
            if in_text {
                [235, 235, 235]
            } else {
                [25, 25, 25]
            }
        });
        assert!(hamming(light, dark) >= 8, "{}", hamming(light, dark));
    }

    #[test]
    fn a_blinking_cursor_or_clock_does_not_change_the_hash() {
        let base = image(640, 360, |_, _| [250, 250, 250]);
        let mut with_cursor = base.clone();
        // A 2x14 px cursor.
        for y in 100..114 {
            for x in 200..202 {
                let i = (y * 640 + x) * 4;
                with_cursor[i..i + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        assert_eq!(dhash(640, 360, &base), dhash(640, 360, &with_cursor));
    }

    #[test]
    fn a_different_slide_is_far_away() {
        let slide_a = hash(640, 360, |x, _| {
            if x < 320 {
                [255, 255, 255]
            } else {
                [30, 60, 160]
            }
        });
        let slide_b = hash(640, 360, |_, y| {
            if y < 180 {
                [255, 255, 255]
            } else {
                [160, 60, 30]
            }
        });
        assert!(
            hamming(slide_a, slide_b) >= 8,
            "{}",
            hamming(slide_a, slide_b)
        );
    }

    #[test]
    fn more_text_on_the_page_moves_the_hash_further() {
        let few = page(640, 360, 2);
        let many = page(640, 360, 12);
        assert!(hamming(few, many) > 0);
    }

    #[test]
    fn degenerate_buffers_hash_to_zero() {
        assert_eq!(dhash(0, 0, &[]), 0);
        assert_eq!(
            dhash(10, 10, &[1, 2, 3]),
            0,
            "buffer shorter than the dimensions"
        );
    }

    #[test]
    fn tiny_images_do_not_panic() {
        let one = dhash(1, 1, &[200, 200, 200, 255]);
        let thin = dhash(3, 2, &[128; 3 * 2 * 4]);
        let _ = (one, thin);
    }

    #[test]
    fn the_hasher_port_delegates_to_dhash() {
        let rgba = image(
            64,
            64,
            |x, _| if x < 32 { [255, 255, 255] } else { [0, 0, 0] },
        );
        let frame = RawFrame {
            captured_at: std::time::Instant::now(),
            width: 64,
            height: 64,
            rgba: rgba.clone(),
        };
        assert_eq!(DHasher.hash(&frame), dhash(64, 64, &rgba));
    }
}
