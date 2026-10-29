//! Raster annotations drawn after capture, never in the page DOM.
use super::{Rect, ScreenshotAnnotation, ScreenshotOptions};
use base64::{engine::general_purpose::STANDARD, Engine};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use std::io::Cursor;

// A small numeric bitmap font keeps labels independent of installed fonts and
// adds no dependency or request. Rows are five bits wide, top to bottom.
const DIGITS: [[u8; 7]; 10] = [
    [14, 17, 19, 21, 25, 17, 14],
    [4, 12, 4, 4, 4, 4, 14],
    [14, 17, 1, 2, 4, 8, 31],
    [30, 1, 1, 14, 1, 1, 30],
    [2, 6, 10, 18, 31, 2, 2],
    [31, 16, 16, 30, 1, 1, 30],
    [14, 16, 16, 30, 17, 17, 14],
    [31, 1, 2, 4, 8, 8, 8],
    [14, 17, 17, 14, 17, 17, 14],
    [14, 17, 17, 15, 1, 1, 14],
];
const RED: Rgba<u8> = Rgba([230, 32, 32, 255]);
const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);

pub(super) fn render(
    encoded: &str,
    annotations: &[ScreenshotAnnotation],
    region: &Rect,
    raster_region: &Rect,
    options: &ScreenshotOptions,
) -> Result<String, String> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|e| format!("Failed to decode screenshot for processing: {e}"))?;
    let image = image::load_from_memory(&bytes)
        .map_err(|e| format!("Failed to read screenshot for processing: {e}"))?
        .to_rgba8();
    let mut image = crop(image, region, raster_region)?;
    draw(&mut image, annotations, region)?;
    let image = DynamicImage::ImageRgba8(image);
    let mut out = Cursor::new(Vec::new());
    if options.format == "jpeg" {
        let quality = options.quality.unwrap_or(80).clamp(0, 100) as u8;
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&image.to_rgb8())
            .map_err(|e| format!("Failed to encode screenshot JPEG: {e}"))?;
    } else {
        image
            .write_to(&mut out, ImageFormat::Png)
            .map_err(|e| format!("Failed to encode screenshot PNG: {e}"))?;
    }
    Ok(STANDARD.encode(out.into_inner()))
}

/// Crop the requested document region from a viewport/full-page raster before
/// drawing labels. This avoids asking Chrome to resize to a small selector clip.
fn crop(image: RgbaImage, region: &Rect, raster_region: &Rect) -> Result<RgbaImage, String> {
    if raster_region.width <= 0.0 || raster_region.height <= 0.0 {
        return Err("Screenshot: invalid capture geometry".into());
    }
    let sx = f64::from(image.width()) / raster_region.width;
    let sy = f64::from(image.height()) / raster_region.height;
    let x = ((region.x - raster_region.x) * sx).round();
    let y = ((region.y - raster_region.y) * sy).round();
    let width = (region.width * sx).round();
    let height = (region.height * sy).round();
    if ![x, y, width, height].iter().all(|v| v.is_finite())
        || x < 0.0
        || y < 0.0
        || width < 1.0
        || height < 1.0
        || x + width > f64::from(image.width())
        || y + height > f64::from(image.height())
    {
        return Err("Screenshot: requested region extends outside captured pixels".into());
    }
    if x == 0.0
        && y == 0.0
        && width == f64::from(image.width())
        && height == f64::from(image.height())
    {
        return Ok(image);
    }
    Ok(
        image::imageops::crop_imm(&image, x as u32, y as u32, width as u32, height as u32)
            .to_image(),
    )
}

fn fill(image: &mut RgbaImage, x: i64, y: i64, width: i64, height: i64, color: Rgba<u8>) {
    let right = x.saturating_add(width).clamp(0, i64::from(image.width()));
    let bottom = y.saturating_add(height).clamp(0, i64::from(image.height()));
    for py in y.max(0)..bottom {
        for px in x.max(0)..right {
            image.put_pixel(px as u32, py as u32, color);
        }
    }
}

