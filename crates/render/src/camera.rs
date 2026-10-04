//! 2D camera.
//!
//! # Coordinate systems
//! - **World coordinates**: fixed-point [`Fx`], measured in game pixels.
//! - **Screen coordinates**: origin top-left, "base pixels" (320x180 by default).
//! - Multiplied by the viewport scale, keeping visuals consistent at any window size.
//!
//! The camera position is the viewport **center** (matching Celeste's `Camera.Position`).

use reles_math::{Fx, Rect, Vec2};
use serde::{Deserialize, Serialize};

/// Base resolution (Celeste's native 320x180).
pub const BASE_WIDTH: f32 = 320.0;
/// Base resolution height.
pub const BASE_HEIGHT: f32 = 180.0;

/// 2D camera.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// Viewport center (world coordinates).
    pub position: Vec2,
    /// Zoom (1.0 = 1:1 at the base resolution).
    pub zoom: Fx,
    /// Base viewport size (base pixels).
    pub base_size: (f32, f32),
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            position: Vec2::ZERO,
            zoom: Fx::one(),
            base_size: (BASE_WIDTH, BASE_HEIGHT),
        }
    }
}

impl Camera {
    /// Creates a camera at the origin.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a camera centered at the given position.
    pub fn at(position: Vec2) -> Self {
        Camera {
            position,
            ..Self::default()
        }
    }

    /// Sets the zoom (clamped to `[0.1, 10.0]` to avoid degeneracy).
    pub fn with_zoom(mut self, zoom: Fx) -> Self {
        self.zoom = zoom.clamp(Fx::from_f32(0.1), Fx::from_int(10));
        self
    }

    /// Base viewport width.
    pub fn base_width(&self) -> Fx {
        Fx::from_f32(self.base_size.0)
    }

    /// Base viewport height.
    pub fn base_height(&self) -> Fx {
        Fx::from_f32(self.base_size.1)
    }

    /// Half-extent of the viewport in world coordinates.
    pub fn half_extent(&self) -> Vec2 {
        let half = Vec2::new(self.base_width(), self.base_height()) * Fx::from_ratio(1, 2);
        half / self.zoom
    }

    /// World coordinates → base screen coordinates.
    pub fn world_to_screen(&self, world: Vec2) -> Vec2 {
        // Center on the camera, apply zoom, then translate to a top-left origin.
        let rel = (world - self.position) * self.zoom;
        rel + Vec2::new(self.base_width(), self.base_height()) * Fx::from_ratio(1, 2)
    }

    /// Base screen coordinates → world coordinates.
    pub fn screen_to_world(&self, screen: Vec2) -> Vec2 {
        let half = Vec2::new(self.base_width(), self.base_height()) * Fx::from_ratio(1, 2);
        (screen - half) / self.zoom + self.position
    }

    /// The currently visible world rectangle.
    pub fn visible_rect(&self) -> Rect {
        let half = self.half_extent();
        Rect::new(self.position - half, self.position + half)
    }

    /// Base coordinates → normalized device coordinates (NDC, `-1..1`) for a viewport.
    ///
    /// `viewport` is `(width, height)` in pixels.
    pub fn base_to_ndc(&self, base: Vec2, viewport: (u32, u32)) -> (f32, f32) {
        let vw = viewport.0.max(1) as f32;
        let vh = viewport.1.max(1) as f32;
        // Preserve the aspect ratio (letterbox): use the smaller scale.
        let scale = (vw / self.base_size.0).min(vh / self.base_size.1);
        let drawn_w = self.base_size.0 * scale;
        let drawn_h = self.base_size.1 * scale;
        let offset_x = (vw - drawn_w) * 0.5;
        let offset_y = (vh - drawn_h) * 0.5;

        let px = offset_x + base.x.to_f32() * scale;
        let py = offset_y + base.y.to_f32() * scale;

        // Screen space (y down) → NDC (y up)
        let ndc_x = (px / vw) * 2.0 - 1.0;
        let ndc_y = 1.0 - (py / vh) * 2.0;
        (ndc_x, ndc_y)
    }

    /// World coordinates → NDC (skips the intermediate base coordinates for direct vertex use).
    pub fn world_to_ndc(&self, world: Vec2, viewport: (u32, u32)) -> (f32, f32) {
        self.base_to_ndc(self.world_to_screen(world), viewport)
    }

