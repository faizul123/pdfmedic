//! Coordinate mapping between PDF user space and the normalised model space.
//!
//! PDF user space: origin bottom-left, y up, *unrotated* (before `/Rotate`).
//! Model space: origin top-left, y down, as displayed (after `/Rotate`).

use crate::BBox;

/// Map a point from unrotated user space to displayed top-left space.
/// `(w0, h0)` is the *unrotated* page size; `rotate` is `/Rotate` clockwise degrees.
pub fn to_display(x: f32, y: f32, w0: f32, h0: f32, rotate: u16) -> (f32, f32) {
    // Unrotated top-left coordinates.
    let (u, v) = (x, h0 - y);
    match rotate % 360 {
        90 => (h0 - v, u),
        180 => (w0 - u, h0 - v),
        270 => (v, w0 - u),
        _ => (u, v),
    }
}

/// Map a user-space rect (left, bottom, right, top) to a displayed [`BBox`].
pub fn rect_to_display(left: f32, bottom: f32, right: f32, top: f32, w0: f32, h0: f32, rotate: u16) -> BBox {
    let pts = [
        to_display(left, bottom, w0, h0, rotate),
        to_display(right, bottom, w0, h0, rotate),
        to_display(right, top, w0, h0, rotate),
        to_display(left, top, w0, h0, rotate),
    ];
    let xs = pts.map(|p| p.0);
    let ys = pts.map(|p| p.1);
    BBox {
        x0: xs.iter().cloned().fold(f32::MAX, f32::min),
        y0: ys.iter().cloned().fold(f32::MAX, f32::min),
        x1: xs.iter().cloned().fold(f32::MIN, f32::max),
        y1: ys.iter().cloned().fold(f32::MIN, f32::max),
    }
}

/// Rotate a point on a `w x h` top-left-origin page clockwise by `deg`
/// (0/90/180/270). The result lives on the rotated page (`h x w` for 90/270).
pub fn rotate_cw_point(x: f32, y: f32, w: f32, h: f32, deg: u16) -> (f32, f32) {
    match deg % 360 {
        90 => (h - y, x),
        180 => (w - x, h - y),
        270 => (y, w - x),
        _ => (x, y),
    }
}

/// Rotate a box on a `w x h` page clockwise by `deg`.
pub fn rotate_bbox_cw(b: &BBox, w: f32, h: f32, deg: u16) -> BBox {
    let (ax, ay) = rotate_cw_point(b.x0, b.y0, w, h, deg);
    let (bx, by) = rotate_cw_point(b.x1, b.y1, w, h, deg);
    BBox { x0: ax.min(bx), y0: ay.min(by), x1: ax.max(bx), y1: ay.max(by) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unrotated 600 x 800 page. The point (100, 700) is near the top-left.
    #[test]
    fn unrotated_flips_y() {
        assert_eq!(to_display(100.0, 700.0, 600.0, 800.0, 0), (100.0, 100.0));
    }

    #[test]
    fn rotate_90_moves_top_left_to_top_right() {
        // Rotating clockwise 90: displayed size is 800 x 600; the old top-left
        // corner lands at the displayed top-right.
        let (x, y) = to_display(0.0, 800.0, 600.0, 800.0, 90);
        assert_eq!((x, y), (800.0, 0.0));
        // Old bottom-left (0,0) goes to displayed top-left.
        assert_eq!(to_display(0.0, 0.0, 600.0, 800.0, 90), (0.0, 0.0));
    }

    #[test]
    fn rotate_180_and_270() {
        assert_eq!(to_display(0.0, 0.0, 600.0, 800.0, 180), (600.0, 0.0));
        // 270 cw: old bottom-left goes to displayed bottom-right.
        assert_eq!(to_display(0.0, 0.0, 600.0, 800.0, 270), (800.0, 600.0));
    }

    #[test]
    fn rect_is_normalised_and_inside_displayed_page() {
        for rot in [0u16, 90, 180, 270] {
            let b = rect_to_display(72.0, 700.0, 300.0, 720.0, 600.0, 800.0, rot);
            let (dw, dh) = if rot % 180 == 90 { (800.0, 600.0) } else { (600.0, 800.0) };
            assert!(b.x0 < b.x1 && b.y0 < b.y1, "rot {rot}: {b:?}");
            assert!(b.x0 >= 0.0 && b.y0 >= 0.0 && b.x1 <= dw && b.y1 <= dh, "rot {rot}: {b:?}");
            // Area is preserved by rotation.
            assert!((b.width() * b.height() - 228.0 * 20.0).abs() < 1e-2);
        }
    }

    #[test]
    fn rotate_bbox_round_trips() {
        let b = BBox { x0: 10.0, y0: 20.0, x1: 110.0, y1: 50.0 };
        for deg in [90u16, 180, 270] {
            let r = rotate_bbox_cw(&b, 600.0, 800.0, deg);
            let (nw, nh) = if deg % 180 == 90 { (800.0, 600.0) } else { (600.0, 800.0) };
            let back = rotate_bbox_cw(&r, nw, nh, (360 - deg) % 360);
            assert_eq!(back, b, "deg {deg}");
        }
        // Clockwise 90: a wide box near top-left becomes a tall box near top-right.
        let r = rotate_bbox_cw(&b, 600.0, 800.0, 90);
        assert!(r.height() > r.width() && r.x1 > 700.0);
    }
}
