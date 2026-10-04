//! 最小 XML 扫描器：只覆盖 `ForegroundTiles.xml` 这一类机器生成的文件。
//!
//! 这个子集非常规则：`<Tileset id path copy ignores>` + `<set mask tiles/>`，
//! 没有嵌套超过两层、没有 CDATA、没有实体引用。这里不引第三方 XML 库
//! （AGENTS.md §15.1：不在 `[workspace.dependencies]` 里的依赖要先讨论），
//! 换成一个小扫描器 + golden 测试钉住真实文件。
//!
//! 已知限制（都是这一类文件的边界，写在这里）：
//! - 属性值必须带引号（单引号或双引号）。
//! - 不支持 `>` 出现在属性值里。
//! - 属性值以 `/` 结尾会被误判成自闭合标签。
//! - 注释 `<!-- ... -->` 整段跳过。

/// 一个 `<Tileset>` 元素：属性 + `<set>` 子元素。
#[derive(Debug, Clone, PartialEq)]
pub struct TilesetElement {
    /// `id` 属性（tileset id，单字符）。
    pub id: String,
    /// `path` 属性（tileset 纹理路径）。
    pub path: Option<String>,
    /// `copy` 属性（继承哪个 tileset 的定义）。
    pub copy: Option<String>,
    /// `ignores` 属性（不视为固体的 tile id 列表）。
    pub ignores: Option<String>,
    /// `<set mask tiles/>` 子元素。
    pub sets: Vec<SetElement>,
}

/// 一个 `<set>` 子元素。
#[derive(Debug, Clone, PartialEq)]
pub struct SetElement {
    /// `mask` 属性（`x0x-111-x1x` 或 `center` / `padding`）。
    pub mask: String,
    /// `tiles` 属性（`0,0;1,0;2,0`）。
    pub tiles: String,
}

/// 解析失败。
#[derive(Debug, thiserror::Error)]
pub enum XmlError {
    /// 标签没有闭合。
    #[error("unterminated tag at byte {0}")]
    Unterminated(usize),
    /// 属性没有值。
    #[error("attribute without a value at byte {0}")]
    MalformedAttribute(usize),
}

/// 解析出文档里的全部 `<Tileset>` 元素（按出现顺序）。
pub fn parse_tilesets(xml: &str) -> Result<Vec<TilesetElement>, XmlError> {
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        if xml[i..].starts_with("<!--") {
            i = skip_comment(xml, i)?;
            continue;
        }
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if xml[i..].starts_with("<Tileset") {
            let (element, next) = parse_tileset(xml, i)?;
            out.push(element);
            i = next;
            continue;
        }
        i += 1;
    }
    Ok(out)
}

/// 跳过一段注释，返回 `-->` 之后的下标。
fn skip_comment(xml: &str, start: usize) -> Result<usize, XmlError> {
    let end = xml[start..]
        .find("-->")
        .ok_or(XmlError::Unterminated(start))?
        + start
        + 3;
    Ok(end)
}

/// 解析一个 `<Tileset ...> ... </Tileset>`（或自闭合的 `<Tileset .../>`），
/// 返回元素与结束后的下标。
fn parse_tileset(xml: &str, start: usize) -> Result<(TilesetElement, usize), XmlError> {
    let open_end = xml[start..]
        .find('>')
        .ok_or(XmlError::Unterminated(start))?
        + start;
    let attributes = &xml[start + 1..open_end];
    let id = attribute(attributes, "id")
        .ok_or(XmlError::MalformedAttribute(start))?
        .to_string();

    let path = attribute(attributes, "path").map(str::to_string);
    let copy = attribute(attributes, "copy").map(str::to_string);
    let ignores = attribute(attributes, "ignores").map(str::to_string);

    // 自闭合（`<Tileset id="n" copy="z" path="scifi"/>`）：没有 body。
    if attributes.ends_with('/') {
        return Ok((
            TilesetElement {
                id,
                path,
                copy,
                ignores,
                sets: Vec::new(),
            },
            open_end + 1,
        ));
    }

    let close = xml[open_end..]
        .find("</Tileset>")
        .ok_or(XmlError::Unterminated(start))?
        + open_end;
    let body = &xml[open_end + 1..close];

    let mut sets = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find("<set") {
        let tail = &rest[at..];
        let self_closing = tail.find("/>").ok_or(XmlError::Unterminated(start))?;
        let inner = &tail[4..self_closing];
        if let (Some(mask), Some(tiles)) = (attribute(inner, "mask"), attribute(inner, "tiles")) {
            sets.push(SetElement {
                mask: mask.to_string(),
                tiles: tiles.to_string(),
            });
        }
        rest = &tail[self_closing + 2..];
    }

    let element = TilesetElement {
        id,
        path,
        copy,
        ignores,
        sets,
    };
    Ok((element, close + "</Tileset>".len()))
}