    /// Size scale from world units → NDC (used to compute a sprite's NDC width/height).
    pub fn world_size_to_ndc(&self, size: Vec2, viewport: (u32, u32)) -> (f32, f32) {
        let vw = viewport.0.max(1) as f32;
        let vh = viewport.1.max(1) as f32;
        let scale = (vw / self.base_size.0).min(vh / self.base_size.1);
        let sx = (size.x * self.zoom).to_f32() * scale / vw * 2.0;
        let sy = (size.y * self.zoom).to_f32() * scale / vh * 2.0;
        (sx, sy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_maps_to_screen_center() {
        let cam = Camera::at(Vec2::from_f32s(100.0, 50.0));
        let s = cam.world_to_screen(Vec2::from_f32s(100.0, 50.0));
        assert_eq!(s, Vec2::from_f32s(160.0, 90.0));
    }

    #[test]
    fn roundtrip_world_screen() {
        let cam = Camera::at(Vec2::from_f32s(37.0, -12.0)).with_zoom(Fx::from_f32(1.5));
        let w = Vec2::from_f32s(123.0, 45.0);
        let back = cam.screen_to_world(cam.world_to_screen(w));
        assert!((back.x.to_f32() - w.x.to_f32()).abs() < 0.01);
        assert!((back.y.to_f32() - w.y.to_f32()).abs() < 0.01);
    }

    #[test]
    fn zoom_in_shrinks_visible_rect() {
        let cam = Camera::at(Vec2::ZERO);
        let r1 = cam.visible_rect();
        let cam2 = cam.with_zoom(Fx::from_int(2));
        let r2 = cam2.visible_rect();
        assert!(r2.width() < r1.width());
        assert_eq!(r1.width(), Fx::from_int(320));
        assert_eq!(r2.width(), Fx::from_int(160));
    }

    #[test]
    fn zoom_is_clamped() {
        let cam = Camera::new().with_zoom(Fx::from_int(1000));
        assert!(cam.zoom <= Fx::from_int(10));
        let cam = Camera::new().with_zoom(Fx::zero());
        assert!(cam.zoom >= Fx::from_f32(0.1));
    }

    #[test]
    fn center_is_ndc_origin() {
        let cam = Camera::at(Vec2::ZERO);
        let (x, y) = cam.world_to_ndc(Vec2::ZERO, (640, 360));
        assert!(x.abs() < 1e-5, "x={x}");
        assert!(y.abs() < 1e-5, "y={y}");
    }

    #[test]
    fn ndc_corners_map_to_edges() {
        let cam = Camera::at(Vec2::ZERO);
        let vp = (640, 360);
        // Top-left corner in world coordinates
        let tl = cam.screen_to_world(Vec2::ZERO);
        let (x, y) = cam.world_to_ndc(tl, vp);
        assert!((x + 1.0).abs() < 1e-4, "x={x}");
        assert!((y - 1.0).abs() < 1e-4, "y={y}");

        // Bottom-right corner
        let br = cam.screen_to_world(Vec2::from_f32s(320.0, 180.0));
        let (x, y) = cam.world_to_ndc(br, vp);
        assert!((x - 1.0).abs() < 1e-4, "x={x}");
        assert!((y + 1.0).abs() < 1e-4, "y={y}");
    }

    #[test]
    fn letterbox_preserves_aspect_on_wide_viewport() {
        let cam = Camera::at(Vec2::ZERO);
        // Viewport wider than 16:9 → black bars on the sides, center stays centered
        let (x, _y) = cam.world_to_ndc(Vec2::ZERO, (1000, 360));
        assert!(x.abs() < 1e-5);
    }

    #[test]
    fn size_scaling_is_linear_in_zoom() {
        let vp = (640, 360);
        let cam = Camera::new();
        let (w1, _) = cam.world_size_to_ndc(Vec2::from_f32s(10.0, 10.0), vp);
        let cam2 = cam.with_zoom(Fx::from_int(2));
        let (w2, _) = cam2.world_size_to_ndc(Vec2::from_f32s(10.0, 10.0), vp);
        assert!((w2 - w1 * 2.0).abs() < 1e-5, "w1={w1} w2={w2}");
    }
}
