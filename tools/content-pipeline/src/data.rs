//! `Celeste .data` 纹理解码。
//!
//! # 源格式
//!
//! 此前本工具把 `.data` 当作 Crunch 压缩——**那是错的**。Monocle 里叫
//! `Crunch*` 的分支实际读的是 `.png`；`.data` 属于 `AtlasDataFormat.Packer`
//! /`PackerNoAtlas`，是一种逐命令的**像素 RLE**：
//!
//! ```text
//! i32 le  width
//! i32 le  height
//! u8      mode
//! 重复以下命令，直到正好写出 width*height 个像素：
//!   u8 count                                  # 本条命令覆盖的像素数（1..=255）
//!   mode == 1（带 alpha）：
//!     u8 a
//!     a > 0 → [u8 b][u8 g][u8 r]              # 5 字节：(r,g,b,a) × count
//!     a == 0 →                               # 2 字节：(0,0,0,0) × count
//!   mode != 1（不透明）：
//!     [u8 b][u8 g][u8 r]                      # 4 字节：(r,g,b,255) × count
//! ```
//!
//! 输出为行主序、自上而下的 **预乘 alpha** RGBA8，正好 `width*height*4`
//! 字节；命令流必须被**完全**消费，多一字节少一字节都算损坏。
//!
//! 原版把这张位图原样喂给 `Texture2D(..).SetData(..)`（`SurfaceFormat.Color`
//! = RGBA8），所以预乘是内容本身的一部分。转成 PNG 时保留原样，渲染端用
//! `PREMULTIPLIED_ALPHA_BLENDING` 即可还原。
//!
//! 参考：`references/pureCeleste.cs:25469-25547`（`.data` 分支）。
//!
//! # 归属
//! 原始格式只允许出现在 `tools/content-pipeline`（AGENTS.md §4.6 / §12）：
//! 离线解码成 PNG，运行时永远看不到 `.data`。

use std::path::Path;

use crate::error::PipelineError;

/// 头部长度：`[i32 width][i32 height][u8 mode]`。
const HEADER: usize = 9;

/// 与原版静态缓冲区（64 MiB）一致的像素上限。
const MAX_PIXELS: usize = 64 * 1024 * 1024 / 4;

/// 解码后的单张纹理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedTexture {
    /// 像素宽度。
    pub width: u32,
    /// 像素高度。
    pub height: u32,
    /// 逐字节 RGBA8，行主序、自上而下、**预乘 alpha**。
    pub rgba: Vec<u8>,
}

impl DecodedTexture {
    /// 像素总数。
    pub fn pixels(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }
}

/// 解码一个 `.data` 文件。
///
/// I/O 错误与格式错误都会带上下文返回，绝不 panic。
pub fn decode_file(path: &Path) -> Result<DecodedTexture, PipelineError> {
    let bytes = std::fs::read(path).map_err(|e| PipelineError::io(path.display(), e))?;
    decode(&bytes, &path.display().to_string())
}

