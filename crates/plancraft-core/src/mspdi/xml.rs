//! A small, bounded XML reader for MSPDI files, and a writer for them.
//!
//! The reader streams the document and hands over one *record* at a time (a `Task`, `Resource`,
//! `Assignment`, `Calendar` or `ExtendedAttribute` definition: an element inside one of the
//! collections under `Project`), so memory stays proportional to the plan rather than to the
//! file. Direct children of `Project` that hold a value arrive as scalars. Input is hostile:
//! - entities declared in a DOCTYPE are never expanded (only the five predefined entities and
//!   character references are understood), so "billion laughs" files cannot blow up;
//! - nesting depth, elements per record and text per element are capped;
//! - subtrees PlanCraft never reads (timephased data, outline codes) are skipped unstored.

use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};

/// Deepest nesting accepted (MSPDI needs 6).
const MAX_DEPTH: usize = 32;
/// Most elements kept in one record.
const MAX_RECORD_ELEMENTS: usize = 200_000;
/// Most bytes of text in one element.
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;

/// Subtrees that are skipped without being stored.
const SKIPPED: [&str; 3] = ["TimephasedData", "OutlineCode", "OutlineCodes"];

/// An element with its text and child elements (namespace prefixes dropped).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub name: String,
    pub text: String,
    pub children: Vec<Element>,
}

impl Element {
    /// The first child named `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every child named `name`.
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The trimmed text of the first child named `name`, when it has any.
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).and_then(Element::value)
    }

    /// The trimmed text of this element (`None` when empty).
    pub fn value(&self) -> Option<&str> {
        Some(self.text.trim()).filter(|t| !t.is_empty())
    }
}

/// What the reader hands over.
#[derive(Debug)]
pub enum Item {
    /// A direct child of `Project` (`<Name>…</Name>`), with its text.
    Scalar(String, String),
    /// A record: an element inside a collection (`Tasks/Task`), with everything under it.
    Record { collection: String, element: Element },
}

/// An open element: the element, and whether child elements were seen (its text is then only
/// layout whitespace and is not kept).
struct Open {
    element: Element,
    has_children: bool,
}

/// Reads an MSPDI document, calling `on` for every scalar and record in document order.
pub fn read(text: &str, mut on: impl FnMut(Item) -> Result<(), String>) -> Result<(), String> {
    let mut reader = Reader::from_str(text.trim_start_matches('\u{feff}'));
    reader.config_mut().check_end_names = true;
    let mut stack: Vec<Open> = Vec::new();
    // Inside a skipped subtree: how many of its elements are open.
    let mut skipping = 0usize;
    let mut record_elements = 0usize;
    let mut root_done = false;
    loop {
        let pos = reader.buffer_position();
        let event = reader.read_event().map_err(|e| format!("this is not valid XML (near byte {pos}): {e}"))?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                if skipping > 0 {
                    skipping = skipping.saturating_add(1);
                    continue;
                }
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if open(&mut stack, name, root_done, &mut record_elements)? {
                    skipping = 1;
                }
            }
            Event::Empty(e) => {
                if skipping > 0 {
                    continue;
                }
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if !open(&mut stack, name, root_done, &mut record_elements)? {
                    root_done |= close(&mut stack, &mut on)?;
                }
            }
            Event::End(_) => {
                if skipping > 0 {
                    skipping -= 1;
                    continue;
                }
                root_done |= close(&mut stack, &mut on)?;
            }
            Event::Text(t) if skipping == 0 => {
                let s = t.xml10_content().map_err(|e| format!("this is not valid UTF-8 text: {e}"))?;
                append(&mut stack, &s)?;
            }
            Event::CData(c) if skipping == 0 => {
                let s = c.decode().map_err(|e| format!("this is not valid UTF-8 text: {e}"))?;
                append(&mut stack, &s)?;
            }
            Event::GeneralRef(r) if skipping == 0 => {
                let c = entity(&r)?;
                append(&mut stack, c.encode_utf8(&mut [0u8; 4]))?;
            }
            // Declarations, comments, processing instructions and a DOCTYPE carry nothing
            // PlanCraft reads (entities a DOCTYPE declares are refused where they are used).
            _ => {}
        }
    }
    if !root_done {
        return Err("the file ended before the end of the Project element".into());
    }
    Ok(())
}