/// 从一段属性文本里取 `name="value"`（或 `name='value'`）。
fn attribute<'a>(attributes: &'a str, name: &str) -> Option<&'a str> {
    let pattern = format!("{name}=");
    let at = attributes.find(&pattern)?;
    let rest = &attributes[at + pattern.len()..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value = &rest[1..];
    let end = value.find(quote)?;
    Some(&value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<Data>
  <!-- a comment -->
  <Tileset id="z" path="template">
    <set mask="x0x-111-x1x" tiles="0,0;1,0;2,0;3,0"/>
    <set mask="center" tiles="4,0;4,1"/>
    <set mask="padding" tiles="5,0"/>
  </Tileset>
  <Tileset id="3" copy="z" path="snow"/>
</Data>"#;

    #[test]
    fn parses_attributes_and_sets() {
        let tilesets = parse_tilesets(SAMPLE).expect("parses");
        assert_eq!(tilesets.len(), 2);

        let template = &tilesets[0];
        assert_eq!(template.id, "z");
        assert_eq!(template.path.as_deref(), Some("template"));
        assert_eq!(template.sets.len(), 3);
        assert_eq!(template.sets[0].mask, "x0x-111-x1x");
        assert_eq!(template.sets[0].tiles, "0,0;1,0;2,0;3,0");
        assert_eq!(template.sets[1].mask, "center");

        let snow = &tilesets[1];
        assert_eq!(snow.id, "3");
        assert_eq!(snow.copy.as_deref(), Some("z"));
        assert_eq!(snow.path.as_deref(), Some("snow"));
        assert!(snow.sets.is_empty(), "copy-only tileset has no sets");
    }

    #[test]
    fn skips_comments_containing_tags() {
        // 注释里出现 `<Tileset` 不会被当成元素。
        let xml = r#"<Data>
  <!-- <Tileset id="ghost" path="nope"/> -->
  <Tileset id="1" path="dirt"/>
</Data>"#;
        let tilesets = parse_tilesets(xml).expect("parses");
        assert_eq!(tilesets.len(), 1);
        assert_eq!(tilesets[0].id, "1");
    }

    #[test]
    fn single_quoted_attributes_work() {
        let xml = r#"<Data><Tileset id='h' path='grass' ignores='*'/></Data>"#;
        let tilesets = parse_tilesets(xml).expect("parses");
        assert_eq!(tilesets[0].id, "h");
        assert_eq!(tilesets[0].ignores.as_deref(), Some("*"));
    }

    #[test]
    fn self_closing_tilesets_have_no_sets() {
        // 文件最后一个 tileset 就是自闭合的（真实文件钉住这个形状）。
        let xml = r#"<Data>
  <Tileset id="z" path="template">
    <set mask="x0x-111-x1x" tiles="0,0"/>
  </Tileset>
  <Tileset id="n" copy="z" path="scifi"/>
</Data>"#;
        let tilesets = parse_tilesets(xml).expect("parses");
        assert_eq!(tilesets.len(), 2);
        let scifi = &tilesets[1];
        assert_eq!(scifi.id, "n");
        assert_eq!(scifi.copy.as_deref(), Some("z"));
        assert!(scifi.sets.is_empty());
    }

    #[test]
    fn unterminated_tag_is_an_error() {
        assert!(matches!(
            parse_tilesets("<Data><Tileset id=\"1\""),
            Err(XmlError::Unterminated(_))
        ));
    }
}
