//! Minimal XML scanner: covers only machine-generated files like `ForegroundTiles.xml`.
//!
//! The subset is very regular: `<Tileset id path copy ignores>` + `<set mask tiles/>`,`
//! never nested more than two levels, no CDATA, no entity references. We do not pull in a third-party XML
//! library (AGENTS.md 15.1: dependencies not in `[workspace.dependencies]` must be discussed first);
//! instead a small scanner plus golden tests pin the real file.
//!
//! Known limitations (all boundaries of this family of files, listed here):
//! - Attribute values must be quoted (single or double quotes).
//! - A `>` inside an attribute value is not supported.
//! - An attribute value ending in `/` is misread as a self-closing tag.
//! - Comments `<!-- ... -->` are skipped whole.

/// A `<Tileset>` element: attributes + `<set>` children.
#[derive(Debug, Clone, PartialEq)]
pub struct TilesetElement {
    /// The `id` attribute (tileset id, single character).
    pub id: String,
    /// The `path` attribute (tileset texture path).
    pub path: Option<String>,
    /// The `copy` attribute (which tileset's definition to inherit).
    pub copy: Option<String>,
    /// The `ignores` attribute (tile ids not treated as solid).
    pub ignores: Option<String>,
    /// `<set mask tiles/>` children.
    pub sets: Vec<SetElement>,
}

/// A `<set>` child element.
#[derive(Debug, Clone, PartialEq)]
pub struct SetElement {
    /// The `mask` attribute (`x0x-111-x1x` or `center` / `padding`).
    pub mask: String,
    /// The `tiles` attribute (`0,0;1,0;2,0`).
    pub tiles: String,
}

/// Parse failure.
#[derive(Debug, thiserror::Error)]
pub enum XmlError {
    /// A tag was not closed.
    #[error("unterminated tag at byte {0}")]
    Unterminated(usize),
    /// An attribute has no value.
    #[error("attribute without a value at byte {0}")]
    MalformedAttribute(usize),
}

/// Parse all `<Tileset>` elements in the document (in order of appearance).
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

/// Skip one comment and return the index after `-->`.
fn skip_comment(xml: &str, start: usize) -> Result<usize, XmlError> {
    let end = xml[start..]
        .find("-->")
        .ok_or(XmlError::Unterminated(start))?
        + start
        + 3;
    Ok(end)
}

/// Parse one `<Tileset ...> ... </Tileset>` (or self-closing `<Tileset .../>`),
/// returning the element and the index after its end.
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

    // Self-closing (`<Tileset id="n" copy="z" path="scifi"/>`): no body.
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

/// Extract `name="value"` (or `name='value'`) from an attribute string.
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
        // A `<Tileset` inside a comment is not treated as an element.
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
        // The last tileset in the file is self-closing (the real file pins this shape).
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