/// Opens an element. Returns true when it starts a skipped subtree (nothing was pushed).
fn open(stack: &mut Vec<Open>, name: String, root_done: bool, record_elements: &mut usize) -> Result<bool, String> {
    let depth = stack.len();
    if depth == 0 {
        if root_done {
            return Err("there is more than one top-level element".into());
        }
        if name != "Project" {
            return Err(format!("this is not a Microsoft Project XML file (its top-level element is <{name}>, not <Project>)"));
        }
    }
    if depth >= MAX_DEPTH {
        return Err(format!("elements are nested more than {MAX_DEPTH} levels deep"));
    }
    if depth >= 1 && SKIPPED.contains(&name.as_str()) {
        return Ok(true);
    }
    if depth == 2 {
        *record_elements = 0;
    }
    if depth >= 2 {
        *record_elements = record_elements.saturating_add(1);
        if *record_elements > MAX_RECORD_ELEMENTS {
            return Err(format!("a <{name}> record has more than {MAX_RECORD_ELEMENTS} elements"));
        }
    }
    if let Some(parent) = stack.last_mut() {
        parent.has_children = true;
        parent.element.text.clear();
    }
    stack.push(Open { element: Element { name, ..Element::default() }, has_children: false });
    Ok(false)
}

/// Closes the innermost element. Returns true when that was the root.
fn close(stack: &mut Vec<Open>, on: &mut impl FnMut(Item) -> Result<(), String>) -> Result<bool, String> {
    let Some(done) = stack.pop() else {
        return Err("an end tag has no start tag".into());
    };
    match stack.len() {
        0 => return Ok(true),
        1 => on(Item::Scalar(done.element.name, done.element.text))?,
        2 => {
            let collection = stack.get(1).map(|c| c.element.name.clone()).unwrap_or_default();
            on(Item::Record { collection, element: done.element })?;
        }
        _ => {
            if let Some(parent) = stack.last_mut() {
                parent.element.children.push(done.element);
            }
        }
    }
    Ok(false)
}

fn append(stack: &mut [Open], s: &str) -> Result<(), String> {
    let Some(top) = stack.last_mut() else { return Ok(()) };
    if top.has_children {
        return Ok(());
    }
    if top.element.text.len().saturating_add(s.len()) > MAX_TEXT_BYTES {
        return Err(format!("the text of a <{}> element is longer than {} MB", top.element.name, MAX_TEXT_BYTES / (1024 * 1024)));
    }
    top.element.text.push_str(s);
    Ok(())
}

/// A character or predefined entity reference (`&#38;`, `&amp;`). Any other entity is refused:
/// it would need a DOCTYPE, whose entities are never expanded.
fn entity(r: &BytesRef) -> Result<char, String> {
    if r.is_char_ref() {
        return match r.resolve_char_ref() {
            Ok(Some(c)) => Ok(c),
            _ => Err("a character reference is not valid".into()),
        };
    }
    let name = r.decode().map_err(|e| format!("an entity name is not valid: {e}"))?;
    match name.as_ref() {
        "lt" => Ok('<'),
        "gt" => Ok('>'),
        "amp" => Ok('&'),
        "apos" => Ok('\''),
        "quot" => Ok('"'),
        other => Err(format!("the entity &{other}; is not supported (entities declared in a DOCTYPE are never expanded)")),
    }
}

/// Builds an XML document with two-space indentation.
pub struct Writer {
    out: String,
    depth: usize,
}

impl Writer {
    pub fn new() -> Self {
        Self { out: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n"), depth: 0 }
    }

    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
    }

    /// Opens `<name>` (with an optional `xmlns`).
    pub fn open(&mut self, name: &str) {
        self.open_ns(name, None);
    }

