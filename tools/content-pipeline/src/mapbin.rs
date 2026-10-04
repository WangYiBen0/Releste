//! Celeste 地图 `.bin` 解析。
//!
//! # 格式
//! ```text
//! string "CELESTE MAP"
//! string map_name
//! u16    lookup_count
//! repeat: string            （字符串查找表）
//! element                   （根元素，递归）
//! ```
//!
//! `element`：
//! ```text
//! u16 lookup_index          （元素名）
//! u8  attribute_count
//! repeat: u16 lookup_index, encoded_var
//! u16 child_count
//! repeat: element
//! ```
//!
//! `encoded_var` 首字节为类型标签：
//! `0=bool 1=byte 2=i16 3=i32 4=f32 5=lookup_index 6=string 7=rle_string`

use crate::error::PipelineError;

/// 编码变量。
#[derive(Debug, Clone, PartialEq)]
pub enum BinValue {
    Bool(bool),
    Byte(u8),
    Short(i16),
    Int(i32),
    Float(f32),
    /// 指向查找表的索引（已解析为字符串）。
    String(String),
    /// RLE 压缩字符串。
    RleString(String),
}

impl BinValue {
    /// 尽力取整数值（用于 tile / 坐标）。
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            BinValue::Bool(b) => Some(i64::from(*b)),
            BinValue::Byte(b) => Some(i64::from(*b)),
            BinValue::Short(s) => Some(i64::from(*s)),
            BinValue::Int(i) => Some(i64::from(*i)),
            BinValue::Float(f) => Some(*f as i64),
            _ => None,
        }
    }

    /// 尽力取浮点值。
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            BinValue::Float(f) => Some(*f),
            BinValue::Int(i) => Some(*i as f32),
            BinValue::Short(s) => Some(f32::from(*s)),
            BinValue::Byte(b) => Some(f32::from(*b)),
            BinValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// 尽力取字符串。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            BinValue::String(s) | BinValue::RleString(s) => Some(s),
            _ => None,
        }
    }
}

/// 地图元素。
#[derive(Debug, Clone, PartialEq)]
pub struct MapBinElement {
    pub name: String,
    pub attributes: Vec<(String, BinValue)>,
    pub children: Vec<MapBinElement>,
}

