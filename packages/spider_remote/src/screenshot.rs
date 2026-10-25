//! Fresh screenshot capture of a specific Minecraft window.
//!
//! The agent captures the game window that belongs to the selected process. It
//! never silently captures the whole desktop: a missing, minimized, or
//! unreadable window is reported as a failure. The image is bounded, encoded in
//! memory, and never written to disk.

use crate::protocol::{MAX_SCREENSHOT_BYTES, MAX_SCREENSHOT_DIMENSION};

/// The largest frame we are willing to allocate before downscaling.
const MAX_CAPTURE_PIXELS: u64 = 16_000_000;

#[derive(Debug, Clone)]
pub struct CapturedImage {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ScreenshotError {
    #[error("screenshot capture is implemented on Windows only; no image was captured")]
    UnsupportedPlatform,
    #[error("no visible Minecraft window belongs to process {0}")]
    WindowNotFound(u32),
    #[error("the Minecraft window is minimized; restore it before requesting a screenshot")]
    WindowMinimized,
    #[error("the Minecraft window is too large to capture safely ({width}x{height})")]
    WindowTooLarge { width: u32, height: u32 },
    #[error("Windows could not capture the game window: {0}")]
    Capture(String),
    #[error("the captured frame was empty, so it is not reported as a screenshot")]
    EmptyFrame,
    #[error("the screenshot could not be encoded within the {0} byte limit")]
    EncodeTooLarge(usize),
    #[error("the screenshot could not be encoded: {0}")]
    Encode(String),
}

/// Capture the largest visible top-level window owned by `pid`.
pub fn capture_game_window(pid: u32) -> Result<CapturedImage, ScreenshotError> {
    #[cfg(windows)]
    {
        windows_capture(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Err(ScreenshotError::UnsupportedPlatform)
    }
}

/// Downscale and encode an RGBA frame. Pure so the limits can be tested
/// without a real window or display.
pub fn encode_rgba(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> Result<CapturedImage, ScreenshotError> {
    if width == 0 || height == 0 {
        return Err(ScreenshotError::EmptyFrame);
    }
    let image = image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
        ScreenshotError::Encode("the frame dimensions did not match the pixels".into())
    })?;
    if looks_blank(image.as_raw()) {
        return Err(ScreenshotError::EmptyFrame);
    }

    let (target_width, target_height) = fit_dimensions(width, height);
    let image = if (target_width, target_height) == (width, height) {
        image
    } else {
        image::imageops::resize(
            &image,
            target_width,
            target_height,
            image::imageops::FilterType::Triangle,
        )
    };

    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|err| ScreenshotError::Encode(err.to_string()))?;
    let png = png.into_inner();
    if png.len() <= MAX_SCREENSHOT_BYTES {
        return Ok(CapturedImage {
            bytes: png,
            content_type: "image/png",
            width: target_width,
            height: target_height,
        });
    }

    let mut jpeg = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 78);
    image::DynamicImage::ImageRgba8(image)
        .write_with_encoder(encoder)
        .map_err(|err| ScreenshotError::Encode(err.to_string()))?;
    if jpeg.len() <= MAX_SCREENSHOT_BYTES {
        return Ok(CapturedImage {
            bytes: jpeg,
            content_type: "image/jpeg",
            width: target_width,
            height: target_height,
        });
    }
    Err(ScreenshotError::EncodeTooLarge(MAX_SCREENSHOT_BYTES))
}

/// Keep the aspect ratio while fitting inside the protocol dimension limit.
fn fit_dimensions(width: u32, height: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= MAX_SCREENSHOT_DIMENSION {
        return (width, height);
    }
    let scale = MAX_SCREENSHOT_DIMENSION as f64 / longest as f64;
    (
        ((width as f64 * scale).round() as u32).max(1),
        ((height as f64 * scale).round() as u32).max(1),
    )
}

/// A fully black or fully transparent frame is what a failed window capture
/// usually returns. Accepting it would be a fake success.
fn looks_blank(rgba: &[u8]) -> bool {
    if rgba.is_empty() {
        return true;
    }
    let mut any_visible = false;
    let mut any_non_black = false;
    for pixel in rgba.chunks_exact(4) {
        if pixel[3] != 0 {
            any_visible = true;
        }
        if pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0 {
            any_non_black = true;
        }
    }
    !any_visible || !any_non_black
}

