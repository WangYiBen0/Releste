//! Sprite batching.
//!
//! # Goal
//! Keep the per-frame render submission budget under 4ms (AGENTS.md 8). The core
//! technique is to sort hand-written draw calls by "layer -> texture" and merge
//! them into a handful of draw calls.
//!
//! # Flow
//! 1. Game logic calls `push()` for a heap of [`SpriteDraw`]s after `begin()` each frame.
//! 2. `sort()` stably sorts by `(layer, atlas)`.
//! 3. `ranges()` yields runs of the same texture -> one draw call per run.
//! 4. `vertices()` expands the sorted draw items into GPU vertices (6 per sprite).

use releste_math::Vec2;
use thiserror::Error;

use crate::camera::Camera;

/// A draw request for a single sprite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteDraw {
    /// Texture group index (assigned by the atlas loader).
    pub atlas: u32,
    /// World-space position (the sprite's top-left corner, after applying the origin offset).
    pub position: Vec2,
    /// World-space size.
    pub size: Vec2,
    /// Texture UV (top-left).
    pub uv_min: (f32, f32),
    /// Texture UV (bottom-right).
    pub uv_max: (f32, f32),
    /// Vertex color (RGBA, premultiplied alpha).
    pub color: [f32; 4],
    /// Draw layer (lower draws first).
    pub layer: i32,
    /// Horizontal flip.
    pub flip_x: bool,
    /// Rotation in radians about `position` (the top-left corner); `0.0` = axis-aligned (fast path).
    ///
    /// Semantically matches XNA `SpriteBatch.Draw(..., rotation, origin: Vector2.Zero,
    /// scale)`: the vanilla `Draw.Line` is exactly a "rotated 1px-tall horizontal bar"
    /// (`pureCeleste.cs:23386-23404`), and the engine uses it to draw `Wire`.
    pub rotation: f32,
}

impl SpriteDraw {
    /// Convenience constructor: opaque white, no flip.
    pub fn new(atlas: u32, position: Vec2, size: Vec2, uv: ((f32, f32), (f32, f32))) -> Self {
        SpriteDraw {
            atlas,
            position,
            size,
            uv_min: uv.0,
            uv_max: uv.1,
            color: [1.0; 4],
            layer: 0,
            flip_x: false,
            rotation: 0.0,
        }
    }

    /// Sets the layer.
    pub fn with_layer(mut self, layer: i32) -> Self {
        self.layer = layer;
        self
    }

    /// Sets the color.
    pub fn with_color(mut self, color: [f32; 4]) -> Self {
        self.color = color;
        self
    }

    /// Horizontal flip.
    pub fn with_flip_x(mut self, flip: bool) -> Self {
        self.flip_x = flip;
        self
    }

    /// Rotates about `position` (radians; screen coordinates have y down, so positive is clockwise).
    pub fn with_rotation(mut self, rotation: f32) -> Self {
        self.rotation = rotation;
        self
    }
}

/// Batch error.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BatchError {
    #[error("batch is full: capacity {capacity}, tried to push {attempted}")]
    Full { capacity: usize, attempted: usize },
    #[error("push before begin()")]
    NotBegun,
}

/// A run of consecutive draws sharing one texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchRange {
    /// Texture group.
    pub atlas: u32,
    /// Start index into `draws`.
    pub start: usize,
    /// Number of elements.
    pub len: usize,
}

/// An expanded vertex (for the wgpu vertex buffer).
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct SpriteVertex {
    /// Normalized device coordinates.
    pub position: [f32; 2],
    /// Texture coordinates.
    pub uv: [f32; 2],
    /// Vertex color.
    pub color: [f32; 4],
}

/// Sprite batch buffer.
#[derive(Debug)]
pub struct SpriteBatch {
    draws: Vec<SpriteDraw>,
    capacity: usize,
    begun: bool,
    /// Whether the sorted order is stale and must be re-sorted.
    dirty: bool,
}

/// Default capacity (enough for a full 320x180 screen of 8x8 tiles plus entities).
pub const DEFAULT_CAPACITY: usize = 16_384;

