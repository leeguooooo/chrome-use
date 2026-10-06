//! Match the main alpha silhouette after rotation about the image's CSS origin.
//! Coordinates and angular velocity come from measured motion, not a fixed
//! vendor ratio. Verification remains the browser handler's responsibility.
use image::{DynamicImage, GenericImageView};
use std::collections::VecDeque;

#[derive(Clone, Copy)]
pub struct Motion {
    pub scale: f64,
    pub origin: (f64, f64),
    pub degrees_per_css: f64,
    pub angle_offset: f64,
    pub max_left: f64,
}
#[derive(Debug)]
pub struct Match {
    pub left: f64,
    pub edge_error: f64,
}

fn main_boundary(jig: &DynamicImage) -> Vec<(f64, f64)> {
    let im = jig.to_rgba8();
    let (w, h) = (im.width() as usize, im.height() as usize);
    let alpha: Vec<bool> = im.pixels().map(|p| p[3] > 40).collect();
    let mut dilated = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    dilated[y * w + x] |= alpha[yy * w + xx];
                }
            }
        }
    }
    let mut seen = vec![false; w * h];
    let mut largest = Vec::new();
    for start in 0..w * h {
        if seen[start] || !dilated[start] {
            continue;
        }
        seen[start] = true;
        let mut todo = VecDeque::from([start]);
        let mut component = Vec::new();
        while let Some(i) = todo.pop_front() {
            component.push(i);
            let (x, y) = (i % w, i / w);
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let k = yy * w + xx;
                    if !seen[k] && dilated[k] {
                        seen[k] = true;
                        todo.push_back(k);
                    }
                }
            }
        }
        if component.len() > largest.len() {
            largest = component;
        }
    }
    let mut mask = vec![false; w * h];
    for i in largest {
        mask[i] = alpha[i];
    }
    let mut boundary = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let mut any = false;
            let mut all = true;
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    any |= mask[yy * w + xx];
                    all &= mask[yy * w + xx];
                }
            }
            if any && !all {
                boundary.push((x as f64, y as f64));
            }
        }
    }
    boundary
}

/// Canny-style non-maximum suppression and hysteresis, followed by a chamfer
/// distance field. Compare contour geometry instead of correlating decoy pixels.
fn edge_distances(bg: &DynamicImage) -> Vec<f64> {
    let im = bg.to_luma8();
    let (w, h) = (im.width() as usize, im.height() as usize);
    let at = |x: isize, y: isize| -> f64 {
        im.get_pixel(
            x.clamp(0, w as isize - 1) as u32,
            y.clamp(0, h as isize - 1) as u32,
        )[0] as f64
    };
    let mut mag = vec![0.; w * h];
    let mut direction = vec![0u8; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let (xi, yi) = (x as isize, y as isize);
            let gx = -at(xi - 1, yi - 1) + at(xi + 1, yi - 1) - 2. * at(xi - 1, yi)
                + 2. * at(xi + 1, yi)
                - at(xi - 1, yi + 1)
                + at(xi + 1, yi + 1);
            let gy = -at(xi - 1, yi - 1) - 2. * at(xi, yi - 1) - at(xi + 1, yi - 1)
                + at(xi - 1, yi + 1)
                + 2. * at(xi, yi + 1)
                + at(xi + 1, yi + 1);
            let i = y * w + x;
            mag[i] = gx.abs() + gy.abs();
            direction[i] =
                ((gy.atan2(gx).to_degrees().rem_euclid(180.) + 22.5) / 45.).floor() as u8 % 4;
        }
    }
    let mut weak = vec![false; w * h];
    let mut edges = vec![false; w * h];
    let mut queue = VecDeque::new();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let (a, b) = match direction[i] {
                0 => (i - 1, i + 1),
                1 => (i - w - 1, i + w + 1),
                2 => (i - w, i + w),
                _ => (i - w + 1, i + w - 1),
            };
            if mag[i] >= 45. && mag[i] >= mag[a] && mag[i] >= mag[b] {
                weak[i] = true;
                if mag[i] >= 120. {
                    edges[i] = true;
                    queue.push_back(i);
                }
            }
        }
    }
    while let Some(i) = queue.pop_front() {
        let (x, y) = (i % w, i / w);
        for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
            for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                let k = yy * w + xx;
                if weak[k] && !edges[k] {
                    edges[k] = true;
                    queue.push_back(k);
                }
            }
        }
    }
    let mut dist: Vec<f64> = edges
        .into_iter()
        .map(|e| if e { 0. } else { 12. })
        .collect();
    let diag = 2f64.sqrt();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if x > 0 {
                dist[i] = dist[i].min(dist[i - 1] + 1.);
            }
            if y > 0 {
                dist[i] = dist[i].min(dist[i - w] + 1.);
                if x > 0 {
                    dist[i] = dist[i].min(dist[i - w - 1] + diag);
                }
                if x + 1 < w {
                    dist[i] = dist[i].min(dist[i - w + 1] + diag);
                }
            }
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if x + 1 < w {
                dist[i] = dist[i].min(dist[i + 1] + 1.);
            }
            if y + 1 < h {
                dist[i] = dist[i].min(dist[i + w] + 1.);
                if x > 0 {
                    dist[i] = dist[i].min(dist[i + w - 1] + diag);
                }
                if x + 1 < w {
                    dist[i] = dist[i].min(dist[i + w + 1] + diag);
                }
            }
        }
    }
    dist
}

