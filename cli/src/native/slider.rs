//! Slider-puzzle captcha gap detection (网易易盾 / yidun and similar).
//!
//! The captcha gives two images: a background (`yidun_bg-img`) with a
//! piece-shaped notch (the gap), and a jigsaw piece PNG (`yidun_jigsaw`) whose
//! alpha channel is the piece silhouette, overlaid at the slider's current x.
//! To solve it you drag the handle so the piece slides into the gap.
//!
//! Detection is offline image processing on the two source images (fetched by
//! URL — no screenshot, no CORS/canvas taint): Sobel-x edges + a masked
//! normalized cross-correlation of the piece's edge silhouette against the
//! background's edges. The correlation peak is the gap's left x; the drag
//! distance is `gap_x - piece_x` in the image's natural pixels, which the caller
//! scales to CSS px by the background's displayed/natural width ratio.
//!
//! The actual drag uses the humanize trajectory (curved, decelerating, jittered)
//! so the motion passes the behavioural check — see `handle_drag` offset mode.

use image::{DynamicImage, GenericImageView};

/// Result of locating the gap, all in the background image's natural pixels.
#[derive(Debug, Clone, Copy)]
pub struct Gap {
    /// Left x of the piece in its initial position (≈ where the piece sits now).
    pub piece_x: u32,
    /// Detected left x of the gap notch.
    pub gap_x: u32,
    /// Horizontal distance to drag the piece: `gap_x - piece_x` (natural px).
    pub drag_nat: i64,
    /// Peak correlation score in `[-1, 1]`; higher = more confident.
    pub score: f64,
}

const ALPHA_THRESHOLD: u8 = 20;

fn luma(img: &DynamicImage) -> (Vec<f64>, usize, usize) {
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();
    let mut g = vec![0.0f64; (w * h) as usize];
    for (i, px) in rgba.pixels().enumerate() {
        g[i] = 0.299 * px[0] as f64 + 0.587 * px[1] as f64 + 0.114 * px[2] as f64;
    }
    (g, w as usize, h as usize)
}

/// |Sobel-x| — emphasises vertical edges, which dominate the gap's left/right
/// borders. Borders are clamped (replicate edge) so the map is the same size.
fn sobel_x(g: &[f64], w: usize, h: usize) -> Vec<f64> {
    let at = |x: isize, y: isize| -> f64 {
        let xc = x.clamp(0, w as isize - 1) as usize;
        let yc = y.clamp(0, h as isize - 1) as usize;
        g[yc * w + xc]
    };
    let mut out = vec![0.0f64; w * h];
    for y in 0..h as isize {
        for x in 0..w as isize {
            // [-1 0 1; -2 0 2; -1 0 1]
            let gx = -at(x - 1, y - 1) + at(x + 1, y - 1) - 2.0 * at(x - 1, y)
                + 2.0 * at(x + 1, y)
                - at(x - 1, y + 1)
                + at(x + 1, y + 1);
            out[y as usize * w + x as usize] = gx.abs();
        }
    }
    out
}