impl Default for SpriteBatch {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl SpriteBatch {
    /// Creates a batch with the given capacity.
    pub fn new(capacity: usize) -> Self {
        SpriteBatch {
            draws: Vec::with_capacity(capacity),
            capacity,
            begun: false,
            dirty: false,
        }
    }

    /// Begins a new frame (clears the batch).
    pub fn begin(&mut self) {
        self.draws.clear();
        self.begun = true;
        self.dirty = false;
    }

    /// Adds one draw request.
    pub fn push(&mut self, draw: SpriteDraw) -> Result<(), BatchError> {
        if !self.begun {
            return Err(BatchError::NotBegun);
        }
        if self.draws.len() >= self.capacity {
            return Err(BatchError::Full {
                capacity: self.capacity,
                attempted: self.draws.len() + 1,
            });
        }
        self.draws.push(draw);
        self.dirty = true;
        Ok(())
    }

    /// Number of draw items.
    pub fn len(&self) -> usize {
        self.draws.len()
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }

    /// Read-only access.
    pub fn draws(&self) -> &[SpriteDraw] {
        &self.draws
    }

    /// Sorts by `(layer, atlas)`, clustering draws with the same layer and texture.
    ///
    /// The sort is **stable**: within the same layer and texture, submission order is preserved (which determines occlusion).
    pub fn sort(&mut self) {
        if !self.dirty {
            return;
        }
        // A stable sort preserves submission order among equal keys.
        self.draws.sort_by_key(|d| (d.layer, d.atlas));
        self.dirty = false;
    }

    /// Runs of the same texture (one draw call per run).
    ///
    /// Call [`SpriteBatch::sort`] first.
    pub fn ranges(&self) -> Vec<BatchRange> {
        let mut out: Vec<BatchRange> = Vec::new();
        let mut i = 0usize;
        while i < self.draws.len() {
            let atlas = self.draws[i].atlas;
            let start = i;
            while i < self.draws.len() && self.draws[i].atlas == atlas {
                i += 1;
            }
            out.push(BatchRange {
                atlas,
                start,
                len: i - start,
            });
        }
        out
    }

    /// Expands to a vertex array (two triangles = 6 vertices per sprite).
    ///
    /// Call [`SpriteBatch::sort`] first.
    pub fn vertices(&self, camera: &Camera, viewport: (u32, u32)) -> Vec<SpriteVertex> {
        let mut out = Vec::with_capacity(self.draws.len() * 6);

        for d in &self.draws {
            let (mut u0, v0) = d.uv_min;
            let (mut u1, v1) = d.uv_max;
            if d.flip_x {
                std::mem::swap(&mut u0, &mut u1);
            }

            let [bl, br, tl, tr] = if d.rotation == 0.0 {
                let (px, py) = camera.world_to_ndc(d.position, viewport);
                let (sx, sy) = camera.world_size_to_ndc(d.size, viewport);

                // NDC has y up, so the "top" is py and the bottom is py - sy.
                let left = px;
                let right = px + sx;
                let top = py;
                let bottom = py - sy;

                [
                    SpriteVertex {
                        position: [left, bottom],
                        uv: [u0, v1],
                        color: d.color,
                    },
                    SpriteVertex {
                        position: [right, bottom],
                        uv: [u1, v1],
                        color: d.color,
                    },
                    SpriteVertex {
                        position: [left, top],
                        uv: [u0, v0],
                        color: d.color,
                    },
                    SpriteVertex {
                        position: [right, top],
                        uv: [u1, v0],
                        color: d.color,
                    },
                ]
            } else {
                // Rotation: first rotate the four corners about the top-left corner
                // in world coordinates (y down, clockwise positive), then project each
                // one to NDC -- the camera only applies a uniform scale, so the
                // rotation angle is unaffected by the aspect ratio.
                let (sin, cos) = d.rotation.sin_cos();
                let w = d.size.x.to_f32();
                let h = d.size.y.to_f32();
                let corner = |lx: f32, ly: f32| -> SpriteVertex {
                    let (rx, ry) = (lx * cos - ly * sin, lx * sin + ly * cos);
                    let (x, y) =
                        camera.world_to_ndc(d.position + Vec2::from_f32s(rx, ry), viewport);
                    SpriteVertex {
                        position: [x, y],
                        uv: [u0, v0],
                        color: d.color,
                    }
                };
                let tl = corner(0.0, 0.0);
                let mut tr = corner(w, 0.0);
                let mut bl = corner(0.0, h);
                let mut br = corner(w, h);
                tr.uv = [u1, v0];
                bl.uv = [u0, v1];
                br.uv = [u1, v1];
                [bl, br, tl, tr]
            };

            // Triangle 1: bl, br, tl ; triangle 2: tl, br, tr
            out.extend_from_slice(&[bl, br, tl, tl, br, tr]);
        }

        out
    }