fn draw(
    image: &mut RgbaImage,
    annotations: &[ScreenshotAnnotation],
    region: &Rect,
) -> Result<(), String> {
    if region.width <= 0.0
        || region.height <= 0.0
        || !region.width.is_finite()
        || !region.height.is_finite()
        || image.width() == 0
        || image.height() == 0
    {
        return Err("Screenshot: invalid raster geometry".to_string());
    }
    let sx = f64::from(image.width()) / region.width;
    let sy = f64::from(image.height()) / region.height;
    let line_x = (2.0 * sx).round().max(1.0) as i64;
    let line_y = (2.0 * sy).round().max(1.0) as i64;
    let cell = (2.0 * sx.min(sy)).round().clamp(1.0, 16.0) as i64;
    for annotation in annotations {
        let b = &annotation.box_;
        let x = (b.x as f64 * sx).round() as i64;
        let y = (b.y as f64 * sy).round() as i64;
        let width = (b.width as f64 * sx).round() as i64;
        let height = (b.height as f64 * sy).round() as i64;
        if width <= 0 || height <= 0 {
            continue;
        }
        fill(image, x, y, width, line_y, RED);
        fill(
            image,
            x,
            y.saturating_add(height).saturating_sub(line_y),
            width,
            line_y,
            RED,
        );
        fill(image, x, y, line_x, height, RED);
        fill(
            image,
            x.saturating_add(width).saturating_sub(line_x),
            y,
            line_x,
            height,
            RED,
        );
        let text = annotation.number.to_string();
        let label_width = (text.len() as i64 * 6 + 2) * cell;
        let label_height = 9 * cell;
        let lx = x.clamp(0, (i64::from(image.width()) - label_width).max(0));
        let ly = if y >= label_height {
            y - label_height
        } else {
            y.max(0)
        }
        .min((i64::from(image.height()) - label_height).max(0));
        fill(image, lx, ly, label_width, label_height, RED);
        for (index, digit) in text.bytes().enumerate() {
            for (row, bits) in DIGITS[(digit - b'0') as usize].iter().enumerate() {
                for col in 0..5 {
                    if bits & (1 << (4 - col)) != 0 {
                        fill(
                            image,
                            lx + cell + (index as i64 * 6 + col) * cell,
                            ly + cell + row as i64 * cell,
                            cell,
                            cell,
                            WHITE,
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::screenshot::AnnotationBox;

    #[test]
    fn crop_projects_document_offsets_at_device_scale_and_refuses_missing_pixels() {
        let raster = Rect {
            x: 100.0,
            y: 500.0,
            width: 100.0,
            height: 80.0,
        };
        let region = Rect {
            x: 130.0,
            y: 550.0,
            width: 20.0,
            height: 10.0,
        };
        let mut image = RgbaImage::from_pixel(200, 160, WHITE);
        image.put_pixel(60, 100, RED);
        let cropped = crop(image, &region, &raster).unwrap();
        assert_eq!((cropped.width(), cropped.height()), (40, 20));
        assert_eq!(*cropped.get_pixel(0, 0), RED);
        assert_eq!(*cropped.get_pixel(39, 19), WHITE);
        let outside = Rect { x: 90.0, ..region };
        assert!(crop(RgbaImage::new(200, 160), &outside, &raster).is_err());
    }

    fn annotation() -> ScreenshotAnnotation {
        ScreenshotAnnotation {
            ref_id: "e12".into(),
            number: 12,
            role: "button".into(),
            name: None,
            box_: AnnotationBox {
                x: 10,
                y: 20,
                width: 30,
                height: 20,
            },
        }
    }

    #[test]
    fn raster_borders_follow_capture_pixel_scale() {
        for scale in [1, 2, 3] {
            let mut image = RgbaImage::from_pixel(100 * scale, 80 * scale, WHITE);
            let region = Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 80.0,
            };
            draw(&mut image, &[annotation()], &region).unwrap();
            assert_eq!(*image.get_pixel(10 * scale, 30 * scale), RED);
            assert_eq!(*image.get_pixel(20 * scale, 30 * scale), WHITE);
            assert_eq!(*image.get_pixel(90 * scale, 70 * scale), WHITE);
        }
    }

    #[test]
    fn clipping_and_tiny_images_do_not_write_outside_raster() {
        let mut a = annotation();
        a.box_.x = -20;
        a.box_.y = -10;
        for size in [1, 5, 20] {
            let mut image = RgbaImage::from_pixel(size, size, WHITE);
            draw(
                &mut image,
                std::slice::from_ref(&a),
                &Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
            )
            .unwrap();
            assert_eq!(image.dimensions(), (size, size));
        }
    }

    #[test]
    fn encoded_output_preserves_dimensions_and_requested_format() {
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(100, 80, WHITE))
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        let raw = STANDARD.encode(png.into_inner());
        for (format, expected) in [("png", ImageFormat::Png), ("jpeg", ImageFormat::Jpeg)] {
            let options = ScreenshotOptions {
                format: format.into(),
                quality: Some(90),
                ..Default::default()
            };
            let encoded = render(
                &raw,
                &[annotation()],
                &Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 80.0,
                },
                &Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 80.0,
                },
                &options,
            )
            .unwrap();
            let bytes = STANDARD.decode(encoded).unwrap();
            assert_eq!(image::guess_format(&bytes).unwrap(), expected);
            let image = image::load_from_memory(&bytes).unwrap();
            assert_eq!((image.width(), image.height()), (100, 80));
        }
    }
}
