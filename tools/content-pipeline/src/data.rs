//! `Celeste .data` texture decoding.
//!
//! # Source format
//!
//! This tool previously treated `.data` as Crunch-compressed -- **that was wrong**. The Monocle branch
//! called `Crunch*` actually reads a `.png`; `.data` belongs to `AtlasDataFormat.Packer`
//! /`PackerNoAtlas` and is a per-command **pixel RLE**:
//!
//! ```text
//! i32 le  width
//! i32 le  height
//! u8      mode
//! Repeat the following command until exactly width*height pixels have been written:
//!   u8 count                                  # pixels covered by this command (1..=255)
//!   mode == 1 (with alpha):
//!     u8 a
//!     a > 0 -> [u8 b][u8 g][u8 r]             # 5 bytes: (r,g,b,a) x count
//!     a == 0 ->                               # 2 bytes: (0,0,0,0) x count
//!   mode != 1 (opaque):
//!     [u8 b][u8 g][u8 r]                      # 4 bytes: (r,g,b,255) x count
//! ```
//!
//! Output is row-major, top-down **premultiplied alpha** RGBA8, exactly `width*height*4`
//! bytes; the command stream must be consumed **completely**; one byte too many or too few counts as corruption.
//!
//! Vanilla feeds this bitmap straight to `Texture2D(..).SetData(..)` (`SurfaceFormat.Color`
//! = RGBA8), so premultiplication is part of the content itself. Keep it as-is when converting to PNG; the
//! renderer can restore it with `PREMULTIPLIED_ALPHA_BLENDING`.
//!
//! Reference: `references/pureCeleste.cs:25469-25547` (the `.data` branch).
//!
//! # Provenance
//! Original formats are only allowed in `tools/content-pipeline` (AGENTS.md 4.6 / 12):
//! decode to PNG offline, so the runtime never sees `.data`.

use std::path::Path;

use crate::error::PipelineError;

/// Header length: `[i32 width][i32 height][u8 mode]`.
const HEADER: usize = 9;

/// Pixel cap matching vanilla's static buffer (64 MiB).
const MAX_PIXELS: usize = 64 * 1024 * 1024 / 4;

/// One decoded texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedTexture {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Byte-by-byte RGBA8, row-major, top-down, **premultiplied alpha**.
    pub rgba: Vec<u8>,
}

impl DecodedTexture {
    /// Total pixel count.
    pub fn pixels(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }
}

/// Decode one `.data` file.
///
/// I/O errors and format errors are both returned with context; this never panics.
pub fn decode_file(path: &Path) -> Result<DecodedTexture, PipelineError> {
    let bytes = std::fs::read(path).map_err(|e| PipelineError::io(path.display(), e))?;
    decode(&bytes, &path.display().to_string())
}

/// Decode a `.data` byte stream.
///
/// `where_` is only used in error messages (usually the file path).
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

        // Read all the bytes of this command before writing pixels: this way `count == 0` just means "draw nothing"
        // instead of leaving `pos` stuck in place and causing an infinite loop.
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
                // The payload order is B,G,R
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

    /// Build a mode==1 (with alpha) stream.
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

    /// Build a mode==0 (opaque) stream.
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
        // 2x2, all the same color
        let bytes = pack_opaque(2, 2, &[(4, 0x10, 0x20, 0x30)]);
        let tex = decode(&bytes, "<test>").unwrap();
        assert_eq!((tex.width, tex.height), (2, 2));
        let expected: Vec<u8> = (0..4).flat_map(|_| [0x30, 0x20, 0x10, 0xff]).collect();
        assert_eq!(tex.rgba, expected);
    }

    #[test]
    fn decodes_alpha_run_and_transparency() {
        // 3 pixels: opaque red x2, transparent x1
        let bytes = pack_alpha(3, 1, &[(2, 0xaa, 0x01, 0x02, 0x03), (1, 0, 0, 0, 0)]);
        let tex = decode(&bytes, "<test>").unwrap();
        assert_eq!(
            tex.rgba,
            vec![0x03, 0x02, 0x01, 0xaa, 0x03, 0x02, 0x01, 0xaa, 0, 0, 0, 0]
        );
    }

    #[test]
    fn rejects_truncated_command_stream() {
        // Claims 4 pixels but only supplies the header of 1 command
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
        // After count == 0 we must keep reading until all 4 pixels are drawn
        let mut bytes = pack_alpha(2, 2, &[(0, 0xff, 1, 2, 3)]);
        // Append a command that actually draws all 4 pixels
        bytes.push(4);
        bytes.push(0xff);
        bytes.extend_from_slice(&[1, 2, 3]);
        let tex = decode(&bytes, "<test>").unwrap();
        assert_eq!(tex.pixels(), 4);
        assert_eq!(&tex.rgba[0..4], &[3, 2, 1, 0xff]);
    }

    /// Golden-check against the real conversion output: `assets/` is a build input (gitignored),
    /// skipped when absent, and `nix build .#checks.unit-tests` still passes.
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

        // Premultiplied invariant: RGB is scaled by alpha, so no channel may exceed A.
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
