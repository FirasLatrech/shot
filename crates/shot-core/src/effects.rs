//! Pixel effects: blur, pixelate, and shadows. They work in place on
//! premultiplied RGBA, restricted to a region `(x, y, w, h)` in pixels.

use tiny_skia::Pixmap;

pub type Region = (u32, u32, u32, u32);

/// The requested region clipped to the pixmap, so callers can't index out of bounds.
fn region_or_full(pm: &Pixmap, region: Option<Region>) -> Region {
    let (x, y, w, h) = region.unwrap_or((0, 0, pm.width(), pm.height()));
    let (x, y) = (x.min(pm.width()), y.min(pm.height()));
    (x, y, w.min(pm.width() - x), h.min(pm.height() - y))
}

/// Gaussian-like blur: three box-blur passes per axis.
pub fn blur(pm: &mut Pixmap, region: Option<Region>, radius: u32) {
    let (x0, y0, w, h) = region_or_full(pm, region);
    if radius == 0 || w == 0 || h == 0 {
        return;
    }
    let stride = pm.width() as usize * 4;
    let (w, h) = (w as usize, h as usize);
    let mut buf: Vec<u8> = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let start = (y0 as usize + y) * stride + x0 as usize * 4;
        buf.extend_from_slice(&pm.data()[start..start + w * 4]);
    }
    // A radius past the image size changes nothing but costs time.
    let r = (radius as usize).min(w.max(h));
    let mut tmp = vec![0u8; buf.len()];
    for _ in 0..3 {
        box_pass(&buf, &mut tmp, w, h, r, 4, w * 4);
        box_pass(&tmp, &mut buf, h, w, r, w * 4, 4);
    }
    for y in 0..h {
        let start = (y0 as usize + y) * stride + x0 as usize * 4;
        pm.data_mut()[start..start + w * 4].copy_from_slice(&buf[y * w * 4..(y + 1) * w * 4]);
    }
}

/// One box blur along an axis. `step` moves along the blurred axis,
/// `line` moves to the next row/column; edges clamp.
fn box_pass(src: &[u8], dst: &mut [u8], len: usize, lines: usize, r: usize, step: usize, line: usize) {
    let win = (2 * r + 1) as u64;
    for l in 0..lines {
        let base = l * line;
        let at = |i: isize, c: usize| src[base + i.clamp(0, len as isize - 1) as usize * step + c] as u64;
        for c in 0..4 {
            let mut sum: u64 = (-(r as isize)..=r as isize).map(|i| at(i, c)).sum();
            for i in 0..len {
                dst[base + i * step + c] = (sum / win) as u8;
                sum += at(i as isize + r as isize + 1, c);
                sum -= at(i as isize - r as isize, c);
            }
        }
    }
}

/// Mosaic with per-block jitter, so the original can't be recovered by
/// re-pixelating guesses (the "randomized" pixelate).
pub fn pixelate(pm: &mut Pixmap, region: Region, block: u32, seed: u64) {
    let (x0, y0, w, h) = region_or_full(pm, Some(region));
    let block = block.max(2);
    let pw = pm.width() as usize;
    let mut rng = seed | 1;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let data = pm.data_mut();
    for by in (y0..y0 + h).step_by(block as usize) {
        for bx in (x0..x0 + w).step_by(block as usize) {
            let (bw, bh) = (block.min(x0 + w - bx), block.min(y0 + h - by));
            let mut sum = [0u64; 4];
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let i = (y as usize * pw + x as usize) * 4;
                    (0..4).for_each(|c| sum[c] += data[i + c] as u64);
                }
            }
            let n = bw as u64 * bh as u64;
            let jitter = (next() % 25) as i32 - 12;
            let a = sum[3] / n;
            let avg: [u8; 4] = std::array::from_fn(|c| {
                let v = (sum[c] / n) as i32 + if c < 3 { jitter } else { 0 };
                // Premultiplied color must not exceed alpha.
                v.clamp(0, a as i32) as u8
            });
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let i = (y as usize * pw + x as usize) * 4;
                    data[i..i + 4].copy_from_slice(&avg);
                }
            }
        }
    }
}

/// Secure blur: pixelate first so no detail survives, then smooth it out.
pub fn secure_blur(pm: &mut Pixmap, region: Region, radius: u32, seed: u64) {
    pixelate(pm, region, (radius / 2).max(6), seed);
    blur(pm, Some(region), radius);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker() -> Pixmap {
        let mut pm = Pixmap::new(32, 32).unwrap();
        for (i, px) in pm.data_mut().chunks_mut(4).enumerate() {
            let on = ((i % 32) / 4 + (i / 32) / 4) % 2 == 0;
            px.copy_from_slice(&if on { [255, 255, 255, 255] } else { [0, 0, 0, 255] });
        }
        pm
    }

    fn variance(pm: &Pixmap) -> f64 {
        let v: Vec<f64> = pm.data().chunks(4).map(|p| p[0] as f64).collect();
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64
    }

    #[test]
    fn blur_reduces_contrast_and_keeps_alpha() {
        let mut pm = checker();
        let before = variance(&pm);
        blur(&mut pm, None, 4);
        assert!(variance(&pm) < before / 4.0);
        assert!(pm.data().chunks(4).all(|p| p[3] == 255));
    }

    #[test]
    fn pixelate_only_touches_region() {
        let mut pm = checker();
        let orig = pm.clone();
        pixelate(&mut pm, (0, 0, 16, 16), 8, 7);
        assert_eq!(pm.pixel(20, 20), orig.pixel(20, 20));
        assert_eq!(pm.pixel(0, 0), pm.pixel(7, 7), "a block is uniform");
    }
}