#[cfg(windows)]
fn windows_capture(pid: u32) -> Result<CapturedImage, ScreenshotError> {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
        CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HGDIOBJ,
        ReleaseDC, SRCCOPY, SelectObject,
    };
    use windows_sys::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_STYLE, GetForegroundWindow, GetWindowLongW, GetWindowRect,
        GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        PW_RENDERFULLCONTENT, WS_CHILD,
    };

    struct Search {
        pid: u32,
        found_minimized: bool,
        best: Option<(HWND, i32, i32)>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` points at the caller's `Search` for the duration of
        // the EnumWindows call.
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        if owner != search.pid {
            return 1;
        }
        if unsafe { IsWindowVisible(hwnd) } == 0 {
            return 1;
        }
        if unsafe { IsIconic(hwnd) } != 0 {
            search.found_minimized = true;
            return 1;
        }
        if unsafe { GetWindowLongW(hwnd, GWL_STYLE) } & WS_CHILD as i32 != 0 {
            return 1;
        }
        if unsafe { GetWindowTextLengthW(hwnd) } == 0 {
            return 1;
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
            return 1;
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width < 32 || height < 32 {
            return 1;
        }
        let area = width as i64 * height as i64;
        if search
            .best
            .as_ref()
            .is_none_or(|(_, best_width, best_height)| {
                area > *best_width as i64 * *best_height as i64
            })
        {
            search.best = Some((hwnd, width, height));
        }
        1
    }

    let mut search = Search {
        pid,
        found_minimized: false,
        best: None,
    };
    // SAFETY: the callback matches WNDENUMPROC and only borrows `search`.
    let enumerated = unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
    if enumerated == 0 {
        return Err(ScreenshotError::Capture("window enumeration failed".into()));
    }
    let Some((hwnd, width, height)) = search.best else {
        return if search.found_minimized {
            Err(ScreenshotError::WindowMinimized)
        } else {
            Err(ScreenshotError::WindowNotFound(pid))
        };
    };
    let width_u = width as u32;
    let height_u = height as u32;
    if width_u as u64 * height_u as u64 > MAX_CAPTURE_PIXELS {
        return Err(ScreenshotError::WindowTooLarge {
            width: width_u,
            height: height_u,
        });
    }

    // SAFETY: GDI handles are checked before use and released below.
    unsafe {
        let window_dc = GetDC(hwnd);
        if window_dc.is_null() {
            return Err(ScreenshotError::Capture(
                "the window has no device context".into(),
            ));
        }
        let memory_dc = CreateCompatibleDC(window_dc);
        if memory_dc.is_null() {
            ReleaseDC(hwnd, window_dc);
            return Err(ScreenshotError::Capture(
                "a capture device context could not be created".into(),
            ));
        }
        let bitmap = CreateCompatibleBitmap(window_dc, width, height);
        if bitmap.is_null() {
            DeleteDC(memory_dc);
            ReleaseDC(hwnd, window_dc);
            return Err(ScreenshotError::Capture(
                "a capture bitmap could not be created".into(),
            ));
        }
        let previous = SelectObject(memory_dc, bitmap as HGDIOBJ);

        let mut painted = PrintWindow(hwnd, memory_dc, PW_RENDERFULLCONTENT);
        if painted == 0 {
            painted = PrintWindow(hwnd, memory_dc, 0 as PRINT_WINDOW_FLAGS);
        }
        if painted == 0 && GetForegroundWindow() == hwnd {
            // BitBlt from the window's own DC is only safe while the window is
            // in front; otherwise it would capture whatever covers it.
            painted = BitBlt(
                memory_dc,
                0,
                0,
                width,
                height,
                window_dc,
                0,
                0,
                SRCCOPY | CAPTUREBLT,
            );
        }

        SelectObject(memory_dc, previous);

        let result = if painted == 0 {
            Err(ScreenshotError::Capture(
                "the window did not render a frame (closed, protected, or an unsupported renderer)"
                    .into(),
            ))
        } else {
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            // Negative height asks for a top-down buffer.
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut pixels = vec![0u8; width as usize * height as usize * 4];
            let copied = GetDIBits(
                memory_dc,
                bitmap,
                0,
                height as u32,
                pixels.as_mut_ptr() as *mut c_void,
                &mut info,
                DIB_RGB_COLORS,
            );
            if copied == 0 {
                Err(ScreenshotError::Capture(
                    "the captured pixels could not be read".into(),
                ))
            } else {
                // GDI gives BGRA; the image crate expects RGBA.
                for pixel in pixels.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                    pixel[3] = 255;
                }
                encode_rgba(width_u, height_u, pixels)
            }
        };

        DeleteObject(bitmap as HGDIOBJ);
        DeleteDC(memory_dc);
        ReleaseDC(hwnd, window_dc);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x % 256) as u8,
                    (y % 256) as u8,
                    ((x + y) % 256) as u8,
                    255,
                ]);
            }
        }
        pixels
    }

    #[test]
    fn large_frames_are_downscaled_within_the_protocol_limit() {
        let (width, height) = fit_dimensions(3840, 2160);
        assert_eq!((width, height), (1920, 1080));
        let (width, height) = fit_dimensions(800, 600);
        assert_eq!((width, height), (800, 600));
    }

    #[test]
    fn encoding_produces_a_bounded_image() {
        let image = encode_rgba(320, 200, frame(320, 200)).unwrap();
        assert!(image.bytes.len() <= MAX_SCREENSHOT_BYTES);
        assert!(matches!(image.content_type, "image/png" | "image/jpeg"));
        assert_eq!((image.width, image.height), (320, 200));
        assert!(
            image.bytes.starts_with(&[0x89, b'P', b'N', b'G'])
                || image.bytes.starts_with(&[0xFF, 0xD8])
        );
    }

    #[test]
    fn a_black_frame_is_a_capture_failure_not_a_screenshot() {
        let result = encode_rgba(64, 64, vec![0u8; 64 * 64 * 4]);
        assert!(matches!(result, Err(ScreenshotError::EmptyFrame)));

        let mut transparent = vec![1u8; 64 * 64 * 4];
        for pixel in transparent.chunks_exact_mut(4) {
            pixel[3] = 0;
        }
        assert!(matches!(
            encode_rgba(64, 64, transparent),
            Err(ScreenshotError::EmptyFrame)
        ));
    }

    #[test]
    fn mismatched_pixel_data_is_rejected() {
        assert!(matches!(
            encode_rgba(10, 10, vec![255u8; 10]),
            Err(ScreenshotError::Encode(_))
        ));
    }

    #[test]
    fn non_windows_platforms_report_the_limitation_instead_of_an_image() {
        #[cfg(not(windows))]
        assert!(matches!(
            capture_game_window(std::process::id()),
            Err(ScreenshotError::UnsupportedPlatform)
        ));
    }
}