/// Locate the gap. `bg` is the gapped background, `jig` the jigsaw-piece PNG
/// (must carry alpha). Returns `None` if the piece silhouette can't be found.
pub fn detect_gap(bg: &DynamicImage, jig: &DynamicImage) -> Option<Gap> {
    let (bw, bh) = bg.dimensions();
    let (jw, jh) = jig.dimensions();
    let (bw, bh, jw, jh) = (bw as usize, bh as usize, jw as usize, jh as usize);

    // Piece silhouette bbox from the jigsaw alpha channel.
    let jig_rgba = jig.to_rgba8();
    let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0usize, usize::MAX, 0usize);
    for y in 0..jh {
        for x in 0..jw {
            if jig_rgba.get_pixel(x as u32, y as u32)[3] > ALPHA_THRESHOLD {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    if x0 == usize::MAX || x1 <= x0 || y1 <= y0 {
        return None;
    }
    // The piece band must fit inside the background vertically.
    let band_h = y1 - y0 + 1;
    let tw = x1 - x0 + 1;
    if y1 >= bh || tw >= bw || band_h > bh {
        return None;
    }

    let (bg_g, _, _) = luma(bg);
    let bg_e = sobel_x(&bg_g, bw, bh);
    let (jig_g, _, _) = luma(jig);
    let jig_e = sobel_x(&jig_g, jw, jh);

    // Build the masked, mean-centred template once.
    let mut mask = vec![0.0f64; band_h * tw];
    let mut tpl = vec![0.0f64; band_h * tw];
    let mut n = 0.0f64;
    let mut tpl_sum = 0.0f64;
    for ty in 0..band_h {
        for tx in 0..tw {
            let a = jig_rgba.get_pixel((x0 + tx) as u32, (y0 + ty) as u32)[3];
            if a > ALPHA_THRESHOLD {
                mask[ty * tw + tx] = 1.0;
                let v = jig_e[(y0 + ty) * jw + (x0 + tx)];
                tpl[ty * tw + tx] = v;
                tpl_sum += v;
                n += 1.0;
            }
        }
    }
    if n < 1.0 {
        return None;
    }
    let tpl_mean = tpl_sum / n;
    let mut tpl_c = vec![0.0f64; band_h * tw];
    let mut tpl_norm_sq = 0.0f64;
    for i in 0..tpl.len() {
        let c = (tpl[i] - tpl_mean) * mask[i];
        tpl_c[i] = c;
        tpl_norm_sq += c * c;
    }
    let tpl_norm = tpl_norm_sq.sqrt() + 1e-9;

    // Slide the template across the background at the piece's y-band. Start past
    // the piece's own position so we don't match the piece against itself.
    let start_x = x0 + tw / 2;
    let mut best_x = 0usize;
    let mut best = f64::MIN;
    for x in start_x..bw.saturating_sub(tw) {
        // masked mean of the window
        let mut win_sum = 0.0f64;
        for ty in 0..band_h {
            let row = (y0 + ty) * bw + x;
            for tx in 0..tw {
                win_sum += bg_e[row + tx] * mask[ty * tw + tx];
            }
        }
        let win_mean = win_sum / n;
        let mut dot = 0.0f64;
        let mut win_norm_sq = 0.0f64;
        for ty in 0..band_h {
            let row = (y0 + ty) * bw + x;
            for tx in 0..tw {
                let m = mask[ty * tw + tx];
                let c = (bg_e[row + tx] - win_mean) * m;
                dot += c * tpl_c[ty * tw + tx];
                win_norm_sq += c * c;
            }
        }
        let score = dot / ((win_norm_sq.sqrt() + 1e-9) * tpl_norm);
        if score > best {
            best = score;
            best_x = x;
        }
    }

    Some(Gap {
        piece_x: x0 as u32,
        gap_x: best_x as u32,
        drag_nat: best_x as i64 - x0 as i64,
        score: best,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// Synthetic: a piece-shaped bright block on the left of the jigsaw, and the
    /// same shape punched as a dark notch into a flat-ish background at a known x.
    /// The detector must recover that x.
    #[test]
    fn detects_synthetic_gap() {
        let (w, h) = (200u32, 80u32);
        let gap_at = 120u32;
        let (px0, pw, py0, ph) = (5u32, 30u32, 20u32, 40u32);

        // background: mid-grey with vertical texture, dark notch at gap_at.
        let mut bg = RgbaImage::from_pixel(w, h, Rgba([128, 128, 128, 255]));
        for y in 0..h {
            for x in 0..w {
                let v = 128i32 + ((x * 7 + y * 3) % 40) as i32 - 20;
                bg.put_pixel(x, y, Rgba([v as u8, v as u8, v as u8, 255]));
            }
        }
        for y in py0..py0 + ph {
            for x in gap_at..gap_at + pw {
                bg.put_pixel(x, y, Rgba([20, 20, 20, 255]));
            }
        }
        // jigsaw: transparent except the opaque bright piece at px0.
        let mut jig = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        for y in py0..py0 + ph {
            for x in px0..px0 + pw {
                jig.put_pixel(x, y, Rgba([230, 230, 230, 255]));
            }
        }

        let g = detect_gap(&DynamicImage::ImageRgba8(bg), &DynamicImage::ImageRgba8(jig))
            .expect("gap detected");
        assert_eq!(g.piece_x, px0);
        // within a couple px of the true gap
        assert!(
            (g.gap_x as i64 - gap_at as i64).abs() <= 2,
            "gap_x={} expected≈{}",
            g.gap_x,
            gap_at
        );
        assert_eq!(g.drag_nat, g.gap_x as i64 - g.piece_x as i64);
    }

    /// Optional real-fixture check: set YIDUN_FIXTURE_DIR to a dir holding
    /// bg.jpg + jig.png to validate against a captured captcha. The downloaded
    /// demo capture detects the gap at natural-x 158 (drag 154).
    #[test]
    fn detects_real_fixture_if_present() {
        let Ok(dir) = std::env::var("YIDUN_FIXTURE_DIR") else {
            return;
        };
        let bg = image::open(format!("{dir}/bg.jpg")).expect("bg");
        let jig = image::open(format!("{dir}/jig.png")).expect("jig");
        let g = detect_gap(&bg, &jig).expect("gap");
        eprintln!("fixture gap: {g:?}");
        assert!((g.gap_x as i64 - 158).abs() <= 4, "gap_x={}", g.gap_x);
    }
}
