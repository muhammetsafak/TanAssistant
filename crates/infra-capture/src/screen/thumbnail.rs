//! Scaling and encoding of display preview thumbnails ([`ScreenCapture::display_thumbnail`]).
//!
//! The pure part of the feature, shared by every platform: [`fit_within`] picks the size,
//! [`encode`] scales a captured frame down and encodes it.
//!
//! The image is PNG. A 320 px preview of a screen is a few tens of kilobytes, text stays
//! crisp, and the `image` crate already encodes PNG for the stored frames, so no new
//! dependency is needed (JPEG would be smaller but would pull in a decoder crate pair that
//! this feature never uses). Nothing here writes to disk or logs pixels.
//!
//! [`ScreenCapture::display_thumbnail`]: tan_application::ports::ScreenCapture::display_thumbnail

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::imageops;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use tan_application::ports::{DisplayThumbnail, RawFrame};

/// Media type of the encoded thumbnail.
pub(super) const MIME_TYPE: &str = "image/png";

/// The size an image of `width` x `height` pixels gets when its longest edge is limited to
/// `max_edge`: the aspect ratio is kept, nothing is enlarged and no edge drops below one
/// pixel. An empty source (or `max_edge` 0) gives `(0, 0)`.
pub(super) fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if width == 0 || height == 0 || max_edge == 0 {
        return (0, 0);
    }
    let longest = width.max(height);
    if longest <= max_edge {
        return (width, height);
    }
    // Round to nearest; the operands stay far below `u64::MAX`.
    let scale = |edge: u32| -> u32 {
        let scaled =
            (u64::from(edge) * u64::from(max_edge) + u64::from(longest) / 2) / u64::from(longest);
        u32::try_from(scaled).unwrap_or(max_edge).clamp(1, max_edge)
    };
    (scale(width), scale(height))
}

/// Scales `frame` down to `max_edge` on its longest edge and encodes it as PNG.
///
/// `None` when the frame is empty or its buffer does not match its dimensions, or when the
/// encoder fails.
pub(super) fn encode(frame: RawFrame, max_edge: u32) -> Option<DisplayThumbnail> {
    let (target_width, target_height) = fit_within(frame.width, frame.height, max_edge);
    if target_width == 0 || target_height == 0 {
        return None;
    }
    let source = RgbaImage::from_raw(frame.width, frame.height, frame.rgba)?;
    let scaled = if (target_width, target_height) == (frame.width, frame.height) {
        source
    } else {
        // Box-style averaging: the right filter for a large reduction.
        imageops::thumbnail(&source, target_width, target_height)
    };

    // The alpha channel carries nothing for a screen and some backends leave it at zero.
    let rgb: Vec<u8> = scaled
        .as_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect();

    let mut bytes = Vec::new();
    PngEncoder::new_with_quality(&mut bytes, CompressionType::Fast, FilterType::Adaptive)
        .write_image(&rgb, target_width, target_height, ExtendedColorType::Rgb8)
        .ok()?;
    Some(DisplayThumbnail {
        mime_type: MIME_TYPE,
        width: target_width,
        height: target_height,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn solid_frame(width: u32, height: u32, rgba: [u8; 4]) -> RawFrame {
        RawFrame {
            captured_at: Instant::now(),
            width,
            height,
            rgba: rgba.repeat((width * height) as usize),
        }
    }

    #[test]
    fn landscape_is_limited_by_its_width() {
        assert_eq!(fit_within(1920, 1080, 320), (320, 180));
        assert_eq!(fit_within(3840, 2160, 320), (320, 180));
    }

    #[test]
    fn portrait_is_limited_by_its_height() {
        assert_eq!(fit_within(1080, 1920, 320), (180, 320));
    }

    #[test]
    fn small_images_are_never_enlarged() {
        assert_eq!(fit_within(200, 100, 320), (200, 100));
        assert_eq!(fit_within(320, 240, 320), (320, 240));
    }

    #[test]
    fn extreme_ratios_keep_at_least_one_pixel() {
        assert_eq!(fit_within(10_000, 3, 320), (320, 1));
        assert_eq!(fit_within(3, 10_000, 320), (1, 320));
    }

    #[test]
    fn rounds_to_the_nearest_pixel() {
        // 1366 x 768 -> 320 x 179.9 -> 180.
        assert_eq!(fit_within(1366, 768, 320), (320, 180));
    }

    #[test]
    fn empty_sources_have_no_size() {
        assert_eq!(fit_within(0, 100, 320), (0, 0));
        assert_eq!(fit_within(100, 0, 320), (0, 0));
        assert_eq!(fit_within(100, 100, 0), (0, 0));
    }

    #[test]
    fn extreme_dimensions_do_not_overflow() {
        assert_eq!(fit_within(u32::MAX, u32::MAX, 320), (320, 320));
    }

    #[test]
    fn encodes_a_scaled_png_with_the_source_colour() {
        let thumbnail = encode(solid_frame(1280, 720, [10, 120, 230, 255]), 320).unwrap();
        assert_eq!(thumbnail.mime_type, "image/png");
        assert_eq!((thumbnail.width, thumbnail.height), (320, 180));
        assert!(thumbnail.bytes.starts_with(b"\x89PNG"));

        let decoded = image::load_from_memory(&thumbnail.bytes).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (320, 180));
        assert_eq!(decoded.get_pixel(160, 90).0, [10, 120, 230]);
    }

    #[test]
    fn a_small_frame_is_encoded_as_it_is() {
        let thumbnail = encode(solid_frame(64, 32, [1, 2, 3, 255]), 320).unwrap();
        assert_eq!((thumbnail.width, thumbnail.height), (64, 32));
    }

    #[test]
    fn transparent_pixels_do_not_make_the_image_blank() {
        // Some backends leave alpha at zero; the colour channels still carry the picture.
        let thumbnail = encode(solid_frame(640, 360, [200, 100, 50, 0]), 320).unwrap();
        let decoded = image::load_from_memory(&thumbnail.bytes).unwrap().to_rgb8();
        assert_eq!(decoded.get_pixel(10, 10).0, [200, 100, 50]);
    }

    #[test]
    fn rejects_empty_frames_and_mismatched_buffers() {
        assert!(encode(solid_frame(0, 0, [0; 4]), 320).is_none());
        let mut short = solid_frame(4, 4, [0; 4]);
        short.rgba.truncate(10);
        assert!(encode(short, 320).is_none());
    }
}