/// 解码 `.data` 字节流。
///
/// `where_` 只用于错误信息（通常是文件路径）。
pub fn decode(bytes: &[u8], where_: &str) -> Result<DecodedTexture, PipelineError> {
    if bytes.len() < HEADER {
        return Err(PipelineError::parse(
            "data",
            where_,
            format!(
                "file is {} bytes, shorter than the {HEADER}-byte header",
                bytes.len()
            ),
        ));
    }

    let width = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let height = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let mode = bytes[8];

    if width <= 0 || height <= 0 {
        return Err(PipelineError::parse(
            "data",
            where_,
            format!("bad dimensions {width}x{height}"),
        ));
    }
    let Some(pixels) = (width as usize).checked_mul(height as usize) else {
        return Err(PipelineError::parse("data", where_, "dimensions overflow"));
    };
    if pixels > MAX_PIXELS {
        return Err(PipelineError::parse(
            "data",
            where_,
            format!("{width}x{height} exceeds the {MAX_PIXELS}-pixel budget"),
        ));
    }

    let out_len = pixels * 4;
    let mut out = vec![0u8; out_len];

    let mut pos = HEADER;
    let mut dst = 0usize;

    while dst < out_len {
        let count = *bytes
            .get(pos)
            .ok_or_else(|| PipelineError::parse("data", where_, "truncated command stream"))?
            as usize;
        pos += 1;

        // 先把这条命令的字节读完，再写像素：这样 `count == 0` 只是"不画"，
        // 而不会让 `pos` 卡在原地造成死循环。
        let pixel = if mode == 1 {
            let alpha = *bytes
                .get(pos)
                .ok_or_else(|| PipelineError::parse("data", where_, "truncated command stream"))?;
            pos += 1;
            if alpha == 0 {
                [0u8, 0, 0, 0]
            } else {
                let bgr = bytes
                    .get(pos..pos + 3)
                    .ok_or_else(|| PipelineError::parse("data", where_, "truncated RGB triple"))?;
                pos += 3;
                // 载荷顺序是 B,G,R
                [bgr[2], bgr[1], bgr[0], alpha]
            }
        } else {
            let bgr = bytes
                .get(pos..pos + 3)
                .ok_or_else(|| PipelineError::parse("data", where_, "truncated RGB triple"))?;
            pos += 3;
            [bgr[2], bgr[1], bgr[0], 255]
        };

        let run = count * 4;
        if dst + run > out_len {
            return Err(PipelineError::parse(
                "data",
                where_,
                format!(
                    "run of {count} pixels overflows the image (dst {dst} + {run} > {out_len})"
                ),
            ));
        }
        for i in 0..count {
            let o = dst + i * 4;
            out[o..o + 4].copy_from_slice(&pixel);
        }
        dst += run;
    }

    if pos != bytes.len() {
        return Err(PipelineError::parse(
            "data",
            where_,
            format!(
                "{} trailing byte(s) after the command stream (consumed {pos} of {})",
                bytes.len() - pos,
                bytes.len()
            ),
        ));
    }

    Ok(DecodedTexture {
        width: width as u32,
        height: height as u32,
        rgba: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 mode==1（带 alpha）流。
    fn pack_alpha(width: i32, height: i32, runs: &[(u8, u8, u8, u8, u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.push(1);
        for &(count, a, b, g, r) in runs {
            out.push(count);
            out.push(a);
            if a > 0 {
                out.extend_from_slice(&[b, g, r]);
            }
        }
        out
    }

    /// 构造 mode==0（不透明）流。
    fn pack_opaque(width: i32, height: i32, runs: &[(u8, u8, u8, u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.push(0);
        for &(count, b, g, r) in runs {
            out.push(count);
            out.extend_from_slice(&[b, g, r]);
        }
        out
    }

    #[test]
    fn decodes_opaque_run() {
        // 2x2 全部同色
        let bytes = pack_opaque(2, 2, &[(4, 0x10, 0x20, 0x30)]);
        let tex = decode(&bytes, "<test>").unwrap();
        assert_eq!((tex.width, tex.height), (2, 2));
        let expected: Vec<u8> = (0..4).flat_map(|_| [0x30, 0x20, 0x10, 0xff]).collect();
        assert_eq!(tex.rgba, expected);
    }

    #[test]
    fn decodes_alpha_run_and_transparency() {
        // 3 像素：不透明红 ×2，透明 ×1
        let bytes = pack_alpha(3, 1, &[(2, 0xaa, 0x01, 0x02, 0x03), (1, 0, 0, 0, 0)]);
        let tex = decode(&bytes, "<test>").unwrap();
        assert_eq!(
            tex.rgba,
            vec![0x03, 0x02, 0x01, 0xaa, 0x03, 0x02, 0x01, 0xaa, 0, 0, 0, 0]
        );
    }

    #[test]
    fn rejects_truncated_command_stream() {
        // 声称 4 个像素但只给了 1 个命令的头部
        let bytes = pack_alpha(2, 2, &[(2, 0xff, 0, 0, 0)]);
        assert!(decode(&bytes, "<test>").is_err());
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = pack_opaque(2, 2, &[(4, 0, 0, 0)]);
        bytes.push(0xff);
        let err = decode(&bytes, "<test>").unwrap_err();
        assert!(err.to_string().contains("trailing"));
    }

    #[test]
    fn rejects_run_that_overflows_the_image() {
        let bytes = pack_opaque(2, 2, &[(5, 0, 0, 0)]);
        let err = decode(&bytes, "<test>").unwrap_err();
        assert!(err.to_string().contains("overflows"));
    }

    #[test]
    fn rejects_short_header() {
        assert!(decode(&[1, 0, 0, 0], "<test>").is_err());
    }

    #[test]
    fn zero_count_command_is_consumed_without_writing() {
        // count == 0 后面必须继续读，直到把 4 个像素画完
        let mut bytes = pack_alpha(2, 2, &[(0, 0xff, 1, 2, 3)]);
        // 追加真正画满 4 像素的命令
        bytes.push(4);
        bytes.push(0xff);
        bytes.extend_from_slice(&[1, 2, 3]);
        let tex = decode(&bytes, "<test>").unwrap();
        assert_eq!(tex.pixels(), 4);
        assert_eq!(&tex.rgba[0..4], &[3, 2, 1, 0xff]);
    }

    /// 用真实转换产物做黄金校验：`assets/` 是构建输入（gitignored），
    /// 缺失时跳过，`nix build .#checks.unit-tests` 仍能过。
    #[test]
    fn decodes_real_gameplay_page_when_present() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/atlas/Gameplay/Gameplay0.data");
        if !path.is_file() {
            eprintln!("skipping: {} is absent", path.display());
            return;
        }
        let tex = decode_file(&path).unwrap();
        assert_eq!((tex.width, tex.height), (4096, 4096));
        assert_eq!(tex.rgba.len(), 4096 * 4096 * 4);

        // 预乘不变式：RGB 是按 alpha 缩过的，所以任意通道都不得超过 A。
        let rgba = &tex.rgba;
        let (mut violations, mut i) = (0usize, 0usize);
        while i + 3 < rgba.len() {
            let (r, g, b, a) = (rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]);
            if a != 0 && a != 255 && (r > a || g > a || b > a) {
                violations += 1;
            }
            i += 4;
        }
        assert_eq!(violations, 0, "premultiplied-alpha invariant broken");
    }
}
