//! Pixel conversion for frames read from a `PipeWire` screen stream.
//!
//! The `ScreenCast` portal delivers 8-bit packed RGB(A)-like buffers whose layout depends on
//! the compositor (`BGRx` is the usual one), possibly with padding at the end of each row.
//! [`to_rgba`] turns such a buffer into the tightly packed RGBA the rest of the application
//! expects ([`tan_application::ports::RawFrame`]).
//!
//! This module has no dependency on `PipeWire` so it can be unit tested on any OS; it is only
//! compiled on Linux (and in tests).
// The functions are only called from the Linux portal adapter; on other OSes just the tests
// reach them.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// Pixel layouts the screen stream is asked to deliver.
///
/// Names follow the byte order in memory: `Bgrx` is blue, green, red, one unused byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PixelFormat {
    /// Blue, green, red, unused.
    Bgrx,
    /// Blue, green, red, alpha.
    Bgra,
    /// Red, green, blue, unused.
    Rgbx,
    /// Red, green, blue, alpha.
    Rgba,
    /// Red, green, blue, three bytes per pixel.
    Rgb,
}

impl PixelFormat {
    /// Bytes one pixel occupies.
    pub(super) const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Bgrx | Self::Bgra | Self::Rgbx | Self::Rgba => 4,
        }
    }
}

/// The row pitch to use: the one the buffer reports when it is positive, else rows are taken
/// to be tightly packed.
pub(super) fn effective_stride(reported: i32, width: usize, bytes_per_pixel: usize) -> usize {
    usize::try_from(reported)
        .ok()
        .filter(|stride| *stride > 0)
        .unwrap_or_else(|| width.saturating_mul(bytes_per_pixel))
}

/// Number of bytes a `width` x `height` image with the given row pitch needs, or `None` when
/// the dimensions are empty, the pitch is shorter than a row, or the size overflows.
pub(super) fn min_buffer_len(
    width: u32,
    height: u32,
    stride: usize,
    bytes_per_pixel: usize,
) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    let row = usize::try_from(width).ok()?.checked_mul(bytes_per_pixel)?;
    if stride < row {
        return None;
    }
    let rows_before_last = usize::try_from(height - 1).ok()?;
    rows_before_last.checked_mul(stride)?.checked_add(row)
}

/// Converts `data` (rows `stride` bytes apart) into tightly packed RGBA with an opaque alpha.
///
/// Returns `None` when the buffer is too short for the dimensions or the dimensions are empty.
pub(super) fn to_rgba(
    format: PixelFormat,
    width: u32,
    height: u32,
    stride: usize,
    data: &[u8],
) -> Option<Vec<u8>> {
    let bytes_per_pixel = format.bytes_per_pixel();
    let needed = min_buffer_len(width, height, stride, bytes_per_pixel)?;
    if data.len() < needed {
        return None;
    }
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let row_bytes = width * bytes_per_pixel;
    let mut rgba = Vec::with_capacity(width.checked_mul(height)?.checked_mul(4)?);
    for row in 0..height {
        let start = row * stride;
        let source = &data[start..start + row_bytes];
        match format {
            PixelFormat::Bgrx | PixelFormat::Bgra => {
                for pixel in source.as_chunks::<4>().0 {
                    rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
                }
            }
            PixelFormat::Rgbx | PixelFormat::Rgba => {
                for pixel in source.as_chunks::<4>().0 {
                    rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                }
            }
            PixelFormat::Rgb => {
                for pixel in source.as_chunks::<3>().0 {
                    rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                }
            }
        }
    }
    Some(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgrx_and_bgra_swap_red_and_blue_and_force_opaque_alpha() {
        // Two pixels: (r=1, g=2, b=3) and (r=4, g=5, b=6); the fourth byte is junk / alpha 0.
        let data = [3, 2, 1, 77, 6, 5, 4, 0];
        let expected = vec![1, 2, 3, 255, 4, 5, 6, 255];
        assert_eq!(
            to_rgba(PixelFormat::Bgrx, 2, 1, 8, &data),
            Some(expected.clone())
        );
        assert_eq!(to_rgba(PixelFormat::Bgra, 2, 1, 8, &data), Some(expected));
    }

    #[test]
    fn rgbx_and_rgba_keep_the_channel_order() {
        let data = [1, 2, 3, 77, 4, 5, 6, 0];
        let expected = vec![1, 2, 3, 255, 4, 5, 6, 255];
        assert_eq!(
            to_rgba(PixelFormat::Rgbx, 2, 1, 8, &data),
            Some(expected.clone())
        );
        assert_eq!(to_rgba(PixelFormat::Rgba, 2, 1, 8, &data), Some(expected));
    }

    #[test]
    fn rgb_gets_an_alpha_channel() {
        let data = [1, 2, 3, 4, 5, 6];
        assert_eq!(
            to_rgba(PixelFormat::Rgb, 2, 1, 6, &data),
            Some(vec![1, 2, 3, 255, 4, 5, 6, 255])
        );
    }

    #[test]
    fn row_padding_is_dropped() {
        // 1 pixel wide, 2 rows, stride 8 (4 bytes of padding per row).
        let data = [3, 2, 1, 0, 9, 9, 9, 9, 6, 5, 4, 0];
        assert_eq!(
            to_rgba(PixelFormat::Bgrx, 1, 2, 8, &data),
            Some(vec![1, 2, 3, 255, 4, 5, 6, 255])
        );
    }

    #[test]
    fn the_last_row_needs_no_padding() {
        // Same image, buffer ends right after the last pixel: still valid.
        let data = [3, 2, 1, 0, 9, 9, 9, 9, 6, 5, 4, 0];
        assert_eq!(min_buffer_len(1, 2, 8, 4), Some(12));
        assert!(to_rgba(PixelFormat::Bgrx, 1, 2, 8, &data[..12]).is_some());
        assert!(to_rgba(PixelFormat::Bgrx, 1, 2, 8, &data[..11]).is_none());
    }

    #[test]
    fn short_buffers_and_empty_images_are_rejected() {
        assert_eq!(to_rgba(PixelFormat::Rgba, 2, 2, 8, &[0; 15]), None);
        assert_eq!(to_rgba(PixelFormat::Rgba, 0, 2, 8, &[0; 16]), None);
        assert_eq!(to_rgba(PixelFormat::Rgba, 2, 0, 8, &[0; 16]), None);
        // A stride shorter than one row cannot describe the image.
        assert_eq!(min_buffer_len(4, 1, 8, 4), None);
        assert_eq!(min_buffer_len(u32::MAX, u32::MAX, usize::MAX, 4), None);
    }

    #[test]
    fn a_missing_or_negative_stride_means_tightly_packed_rows() {
        assert_eq!(effective_stride(0, 10, 4), 40);
        assert_eq!(effective_stride(-5, 10, 4), 40);
        assert_eq!(effective_stride(48, 10, 4), 48);
    }

    #[test]
    fn bytes_per_pixel_matches_the_layout() {
        assert_eq!(PixelFormat::Rgb.bytes_per_pixel(), 3);
        assert_eq!(PixelFormat::Bgrx.bytes_per_pixel(), 4);
        assert_eq!(PixelFormat::Rgba.bytes_per_pixel(), 4);
    }
}