impl MapBinElement {
    /// 取属性。
    pub fn attr(&self, key: &str) -> Option<&BinValue> {
        self.attributes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// 按名称取第一个子元素。
    pub fn child(&self, name: &str) -> Option<&MapBinElement> {
        self.children.iter().find(|c| c.name == name)
    }

    /// 按名称取全部子元素。
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a MapBinElement> {
        self.children.iter().filter(move |c| c.name == name)
    }
}

/// 解析后的 Celeste 地图。
#[derive(Debug, Clone, PartialEq)]
pub struct MapBin {
    pub name: String,
    pub lookup: Vec<String>,
    pub root: MapBinElement,
}

/// 二进制读取器。
struct Reader<'a> {
    data: &'a [u8],
    cur: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, cur: 0 }
    }

    fn u8(&mut self) -> Result<u8, PipelineError> {
        let b = *self
            .data
            .get(self.cur)
            .ok_or_else(|| PipelineError::parse("bin", "<map>", "unexpected EOF"))?;
        self.cur += 1;
        Ok(b)
    }

    fn i16(&mut self) -> Result<i16, PipelineError> {
        let a = self.u8()?;
        let b = self.u8()?;
        Ok(i16::from_le_bytes([a, b]))
    }

    fn u16(&mut self) -> Result<u16, PipelineError> {
        Ok(self.i16()? as u16)
    }

    fn i32(&mut self) -> Result<i32, PipelineError> {
        let mut buf = [0u8; 4];
        for slot in &mut buf {
            *slot = self.u8()?;
        }
        Ok(i32::from_le_bytes(buf))
    }

    fn f32(&mut self) -> Result<f32, PipelineError> {
        Ok(f32::from_bits(self.i32()? as u32))
    }

    fn bool(&mut self) -> Result<bool, PipelineError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(PipelineError::parse(
                "bin",
                "<map>",
                format!("invalid bool pattern {other}"),
            )),
        }
    }

    /// 7-bit varint 长度前缀字符串（ASCII）。
    fn string(&mut self) -> Result<String, PipelineError> {
        let mut len: u32 = 0;
        let mut shift = 0;
        loop {
            let b = self.u8()?;
            len |= u32::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 28 {
                return Err(PipelineError::parse("bin", "<map>", "varint too large"));
            }
        }

        let end = self.cur + len as usize;
        let bytes = self
            .data
            .get(self.cur..end)
            .ok_or_else(|| PipelineError::parse("bin", "<map>", "unexpected EOF in string body"))?;
        self.cur = end;
        Ok(bytes.iter().map(|b| *b as char).collect())
    }

    /// RLE 压缩字符串（`i16` 字节数，然后 `(重复次数 u8, 字符 u8)` 对）。
    fn rle_string(&mut self) -> Result<String, PipelineError> {
        let byte_count = self.i16()?;
        let mut out = String::with_capacity((byte_count.max(0) as usize) / 2);
        let mut read = 0i16;
        while read < byte_count {
            let repeat = self.u8()?;
            let ch = self.u8()? as char;
            for _ in 0..repeat {
                out.push(ch);
            }
            read += 2;
        }
        Ok(out)
    }

    /// 读取编码变量（返回类型标签与原始索引，供上层解析查表）。
    fn encoded_var(&mut self) -> Result<EncodedVar, PipelineError> {
        let tag = self.u8()?;
        Ok(match tag {
            0 => EncodedVar::Bool(self.bool()?),
            1 => EncodedVar::Byte(self.u8()?),
            2 => EncodedVar::Short(self.i16()?),
            3 => EncodedVar::Int(self.i32()?),
            4 => EncodedVar::Float(self.f32()?),
            5 => EncodedVar::LookupIndex(self.u16()?),
            6 => EncodedVar::Str(self.string()?),
            7 => EncodedVar::RleStr(self.rle_string()?),
            other => {
                return Err(PipelineError::parse(
                    "bin",
                    "<map>",
                    format!("unknown encoded var type {other}"),
                ))
            }
        })
    }

    fn element(&mut self) -> Result<RawElement, PipelineError> {
        let name_index = self.u16()?;
        let attr_count = self.u8()?;
        let mut attributes = Vec::with_capacity(attr_count as usize);
        for _ in 0..attr_count {
            let key_index = self.u16()?;
            let value = self.encoded_var()?;
            attributes.push((key_index, value));
        }
        let child_count = self.u16()?;
        let mut children = Vec::with_capacity(child_count as usize);
        for _ in 0..child_count {
            children.push(self.element()?);
        }
        Ok(RawElement {
            name_index,
            attributes,
            children,
        })
    }
}

/// 未解析查找表的元素。
struct RawElement {
    name_index: u16,
    attributes: Vec<(u16, EncodedVar)>,
    children: Vec<RawElement>,
}

enum EncodedVar {
    Bool(bool),
    Byte(u8),
    Short(i16),
    Int(i32),
    Float(f32),
    LookupIndex(u16),
    Str(String),
    RleStr(String),
}

impl MapBin {
    /// 从字节解析。
    pub fn parse(data: &[u8]) -> Result<Self, PipelineError> {
        let mut r = Reader::new(data);

        let header = r.string()?;
        if header != "CELESTE MAP" {
            return Err(PipelineError::parse(
                "bin",
                "<map>",
                format!("bad header: {header:?} (expected \"CELESTE MAP\")"),
            ));
        }

        let name = r.string()?;

        let lookup_count = r.u16()?;
        let mut lookup = Vec::with_capacity(lookup_count as usize);
        for _ in 0..lookup_count {
            lookup.push(r.string()?);
        }

        let raw_root = r.element()?;
        let root = resolve(raw_root, &lookup)?;

        Ok(MapBin { name, lookup, root })
    }

    /// 从文件读取。
    pub fn from_file(path: &std::path::Path) -> Result<Self, PipelineError> {
        let data = std::fs::read(path).map_err(|e| PipelineError::io(path.display(), e))?;
        Self::parse(&data)
    }
}