pub fn locate(bg: &DynamicImage, jig: &DynamicImage, m: Motion) -> Option<Match> {
    let (w, h) = bg.dimensions();
    let (jw, jh) = jig.dimensions();
    if w < 3
        || h < 3
        || jw < 3
        || jh < 3
        || w as u64 * h as u64 > 4_000_000
        || jw as u64 * jh as u64 > 4_000_000
    {
        return None;
    }
    if ![
        m.scale,
        m.origin.0,
        m.origin.1,
        m.degrees_per_css,
        m.angle_offset,
        m.max_left,
    ]
    .iter()
    .all(|v| v.is_finite())
        || m.scale <= 0.
        || m.max_left <= 0.
        || m.max_left > 4096.
    {
        return None;
    }
    let contour = main_boundary(jig);
    if contour.len() < 30 {
        return None;
    }
    let distances = edge_distances(bg);
    let mut best = Match {
        left: 0.,
        edge_error: f64::INFINITY,
    };
    for step in 0..=(m.max_left * 4.) as usize {
        let left = step as f64 / 4.;
        let theta = (m.angle_offset + m.degrees_per_css * left).to_radians();
        let (c, s) = (theta.cos(), theta.sin());
        let mut error = 0.;
        let mut valid = 0;
        for &(x, y) in &contour {
            let xx = (left / m.scale + m.origin.0 + (x - m.origin.0) * c - (y - m.origin.1) * s)
                .round() as isize;
            let yy = (m.origin.1 + (x - m.origin.0) * s + (y - m.origin.1) * c).round() as isize;
            if xx >= 0 && yy >= 0 && xx < w as isize && yy < h as isize {
                valid += 1;
                error += distances[yy as usize * w as usize + xx as usize].min(12.);
            }
        }
        if valid as f64 / (contour.len() as f64) < 0.98 {
            continue;
        }
        error /= valid as f64;
        if error < best.edge_error {
            best = Match {
                left,
                edge_error: error,
            };
        }
    }
    (best.edge_error <= 1.8).then_some(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    #[test]
    fn finds_rotated_asymmetric_shape_with_small_decoy() {
        let mut jig = RgbaImage::new(80, 120);
        for y in 35..78 {
            for x in 12..53 {
                if x < 23 || y > 65 {
                    jig.put_pixel(x, y, Rgba([200, 180, 160, 255]));
                }
            }
        }
        for y in 8..12 {
            for x in 64..69 {
                jig.put_pixel(x, y, Rgba([180, 180, 180, 255]));
            }
        }
        let motion = Motion {
            scale: 0.75,
            origin: (80., 120.),
            degrees_per_css: 0.5,
            angle_offset: 0.,
            max_left: 140.,
        };
        let expected = 90.;
        let theta: f64 = (expected * motion.degrees_per_css).to_radians();
        let (c, s) = (theta.cos(), theta.sin());
        let mut bg = RgbaImage::from_pixel(320, 180, Rgba([210, 210, 210, 255]));
        // Inverse-map an independently rendered rotated silhouette into the fixture.
        for y in 0..180 {
            for x in 0..320 {
                let xx = x as f64 - expected / motion.scale - motion.origin.0;
                let yy = y as f64 - motion.origin.1;
                let sx = (motion.origin.0 + xx * c + yy * s).round() as i32;
                let sy = (motion.origin.1 - xx * s + yy * c).round() as i32;
                if (0..80).contains(&sx)
                    && (0..120).contains(&sy)
                    && jig.get_pixel(sx as u32, sy as u32)[3] > 40
                {
                    bg.put_pixel(x, y, Rgba([40, 40, 40, 255]));
                }
            }
        }
        let found = locate(
            &DynamicImage::ImageRgba8(bg),
            &DynamicImage::ImageRgba8(jig),
            motion,
        )
        .expect("rotated contour must be located");
        assert!((found.left - expected).abs() < 2., "{found:?}");
    }
    #[test]
    fn refuses_empty_shape_and_unmatched_background() {
        let bg =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(320, 180, Rgba([200, 200, 200, 255])));
        let mut jig = RgbaImage::new(80, 120);
        let m = Motion {
            scale: 0.75,
            origin: (80., 120.),
            degrees_per_css: 0.5,
            angle_offset: 0.,
            max_left: 140.,
        };
        assert!(locate(&bg, &DynamicImage::ImageRgba8(jig.clone()), m).is_none());
        for y in 35..78 {
            for x in 12..53 {
                jig.put_pixel(x, y, Rgba([20, 20, 20, 255]));
            }
        }
        assert!(locate(&bg, &DynamicImage::ImageRgba8(jig), m).is_none());
    }
    #[test]
    fn rejects_invalid_motion() {
        let im = DynamicImage::ImageRgba8(RgbaImage::new(10, 10));
        let m = Motion {
            scale: 0.,
            origin: (0., 0.),
            degrees_per_css: 1.,
            angle_offset: 0.,
            max_left: 100.,
        };
        assert!(locate(&im, &im, m).is_none());
    }
}
