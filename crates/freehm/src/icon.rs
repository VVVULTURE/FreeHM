//! The FreeHM icon, drawn in code so the tray, window and installer share one source.

/// Draw the FreeHM icon (three rising bars on a dark rounded tile) into BGRA pixels.
pub fn icon_pixels(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let r = s * 0.2;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(r, s - r);
            let cy = fy.clamp(r, s - r);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let a = (r - d + 0.5).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            // Background tile.
            let (mut cr, mut cg, mut cb) = (28.0, 30.0, 34.0);
            // Bars: x ranges and heights as fractions of the tile.
            for (x0, x1, h) in [(0.18, 0.34, 0.35), (0.42, 0.58, 0.55), (0.66, 0.82, 0.75)] {
                if fx >= s * x0 && fx < s * x1 && fy >= s * (0.85 - h) && fy < s * 0.85 {
                    (cr, cg, cb) = (118.0, 185.0, 0.0);
                }
            }
            let i = ((y * size + x) * 4) as usize;
            let al = (a * 255.0) as u8;
            px[i] = (cb * a) as u8;
            px[i + 1] = (cg * a) as u8;
            px[i + 2] = (cr * a) as u8;
            px[i + 3] = al;
        }
    }
    px
}

/// Straight-alpha RGBA version (window icons, PNG export).
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let mut px = icon_pixels(size);
    for p in px.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        let un = |c: u8| (c as u32 * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
        let (b, g, r) = (un(p[0]), un(p[1]), un(p[2]));
        p[0] = r;
        p[1] = g;
        p[2] = b;
    }
    px
}