    pub fn open_ns(&mut self, name: &str, xmlns: Option<&str>) {
        self.indent();
        self.out.push('<');
        self.out.push_str(name);
        if let Some(ns) = xmlns {
            self.out.push_str(" xmlns=\"");
            self.out.push_str(&escape(ns));
            self.out.push('"');
        }
        self.out.push_str(">\n");
        self.depth += 1;
    }

    pub fn close(&mut self, name: &str) {
        self.depth = self.depth.saturating_sub(1);
        self.indent();
        self.out.push_str("</");
        self.out.push_str(name);
        self.out.push_str(">\n");
    }

    /// `<name>value</name>` (the value escaped).
    pub fn leaf(&mut self, name: &str, value: impl std::fmt::Display) {
        let v = value.to_string();
        self.indent();
        self.out.push('<');
        self.out.push_str(name);
        self.out.push('>');
        self.out.push_str(&escape(&v));
        self.out.push_str("</");
        self.out.push_str(name);
        self.out.push_str(">\n");
    }

    pub fn finish(self) -> String {
        self.out
    }
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

/// Escapes text for XML, dropping characters XML 1.0 cannot carry (control characters other
/// than tab, line feed and carriage return; unpaired surrogates cannot occur in a `str`).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(text: &str) -> Result<Vec<Item>, String> {
        let mut out = Vec::new();
        read(text, |i| {
            out.push(i);
            Ok(())
        })?;
        Ok(out)
    }

    #[test]
    fn records_and_scalars_stream_out() {
        let x = "\u{feff}<?xml version=\"1.0\"?><Project xmlns=\"x\"><Name>A &amp; B&#33;</Name><Tasks>\n  <Task><UID>1</UID><TimephasedData><Value>9</Value></TimephasedData><PredecessorLink><PredecessorUID>2</PredecessorUID></PredecessorLink></Task>\n</Tasks><Empty/></Project>";
        let got = items(x).unwrap();
        assert!(matches!(&got[0], Item::Scalar(n, v) if n == "Name" && v == "A & B!"));
        let Item::Record { collection, element } = &got[1] else { panic!("{got:?}") };
        assert_eq!(collection, "Tasks");
        assert_eq!(element.text_of("UID"), Some("1"));
        assert!(element.child("TimephasedData").is_none(), "skipped");
        assert_eq!(element.child("PredecessorLink").and_then(|l| l.text_of("PredecessorUID")), Some("2"));
        assert!(matches!(&got[2], Item::Scalar(n, v) if n == "Tasks" && v.is_empty()), "layout whitespace is dropped");
        assert!(matches!(&got[3], Item::Scalar(n, _) if n == "Empty"));
    }

    #[test]
    fn hostile_documents_are_refused() {
        let laughs = "<?xml version=\"1.0\"?><!DOCTYPE p [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;&a;&a;&a;&a;&a;\">]><Project><Name>&b;</Name></Project>";
        assert!(items(laughs).unwrap_err().contains("never expanded"));
        assert!(items("<Other/>").unwrap_err().contains("not a Microsoft Project XML file"));
        assert!(items("<Project><Name>x</Project>").is_err());
        assert!(items("<Project>").unwrap_err().contains("ended"));
        assert!(items("<Project/><Project/>").is_err());
        let deep = format!("<Project>{}{}</Project>", "<a>".repeat(100), "</a>".repeat(100));
        assert!(items(&deep).unwrap_err().contains("nested"));
        assert!(items("<Project><Name>&#0;</Name></Project>").is_err());
    }

    #[test]
    fn writing_escapes_and_drops_control_characters() {
        assert_eq!(escape("a<b>&\"'\u{1}\tc"), "a&lt;b&gt;&amp;&quot;&apos;\tc");
        let mut w = Writer::new();
        w.open_ns("Project", Some("ns"));
        w.leaf("Name", "R&D");
        w.close("Project");
        let s = w.finish();
        let got = items(&s).unwrap();
        assert!(matches!(&got[0], Item::Scalar(n, v) if n == "Name" && v == "R&D"));
    }
}
