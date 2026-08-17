//! Just enough XML to read a URDF, in `std` alone.
//!
//! URDF is a small, well-behaved dialect: elements, attributes, comments, and text nobody
//! reads. It has no namespaces to resolve, no DTD to fetch, no processing instructions beyond
//! the prolog, and no mixed content that matters. So this parses that dialect and refuses
//! anything else by name, rather than pulling in a general XML stack to read four tag types.

use std::fmt;

/// One element: a name, its attributes, and its children.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    /// The first child with this tag name.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }
    /// Every child with this tag name.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
    /// A whitespace-separated list of numbers, as URDF writes vectors.
    pub fn attr_vec(&self, name: &str) -> Option<Vec<f64>> {
        let raw = self.attr(name)?;
        raw.split_whitespace()
            .map(|t| t.parse::<f64>().ok())
            .collect()
    }
    pub fn attr_f64(&self, name: &str) -> Option<f64> {
        self.attr(name)?.parse().ok()
    }
}

/// What the parser refused, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlError {
    pub at: usize,
    pub what: String,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "XML error at byte {}: {}", self.at, self.what)
    }
}

impl std::error::Error for XmlError {}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn err<T>(&self, what: impl Into<String>) -> Result<T, XmlError> {
        Err(XmlError {
            at: self.i,
            what: what.into(),
        })
    }
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn starts(&self, s: &str) -> bool {
        self.b[self.i..].starts_with(s.as_bytes())
    }
    /// Skip comments, the prolog, DOCTYPE and processing instructions, plus any whitespace.
    fn trivia(&mut self) -> Result<(), XmlError> {
        loop {
            self.ws();
            if self.starts("<!--") {
                match find(self.b, self.i + 4, b"-->") {
                    Some(j) => self.i = j + 3,
                    None => return self.err("unterminated comment"),
                }
            } else if self.starts("<?") || self.starts("<!") {
                match find(self.b, self.i, b">") {
                    Some(j) => self.i = j + 1,
                    None => return self.err("unterminated declaration"),
                }
            } else {
                return Ok(());
            }
        }
    }
    fn name(&mut self) -> Result<String, XmlError> {
        let start = self.i;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':') {
                self.i += 1;
            } else {
                break;
            }
        }
        if self.i == start {
            return self.err("expected a name");
        }
        Ok(String::from_utf8_lossy(&self.b[start..self.i]).into_owned())
    }
    fn quoted(&mut self) -> Result<String, XmlError> {
        let q = *self.b.get(self.i).unwrap_or(&0);
        if q != b'"' && q != b'\'' {
            return self.err("attribute values must be quoted");
        }
        self.i += 1;
        let start = self.i;
        while self.i < self.b.len() && self.b[self.i] != q {
            self.i += 1;
        }
        if self.i >= self.b.len() {
            return self.err("unterminated attribute value");
        }
        let raw = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
        self.i += 1;
        Ok(unescape(&raw))
    }
    fn element(&mut self) -> Result<Element, XmlError> {
        self.trivia()?;
        if !self.starts("<") {
            return self.err("expected an element");
        }
        self.i += 1;
        let name = self.name()?;
        let mut el = Element {
            name,
            ..Default::default()
        };
        loop {
            self.ws();
            if self.starts("/>") {
                self.i += 2;
                return Ok(el);
            }
            if self.starts(">") {
                self.i += 1;
                break;
            }
            let k = self.name()?;
            self.ws();
            if !self.starts("=") {
                return self.err(format!("attribute {k:?} has no value"));
            }
            self.i += 1;
            self.ws();
            let v = self.quoted()?;
            el.attrs.push((k, v));
        }
        // Children, until the closing tag. Text content is skipped: URDF puts nothing in it
        // that a scene needs.
        loop {
            self.trivia()?;
            if self.starts("</") {
                self.i += 2;
                let close = self.name()?;
                if close != el.name {
                    return self.err(format!("</{close}> closes <{}>", el.name));
                }
                self.ws();
                if !self.starts(">") {
                    return self.err("unterminated closing tag");
                }
                self.i += 1;
                return Ok(el);
            }
            if self.starts("<") {
                let c = self.element()?;
                el.children.push(c);
            } else if self.i >= self.b.len() {
                return self.err(format!("<{}> is never closed", el.name));
            } else {
                // Text: skip to the next tag.
                match find(self.b, self.i, b"<") {
                    Some(j) => self.i = j,
                    None => return self.err(format!("<{}> is never closed", el.name)),
                }
            }
        }
    }
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= b.len() {
        return None;
    }
    b[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Parse a document and return its root element.
pub fn parse(text: &str) -> Result<Element, XmlError> {
    let mut p = P {
        b: text.as_bytes(),
        i: 0,
    };
    let root = p.element()?;
    p.trivia()?;
    if p.i < p.b.len() {
        return p.err("trailing content after the root element");
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_urdf_shaped_document_parses() {
        let x = parse(
            r#"<?xml version="1.0"?>
            <!-- a comment -->
            <robot name="arm">
              <link name="base">
                <visual>
                  <origin xyz="0 0 0.05" rpy="0 0 0"/>
                  <geometry><box size="0.2 0.2 0.1"/></geometry>
                  <material name="grey"><color rgba="0.5 0.5 0.5 1"/></material>
                </visual>
              </link>
              <joint name="j1" type="revolute">
                <parent link="base"/><child link="upper"/>
                <origin xyz="0 0 0.1"/><axis xyz="0 0 1"/>
                <limit lower="-1.57" upper="1.57" effort="10" velocity="1"/>
              </joint>
            </robot>"#,
        )
        .unwrap();
        assert_eq!(x.name, "robot");
        assert_eq!(x.attr("name"), Some("arm"));
        let link = x.child("link").unwrap();
        let vis = link.child("visual").unwrap();
        assert_eq!(
            vis.child("origin").unwrap().attr_vec("xyz").unwrap(),
            vec![0.0, 0.0, 0.05]
        );
        assert_eq!(
            vis.child("geometry")
                .unwrap()
                .child("box")
                .unwrap()
                .attr_vec("size")
                .unwrap(),
            vec![0.2, 0.2, 0.1]
        );
        let j = x.child("joint").unwrap();
        assert_eq!(j.attr("type"), Some("revolute"));
        assert_eq!(j.child("parent").unwrap().attr("link"), Some("base"));
        assert_eq!(j.child("limit").unwrap().attr_f64("lower"), Some(-1.57));
    }

    #[test]
    fn single_quotes_entities_and_self_closing_all_work() {
        let x = parse(r#"<a p='1' q="a &amp; b"><b/><b/></a>"#).unwrap();
        assert_eq!(x.attr("p"), Some("1"));
        assert_eq!(x.attr("q"), Some("a & b"));
        assert_eq!(x.children_named("b").count(), 2);
    }

    #[test]
    fn text_content_is_skipped_rather_than_choking() {
        let x = parse("<a>some text<b/>more text</a>").unwrap();
        assert_eq!(x.children.len(), 1);
        assert_eq!(x.children[0].name, "b");
    }

    #[test]
    fn malformed_documents_are_refused_with_a_position() {
        for (doc, hint) in [
            ("<a>", "never closed"),
            ("<a></b>", "closes"),
            ("<a x=1/>", "quoted"),
            ("<a/><b/>", "trailing"),
            ("<!-- unterminated", "comment"),
        ] {
            let e = parse(doc).unwrap_err();
            assert!(
                e.what.contains(hint),
                "{doc:?} should mention {hint:?}, said {:?}",
                e.what
            );
        }
    }

    #[test]
    fn a_vector_attribute_with_a_bad_number_is_none_not_zero() {
        let x = parse(r#"<o xyz="0 nope 1"/>"#).unwrap();
        assert_eq!(
            x.attr_vec("xyz"),
            None,
            "a typo must not silently read as 0"
        );
    }
}