    /// Number of draw calls (= number of runs).
    pub fn draw_call_count(&self) -> usize {
        self.ranges().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(atlas: u32, layer: i32) -> SpriteDraw {
        SpriteDraw::new(
            atlas,
            Vec2::from_f32s(0.0, 0.0),
            Vec2::from_f32s(8.0, 8.0),
            ((0.0, 0.0), (0.1, 0.1)),
        )
        .with_layer(layer)
    }

    #[test]
    fn push_requires_begin() {
        let mut b = SpriteBatch::new(8);
        assert_eq!(b.push(draw(0, 0)), Err(BatchError::NotBegun));
        b.begin();
        assert!(b.push(draw(0, 0)).is_ok());
    }

    #[test]
    fn capacity_is_enforced() {
        let mut b = SpriteBatch::new(2);
        b.begin();
        assert!(b.push(draw(0, 0)).is_ok());
        assert!(b.push(draw(0, 0)).is_ok());
        assert_eq!(
            b.push(draw(0, 0)),
            Err(BatchError::Full {
                capacity: 2,
                attempted: 3
            })
        );
    }

    #[test]
    fn begin_clears() {
        let mut b = SpriteBatch::new(8);
        b.begin();
        b.push(draw(0, 0)).unwrap();
        b.begin();
        assert!(b.is_empty());
    }

    #[test]
    fn sort_groups_by_layer_then_atlas() {
        let mut b = SpriteBatch::new(64);
        b.begin();
        // Deliberately interleaved: layer 0 texture 1, layer 1 texture 0, layer 0 texture 0
        b.push(draw(1, 0)).unwrap();
        b.push(draw(0, 1)).unwrap();
        b.push(draw(0, 0)).unwrap();
        b.sort();

        let got: Vec<(i32, u32)> = b.draws().iter().map(|d| (d.layer, d.atlas)).collect();
        assert_eq!(got, vec![(0, 0), (0, 1), (1, 0)]);
    }

    #[test]
    fn sort_is_stable_within_same_key() {
        let mut b = SpriteBatch::new(64);
        b.begin();
        for x in 0..5 {
            let mut d = draw(0, 0);
            d.position = Vec2::from_f32s(x as f32, 0.0);
            b.push(d).unwrap();
        }
        b.sort();
        let xs: Vec<f32> = b.draws().iter().map(|d| d.position.x.to_f32()).collect();
        assert_eq!(xs, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn ranges_merge_same_atlas() {
        let mut b = SpriteBatch::new(64);
        b.begin();
        b.push(draw(0, 0)).unwrap();
        b.push(draw(0, 0)).unwrap();
        b.push(draw(1, 0)).unwrap();
        b.push(draw(0, 0)).unwrap();
        b.sort();

        let ranges = b.ranges();
        // After sorting: 0,0,0,1 -> two runs
        assert_eq!(
            ranges,
            vec![
                BatchRange {
                    atlas: 0,
                    start: 0,
                    len: 3
                },
                BatchRange {
                    atlas: 1,
                    start: 3,
                    len: 1
                },
            ]
        );
        assert_eq!(b.draw_call_count(), 2);
    }

    #[test]
    fn ranges_empty_when_no_draws() {
        let mut b = SpriteBatch::new(8);
        b.begin();
        assert!(b.ranges().is_empty());
        assert_eq!(b.draw_call_count(), 0);
    }

    #[test]
    fn vertices_are_six_per_sprite() {
        let mut b = SpriteBatch::new(8);
        b.begin();
        b.push(draw(0, 0)).unwrap();
        b.push(draw(0, 0)).unwrap();
        let cam = Camera::new();
        assert_eq!(b.vertices(&cam, (320, 180)).len(), 12);
    }

    #[test]
    fn vertices_fill_expected_ndc_quad() {
        let mut b = SpriteBatch::new(8);
        b.begin();
        // Camera centered on the origin, base 320x180: a sprite at (0,0) with size 8x8
        // -> screen (160,90)..(168,98) -> NDC (0,0)..(0.05,-0.0889)
        b.push(draw(0, 0)).unwrap();
        let cam = Camera::new();
        let v = b.vertices(&cam, (320, 180));

        let left = v[0].position[0];
        let bottom = v[0].position[1];
        let right = v[1].position[0];
        let top = v[2].position[1];

        assert!((left - 0.0).abs() < 1e-5, "left={left}");
        assert!((top - 0.0).abs() < 1e-5, "top={top}");
        assert!((right - 0.05).abs() < 1e-4, "right={right}");
        assert!(bottom < top, "bottom must be below top in NDC");
    }

    #[test]
    fn flip_x_swaps_u() {
        let mut b = SpriteBatch::new(8);
        b.begin();
        b.push(draw(0, 0).with_flip_x(true)).unwrap();
        let cam = Camera::new();
        let v = b.vertices(&cam, (320, 180));
        // The bottom-left corner should get uv_max.x
        assert!((v[0].uv[0] - 0.1).abs() < 1e-6, "uv u0={}", v[0].uv[0]);
    }

    #[test]
    fn rotation_turns_the_quad_around_its_anchor() {
        let mut b = SpriteBatch::new(8);
        b.begin();
        // A 10x1 bar rotated 90 deg about its top-left corner (screen y down -> positive angle is clockwise):
        // the edge that should point 10px right must now point straight down.
        b.push(
            SpriteDraw::new(
                0,
                Vec2::from_f32s(0.0, 0.0),
                Vec2::from_f32s(10.0, 1.0),
                ((0.0, 0.0), (1.0, 1.0)),
            )
            .with_rotation(std::f32::consts::FRAC_PI_2),
        )
        .unwrap();
        let cam = Camera::new();
        let v = b.vertices(&cam, (320, 180));
        let tl = v[2]; // Vertex order: bl,br,tl,tl,br,tr
        let tr = v[5];
        // Straight down -> in NDC x stays the same and y decreases (NDC y is up).
        assert!(
            (tr.position[0] - tl.position[0]).abs() < 1e-5,
            "rotated tip must stay on the anchor's vertical, got {} vs {}",
            tr.position[0],
            tl.position[0]
        );
        assert!(
            tr.position[1] < tl.position[1],
            "90 deg CW must point down in NDC, tip y={} anchor y={}",
            tr.position[1],
            tl.position[1]
        );
        // uv is still assigned per corner (the white texel / texture sampling relies on it).
        assert_eq!(tl.uv, [0.0, 0.0]);
        assert_eq!(tr.uv, [1.0, 0.0]);
        assert_eq!(v[0].uv, [0.0, 1.0]); // bl
        assert_eq!(v[1].uv, [1.0, 1.0]); // br
    }

    #[test]
    fn draw_call_count_drops_after_sort() {
        let mut b = SpriteBatch::new(64);
        b.begin();
        // Interleave 4 textures twice -> 8 runs before sorting
        for _ in 0..2 {
            for atlas in 0..4 {
                b.push(draw(atlas, 0)).unwrap();
            }
        }
        assert_eq!(b.draw_call_count(), 8);
        b.sort();
        assert_eq!(b.draw_call_count(), 4, "sorting must merge same-atlas runs");
    }
}