fn resolve(raw: RawElement, lookup: &[String]) -> Result<MapBinElement, PipelineError> {
    let name = lookup
        .get(raw.name_index as usize)
        .cloned()
        .ok_or_else(|| {
            PipelineError::parse(
                "bin",
                "<map>",
                format!("element name index {} out of range", raw.name_index),
            )
        })?;

    let mut attributes = Vec::with_capacity(raw.attributes.len());
    for (key_index, value) in raw.attributes {
        let key = lookup.get(key_index as usize).cloned().ok_or_else(|| {
            PipelineError::parse(
                "bin",
                "<map>",
                format!("attribute key index {key_index} out of range"),
            )
        })?;
        let value = match value {
            EncodedVar::Bool(b) => BinValue::Bool(b),
            EncodedVar::Byte(b) => BinValue::Byte(b),
            EncodedVar::Short(s) => BinValue::Short(s),
            EncodedVar::Int(i) => BinValue::Int(i),
            EncodedVar::Float(f) => BinValue::Float(f),
            EncodedVar::Str(s) => BinValue::String(s),
            EncodedVar::RleStr(s) => BinValue::RleString(s),
            EncodedVar::LookupIndex(i) => {
                BinValue::String(lookup.get(i as usize).cloned().ok_or_else(|| {
                    PipelineError::parse("bin", "<map>", format!("lookup index {i} out of range"))
                })?)
            }
        };
        attributes.push((key, value));
    }

    let mut children = Vec::with_capacity(raw.children.len());
    for child in raw.children {
        children.push(resolve(child, lookup)?);
    }

    Ok(MapBinElement {
        name,
        attributes,
        children,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_string(out: &mut Vec<u8>, s: &str) {
        out.push(s.len() as u8);
        out.extend_from_slice(s.as_bytes());
    }

    /// 构造一个最小但完整的地图：1 个 lookup 项 + 根元素带 1 属性 1 子元素。
    fn build_map() -> Vec<u8> {
        let mut out = Vec::new();
        push_string(&mut out, "CELESTE MAP");
        push_string(&mut out, "TestMap");

        // lookup: ["levels", "name", "value"]
        out.extend_from_slice(&3u16.to_le_bytes());
        push_string(&mut out, "levels");
        push_string(&mut out, "name");
        push_string(&mut out, "value");

        // root: name=0 ("levels"), 1 attr, 0 children
        out.extend_from_slice(&0u16.to_le_bytes());
        out.push(1); // attr count
        out.extend_from_slice(&1u16.to_le_bytes()); // key index -> "name"
        out.push(3); // type int
        out.extend_from_slice(&42i32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // 1 child

        // child: name=2 ("value"), 1 attr (bool), 0 children
        out.extend_from_slice(&2u16.to_le_bytes());
        out.push(1);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.push(0); // type bool
        out.push(1);
        out.extend_from_slice(&0u16.to_le_bytes());

        out
    }

    #[test]
    fn parses_header_name_lookup() {
        let map = MapBin::parse(&build_map()).unwrap();
        assert_eq!(map.name, "TestMap");
        assert_eq!(map.lookup, vec!["levels", "name", "value"]);
    }

    #[test]
    fn resolves_element_and_attributes() {
        let map = MapBin::parse(&build_map()).unwrap();
        assert_eq!(map.root.name, "levels");
        assert_eq!(map.root.attr("name"), Some(&BinValue::Int(42)));

        let child = map.root.child("value").expect("child present");
        assert_eq!(child.attr("name"), Some(&BinValue::Bool(true)));
    }

    #[test]
    fn bad_header_errors() {
        let mut bad = Vec::new();
        push_string(&mut bad, "NOT A MAP");
        assert!(MapBin::parse(&bad).is_err());
    }

    #[test]
    fn truncated_input_errors_not_panics() {
        let full = build_map();
        for cut in 0..full.len() {
            // 不应 panic；允许 Ok/Err 任一。
            let _ = MapBin::parse(&full[..cut]);
        }
    }

    #[test]
    fn value_accessors() {
        let f = BinValue::Float(1.5);
        assert_eq!(f.as_f32(), Some(1.5));
        assert_eq!(f.as_i64(), Some(1));
        assert_eq!(BinValue::String("hi".into()).as_str(), Some("hi"));
    }
}
