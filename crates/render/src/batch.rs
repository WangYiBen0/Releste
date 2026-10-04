//! Sprite 批处理。
//!
//! # 目标
//! 单帧渲染提交预算 < 4ms（AGENTS.md §8）。核心手段是把手绘
//! 调用按"层 → 纹理"排序后合并成少数几个 draw call。
//!
//! # 流程
//! 1. 游戏逻辑每帧 `begin()` 后 `push()` 一堆 [`SpriteDraw`]。
//! 2. `sort()` 按 `(layer, atlas)` 稳定排序。
//! 3. `ranges()` 给出相同纹理的连续区段 → 每段一个 draw call。
//! 4. `vertices()` 把排序后的绘制项展开为 GPU 顶点（每 sprite 6 个）。

use reles_math::Vec2;
use thiserror::Error;

use crate::camera::Camera;

/// 单个 sprite 的绘制请求。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteDraw {
    /// 纹理组索引（由图集加载器分配）。
    pub atlas: u32,
    /// 世界坐标位置（sprite 左上角，已应用原点偏移）。
    pub position: Vec2,
    /// 世界坐标尺寸。
    pub size: Vec2,
    /// 纹理 UV（左上）。
    pub uv_min: (f32, f32),
    /// 纹理 UV（右下）。
    pub uv_max: (f32, f32),
    /// 顶点色（RGBA，预乘 alpha）。
    pub color: [f32; 4],
    /// 绘制层（小者先画）。
    pub layer: i32,
    /// 水平翻转。
    pub flip_x: bool,
}

impl SpriteDraw {
    /// 便捷构造：不透明白色、无翻转。
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
        }
    }

    /// 设置层。
    pub fn with_layer(mut self, layer: i32) -> Self {
        self.layer = layer;
        self
    }

    /// 设置颜色。
    pub fn with_color(mut self, color: [f32; 4]) -> Self {
        self.color = color;
        self
    }

    /// 水平翻转。
    pub fn with_flip_x(mut self, flip: bool) -> Self {
        self.flip_x = flip;
        self
    }
}

/// 批处理错误。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BatchError {
    #[error("batch is full: capacity {capacity}, tried to push {attempted}")]
    Full { capacity: usize, attempted: usize },
    #[error("push before begin()")]
    NotBegun,
}

/// 相同纹理的连续区段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchRange {
    /// 纹理组。
    pub atlas: u32,
    /// `draws` 中的起始下标。
    pub start: usize,
    /// 元素个数。
    pub len: usize,
}

/// 展开后的顶点（供 wgpu 顶点缓冲使用）。
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct SpriteVertex {
    /// 归一化设备坐标。
    pub position: [f32; 2],
    /// 纹理坐标。
    pub uv: [f32; 2],
    /// 顶点色。
    pub color: [f32; 4],
}

/// Sprite 批处理缓冲。
#[derive(Debug)]
pub struct SpriteBatch {
    draws: Vec<SpriteDraw>,
    capacity: usize,
    begun: bool,
    /// 排序后是否仍需要重新排序。
    dirty: bool,
}

/// 默认容量（够一屏 320x180 下铺满 8x8 tile + 实体）。
pub const DEFAULT_CAPACITY: usize = 16_384;

impl Default for SpriteBatch {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl SpriteBatch {
    /// 指定容量。
    pub fn new(capacity: usize) -> Self {
        SpriteBatch {
            draws: Vec::with_capacity(capacity),
            capacity,
            begun: false,
            dirty: false,
        }
    }

    /// 开始新一帧（清空）。
    pub fn begin(&mut self) {
        self.draws.clear();
        self.begun = true;
        self.dirty = false;
    }

    /// 加入一个绘制请求。
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

    /// 绘制项数量。
    pub fn len(&self) -> usize {
        self.draws.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }

    /// 只读访问。
    pub fn draws(&self) -> &[SpriteDraw] {
        &self.draws
    }

    /// 按 `(layer, atlas)` 排序，把同层同纹理的绘制项聚到一起。
    ///
    /// 排序是**稳定**的：同层同纹理内保持提交顺序（决定遮挡）。
    pub fn sort(&mut self) {
        if !self.dirty {
            return;
        }
        // 稳定排序保持同 key 的提交顺序。
        self.draws.sort_by_key(|d| (d.layer, d.atlas));
        self.dirty = false;
    }

    /// 相同纹理的连续区段（每段一次 draw call）。
    ///
    /// 调用前应先 [`SpriteBatch::sort`]。
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

    /// 展开为顶点数组（每 sprite 两个三角形 = 6 个顶点）。
    ///
    /// 调用前应先 [`SpriteBatch::sort`]。
    pub fn vertices(&self, camera: &Camera, viewport: (u32, u32)) -> Vec<SpriteVertex> {
        let mut out = Vec::with_capacity(self.draws.len() * 6);

        for d in &self.draws {
            let (px, py) = camera.world_to_ndc(d.position, viewport);
            let (sx, sy) = camera.world_size_to_ndc(d.size, viewport);

            // NDC 的 y 向上，因此"顶部"是 py，底部是 py - sy。
            let left = px;
            let right = px + sx;
            let top = py;
            let bottom = py - sy;

            let (mut u0, v0) = d.uv_min;
            let (mut u1, v1) = d.uv_max;
            if d.flip_x {
                std::mem::swap(&mut u0, &mut u1);
            }

            let bl = SpriteVertex {
                position: [left, bottom],
                uv: [u0, v1],
                color: d.color,
            };
            let br = SpriteVertex {
                position: [right, bottom],
                uv: [u1, v1],
                color: d.color,
            };
            let tl = SpriteVertex {
                position: [left, top],
                uv: [u0, v0],
                color: d.color,
            };
            let tr = SpriteVertex {
                position: [right, top],
                uv: [u1, v0],
                color: d.color,
            };

            // 三角形 1: bl, br, tl ; 三角形 2: tl, br, tr
            out.extend_from_slice(&[bl, br, tl, tl, br, tr]);
        }

        out
    }

    /// draw call 数量（= 区段数）。
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
        // 故意交错：层 0 纹理 1、层 1 纹理 0、层 0 纹理 0
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
        // 排序后：0,0,0,1 → 两段
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
        // 摄像机居中于原点，基准 320x180：sprite 在 (0,0) 尺寸 8x8
        // → 屏幕 (160,90)..(168,98) → NDC (0,0)..(0.05,-0.0889)
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
        // 左下角应该拿到 uv_max.x
        assert!((v[0].uv[0] - 0.1).abs() < 1e-6, "uv u0={}", v[0].uv[0]);
    }

    #[test]
    fn draw_call_count_drops_after_sort() {
        let mut b = SpriteBatch::new(64);
        b.begin();
        // 交错 4 个纹理两次 → 未排序 8 段
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
