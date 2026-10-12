//! Pictures the app draws itself: rounded avatars, the group DM badge and
//! the presence dots.

use image::{DynamicImage, Rgba, RgbaImage};
use std::sync::OnceLock;

/// Corner radius as a share of the shorter side, close to Slack's avatars.
const RADIUS: f32 = 0.22;

/// How much of the pixel at (x, y) lies inside a rounded rectangle of
/// `w`×`h` with corner radius `r`, for smooth edges.
fn coverage(x: u32, y: u32, w: u32, h: u32, r: f32) -> f32 {
    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
    let cx = px.clamp(r, w as f32 - r);
    let cy = py.clamp(r, h as f32 - r);
    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}

/// The profile panel's large photo: gentler corners, as in Slack.
pub const PROFILE_RADIUS: f32 = 0.08;

/// Clears the corners so the photo reads as a rounded square.
pub fn rounded(img: DynamicImage) -> DynamicImage {
    rounded_by(img, RADIUS)
}

/// `rounded` with the radius as a share of the shorter side.
pub fn rounded_by(img: DynamicImage, radius: f32) -> DynamicImage {
    let mut img = img.to_rgba8();
    let (w, h) = img.dimensions();
    let r = w.min(h) as f32 * radius;
    for (x, y, p) in img.enumerate_pixels_mut() {
        p[3] = (p[3] as f32 * coverage(x, y, w, h, r)) as u8;
    }
    DynamicImage::ImageRgba8(img)
}

/// 5×7 digits; each row is five bits, the leftmost the highest.
const DIGITS: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

/// The group DM icon: a light rounded square with the number of other
/// people centred on it, sized `w`×`h` pixels to fill its cells exactly.
pub fn group_badge(n: usize, w: u32, h: u32) -> DynamicImage {
    let (w, h) = (w.max(8), h.max(8));
    let side = w.min(h);
    let (ox, oy) = ((w - side) / 2, (h - side) / 2);
    let r = side as f32 * RADIUS;
    let mut img = RgbaImage::new(w, h);
    for y in 0..side {
        for x in 0..side {
            let a = (255.0 * coverage(x, y, side, side, r)) as u8;
            img.put_pixel(ox + x, oy + y, Rgba([200, 200, 200, a]));
        }
    }

    let digits: Vec<usize> = n.min(99).to_string().bytes().map(|b| (b - b'0') as usize).collect();
    // Glyphs about half the badge tall, one dot apart.
    let dot = (side / 14).max(1);
    let text_w = digits.len() as u32 * 6 * dot - dot;
    let (tx, ty) = (ox + (side.saturating_sub(text_w)) / 2, oy + (side.saturating_sub(7 * dot)) / 2);
    for (i, d) in digits.iter().enumerate() {
        for (row, bits) in DIGITS[*d].iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                let x0 = tx + (i as u32 * 6 + col) * dot;
                let y0 = ty + row as u32 * dot;
                for y in y0..(y0 + dot).min(h) {
                    for x in x0..(x0 + dot).min(w) {
                        img.put_pixel(x, y, Rgba([30, 30, 30, 255]));
                    }
                }
            }
        }
    }
    DynamicImage::ImageRgba8(img)
}

/// Someone's presence as Slack draws it: a green dot when active, a ring
/// when away, and a "z" in either while notifications are paused.
#[derive(Clone, Copy, PartialEq)]
pub struct Presence {
    pub active: bool,
    pub paused: bool,
}

impl Presence {
    /// Short name for cache keys.
    pub fn key(self) -> &'static str {
        match (self.active, self.paused) {
            (true, false) => "on",
            (true, true) => "onz",
            (false, false) => "off",
            (false, true) => "offz",
        }
    }
}

const GREEN: [u8; 3] = [43, 172, 118];
const GREY: [u8; 3] = [160, 160, 160];

/// The terminal's own green (ANSI 2) and dark grey (ANSI 8), the colours the
/// text dots use; set once at startup when the terminal tells us.
static PALETTE: OnceLock<[[u8; 3]; 2]> = OnceLock::new();

/// Remembers the terminal's palette; a missing entry keeps the default.
pub fn set_palette(green: Option<[u8; 3]>, grey: Option<[u8; 3]>) {
    let _ = PALETTE.set([green.unwrap_or(GREEN), grey.unwrap_or(GREY)]);
}

/// How much of a pixel lies inside, from its signed distance to an edge.
fn cov(d: f32) -> f32 {
    (d + 0.5).clamp(0.0, 1.0)
}

/// Distance from (px, py) to the segment a–b.
fn seg(px: f32, py: f32, (ax, ay): (f32, f32), (bx, by): (f32, f32)) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2)).sqrt()
}

/// The dot's colour and opacity at (px, py), for a dot of radius `r` at (cx, cy).
fn dot_at(p: Presence, px: f32, py: f32, cx: f32, cy: f32, r: f32) -> Rgba<u8> {
    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
    let disc = cov(r - d);
    let ring = disc * cov(d - (r - (r * 0.32).max(1.2)));
    let z = if p.paused {
        let (s, w) = (r * 0.42, (r * 0.13).max(0.6));
        let (tl, tr, bl, br) = ((cx - s, cy - s), (cx + s, cy - s), (cx - s, cy + s), (cx + s, cy + s));
        let dz = seg(px, py, tl, tr).min(seg(px, py, tr, bl)).min(seg(px, py, bl, br));
        cov(w - dz)
    } else {
        0.0
    };
    // Active: the "z" is cut out of the dot; away: drawn inside the ring.
    let [green, grey] = PALETTE.get().copied().unwrap_or([GREEN, GREY]);
    let (c, a) = if p.active { (green, disc * (1.0 - z)) } else { (grey, ring.max(z)) };
    Rgba([c[0], c[1], c[2], (a * 255.0) as u8])
}

/// The presence dot alone, `w`×`h` pixels to fill one cell.
pub fn presence(p: Presence, w: u32, h: u32) -> DynamicImage {
    let (w, h) = (w.max(4), h.max(4));
    let r = (w.min(h) as f32 * 0.45).max(2.0);
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let img = RgbaImage::from_fn(w, h, |x, y| dot_at(p, x as f32 + 0.5, y as f32 + 0.5, cx, cy, r));
    DynamicImage::ImageRgba8(img)
}

/// A photo with the presence dot over its bottom-right corner, as Slack
/// shows it, with a clear gap around the dot.
pub fn with_presence(img: &DynamicImage, p: Presence) -> DynamicImage {
    let mut img = img.to_rgba8();
    let (w, h) = img.dimensions();
    let r = w.min(h) as f32 * 0.2;
    let (cx, cy) = (w as f32 - r, h as f32 - r);
    let gap = r + (r * 0.35).max(1.0);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
        if d > gap + 1.0 {
            continue;
        }
        px[3] = (px[3] as f32 * (1.0 - cov(gap - d))) as u8;
        // The dot over what is left of the photo.
        let top = dot_at(p, fx, fy, cx, cy, r);
        let (ta, ba) = (top[3] as f32 / 255.0, px[3] as f32 / 255.0);
        let a = ta + ba * (1.0 - ta);
        if a > 0.0 {
            for i in 0..3 {
                px[i] = ((top[i] as f32 * ta + px[i] as f32 * ba * (1.0 - ta)) / a) as u8;
            }
        }
        px[3] = (a * 255.0) as u8;
    }
    DynamicImage::ImageRgba8(img)
}
