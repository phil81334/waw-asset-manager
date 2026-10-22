//! Readers for the installation's text formats. No research data is bundled.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Properties = BTreeMap<String, String>;

#[derive(Clone, Debug)]
pub struct Token {
    pub value: String,
    pub quoted: bool,
    pub start: usize,
    pub end: usize,
}

pub fn quoted(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

pub fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        if text[cursor..].starts_with("//") {
            cursor += text[cursor..].find('\n').unwrap_or(bytes.len() - cursor);
            continue;
        }
        if text[cursor..].starts_with("/*") {
            let end = text[cursor + 2..]
                .find("*/")
                .ok_or("Unclosed block comment")?;
            cursor += end + 4;
            continue;
        }
        let start = cursor;
        let is_quoted = bytes[cursor] == b'"';
        let value;
        if is_quoted {
            cursor += 1;
            let mut content = String::new();
            let mut closed = false;
            while cursor < bytes.len() {
                let character = text[cursor..].chars().next().unwrap();
                cursor += character.len_utf8();
                if character == '"' {
                    closed = true;
                    break;
                }
                if character == '\\' {
                    let escaped = text[cursor..]
                        .chars()
                        .next()
                        .ok_or("Unfinished string escape")?;
                    cursor += escaped.len_utf8();
                    content.push(match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                } else {
                    content.push(character);
                }
            }
            if !closed {
                return Err("Unclosed quoted string".into());
            }
            value = content;
        } else if b"{}()[],|=?!&<>+:".contains(&bytes[cursor]) {
            cursor += 1;
            if cursor < bytes.len()
                && matches!(
                    &text[start..cursor + 1],
                    "==" | "!=" | "<=" | ">=" | "&&" | "||"
                )
            {
                cursor += 1;
            }
            value = text[start..cursor].into();
        } else {
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && !b"{}()[],|=?!&<>+:\"".contains(&bytes[cursor])
            {
                cursor += 1;
            }
            value = text[start..cursor].into();
        }
        result.push(Token {
            value,
            quoted: is_quoted,
            start,
            end: cursor,
        });
    }
    Ok(result)
}

struct Reader {
    tokens: Vec<Token>,
    at: usize,
}

impl Reader {
    fn peek(&self) -> &str {
        self.tokens
            .get(self.at)
            .map(|token| token.value.as_str())
            .unwrap_or("")
    }

    fn take(&mut self) -> Result<Token, String> {
        let token = self
            .tokens
            .get(self.at)
            .cloned()
            .ok_or("Unexpected end of file")?;
        self.at += 1;
        Ok(token)
    }

    fn expect(&mut self, value: &str) -> Result<(), String> {
        let token = self.take()?;
        if token.value != value || token.quoted {
            return Err(format!("Expected {value}, found {}", token.value));
        }
        Ok(())
    }

    fn arguments(&mut self) -> Result<Vec<Vec<Token>>, String> {
        self.expect("(")?;
        let mut depth = 0;
        let mut arguments = vec![Vec::new()];
        loop {
            let token = self.take()?;
            if !token.quoted {
                if token.value == ")" && depth == 0 {
                    break;
                }
                if token.value == "," && depth == 0 {
                    arguments.push(Vec::new());
                    continue;
                }
                if token.value == "(" {
                    depth += 1;
                }
                if token.value == ")" {
                    depth -= 1;
                }
            }
            arguments.last_mut().unwrap().push(token);
        }
        Ok(arguments)
    }
}

fn expression(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(|token| {
            if token.quoted {
                quoted(&token.value)
            } else {
                token.value.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Field {
    pub name: String,
    pub kind: String,
    pub default: String,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub choices: Vec<String>,
    pub references: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Control {
    pub kind: String,
    pub binding: String,
    pub label: String,
    pub tooltip: String,
    pub visible: String,
    pub directory: String,
    pub children: Vec<Control>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Schema {
    pub file: String,
    pub name: String,
    pub fields: Vec<Field>,
    pub controls: Vec<Control>,
    pub targets: Vec<String>,
    pub plugins: Vec<String>,
}

fn control(reader: &mut Reader) -> Result<Control, String> {
    let kind = reader.take()?.value;
    let mut node = Control {
        kind: kind.clone(),
        binding: String::new(),
        label: String::new(),
        tooltip: String::new(),
        visible: String::new(),
        directory: String::new(),
        children: Vec::new(),
    };
    if kind.ends_with("container") {
        reader.expect("{")?;
        while reader.peek() != "}" {
            node.children.push(control(reader)?);
        }
        reader.expect("}")?;
    } else {
        let arguments = reader.arguments()?;
        node.binding = expression(&arguments[0]);
    }
    if reader.peek() == "[" {
        reader.expect("[")?;
        reader.expect("exec")?;
        reader.expect("{")?;
        while reader.peek() != "}" {
            let property = reader.take()?.value;
            let arguments = reader.arguments()?;
            let text = arguments
                .first()
                .and_then(|arg| arg.first())
                .map(|token| token.value.clone())
                .unwrap_or_default();
            match property.as_str() {
                "label" | "groupBox" => node.label = text,
                "tooltip" => node.tooltip = text,
                "reldir" => node.directory = text,
                "visible" => node.visible = expression(&arguments[0]),
                _ => {} // Width, alignment and annotation typos do not change stored values.
            }
        }
        reader.expect("}")?;
        reader.expect("]")?;
    }
    Ok(node)
}

pub fn parse_schema(file: &str, text: &str) -> Result<Schema, String> {
    let mut reader = Reader {
        tokens: tokenize(text)?,
        at: 0,
    };
    let mut schema = Schema {
        file: file.into(),
        name: file.trim_end_matches(".gdf").into(),
        fields: Vec::new(),
        controls: Vec::new(),
        targets: Vec::new(),
        plugins: Vec::new(),
    };
    while !reader.peek().is_empty() {
        if matches!(reader.peek(), "vcontainer" | "hcontainer" | "fxvcontainer") {
            schema.controls.push(control(&mut reader)?);
            continue;
        }
        let kind = reader.take()?.value;
        if kind.starts_with('#') {
            let first = reader.take()?.value;
            match kind.as_str() {
                "#name" => schema.name = first,
                "#target_dir" => schema.targets.push(first),
                "#dll" => {
                    let plugin = reader.take()?.value;
                    if first == "pc" || first == "all" {
                        schema.plugins.push(plugin);
                    }
                }
                _ => {}
            }
            continue;
        }
        let mut field = Field {
            name: String::new(),
            kind: kind.clone(),
            default: String::new(),
            minimum: None,
            maximum: None,
            choices: Vec::new(),
            references: Vec::new(),
        };
        match kind.as_str() {
            "string" => {
                if reader
                    .tokens
                    .get(reader.at)
                    .is_some_and(|token| token.quoted)
                {
                    field.default = reader.take()?.value;
                }
            }
            "int" | "float" => {
                field.default = reader.take()?.value;
                field.minimum = reader.take()?.value.parse().ok();
                field.maximum = reader.take()?.value.parse().ok();
            }
            "color" => {
                let mut values = Vec::new();
                for _ in 0..4 {
                    values.push(reader.take()?.value);
                }
                field.default = values.join(" ");
            }
            "enum" => {
                reader.expect("{")?;
                while reader.peek() != "}" {
                    let token = reader.take()?;
                    if token.quoted {
                        field.choices.push(token.value);
                    }
                }
                reader.expect("}")?;
                field.default = field.choices.first().cloned().unwrap_or_default();
            }
            "keyenum" => {
                let arguments = reader.arguments()?;
                field.references = arguments
                    .into_iter()
                    .flatten()
                    .filter(|token| token.quoted)
                    .map(|token| token.value)
                    .collect();
            }
            _ => return Err(format!("Unsupported declaration {kind} in {file}")),
        }
        field.name = reader.take()?.value;
        schema.fields.push(field);
    }
    Ok(schema)
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Entry {
    pub id: usize,
    pub name: String,
    pub schema: String,
    pub properties: Properties,
}

struct Span {
    entry: Entry,
    start: usize,
    end: usize,
    name: (usize, usize),
    properties: BTreeMap<String, (usize, usize)>,
    closing: usize,
}

fn spans(text: &str) -> Result<Vec<Span>, String> {
    let mut reader = Reader {
        tokens: tokenize(text)?,
        at: 0,
    };
    reader.expect("{")?;
    let mut entries = Vec::new();
    while reader.peek() != "}" {
        let name = reader.take()?;
        reader.expect("(")?;
        let schema = reader.take()?;
        reader.expect(")")?;
        reader.expect("{")?;
        let mut properties = Properties::new();
        let mut offsets = BTreeMap::new();
        while reader.peek() != "}" {
            let key = reader.take()?;
            let value = reader.take()?;
            if !key.quoted || !value.quoted || properties.contains_key(&key.value) {
                return Err(format!("Invalid or repeated property in {}", name.value));
            }
            offsets.insert(key.value.clone(), (key.start, value.end));
            properties.insert(key.value, value.value);
        }
        let closing = reader.take()?;
        entries.push(Span {
            entry: Entry {
                id: entries.len(),
                name: name.value,
                schema: schema.value,
                properties,
            },
            start: name.start,
            end: closing.end,
            name: (name.start, name.end),
            properties: offsets,
            closing: closing.start,
        });
    }
    reader.expect("}")?;
    if !reader.peek().is_empty() {
        return Err("Unexpected data after GDT closing brace".into());
    }
    Ok(entries)
}

pub fn parse_gdt(text: &str) -> Result<Vec<Entry>, String> {
    Ok(spans(text)?.into_iter().map(|span| span.entry).collect())
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.contains("..")
        && !name.starts_with('/')
        && !name.starts_with('-')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && !part.ends_with('.'))
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-/.$~".contains(&byte))
}

pub fn render_entry(entry: &Entry) -> String {
    let mut text = format!(
        "{} ( {} )\n{{\n",
        quoted(&entry.name),
        quoted(&entry.schema)
    );
    for (key, value) in &entry.properties {
        text.push_str(&format!("\t{} {}\n", quoted(key), quoted(value)));
    }
    text.push_str("}\n");
    text
}

/// Apply edits by source spans. Untouched entries, comments and unknown fields
/// stay byte-identical; hidden/default fields are never regenerated wholesale.
pub fn rewrite_gdt(original: &str, entries: &[Entry]) -> Result<String, String> {
    let original_spans = spans(original)?;
    let mut replacements = Vec::new();
    for span in &original_spans {
        if let Some(edited) = entries.iter().find(|entry| entry.id == span.entry.id) {
            if edited.schema != span.entry.schema {
                return Err(
                    "Changing an existing asset's type is not supported; create a new asset."
                        .into(),
                );
            }
            if edited.name != span.entry.name {
                replacements.push((span.name.0, span.name.1, quoted(&edited.name)));
            }
            for (key, (start, end)) in &span.properties {
                if edited.properties.get(key) != span.entry.properties.get(key) {
                    let replacement = edited
                        .properties
                        .get(key)
                        .map(|value| format!("{} {}", quoted(key), quoted(value)))
                        .unwrap_or_default();
                    replacements.push((*start, *end, replacement));
                }
            }
            let mut added = String::new();
            for (key, value) in &edited.properties {
                if !span.properties.contains_key(key) {
                    added.push_str(&format!("\t{} {}\n", quoted(key), quoted(value)));
                }
            }
            if !added.is_empty() {
                replacements.push((span.closing, span.closing, added));
            }
        } else {
            replacements.push((span.start, span.end, String::new()));
        }
    }
    let closing = tokenize(original)?.last().ok_or("Empty GDT")?.start;
    let mut added = String::new();
    for entry in entries {
        if !valid_name(&entry.name)
            || !entry.schema.ends_with(".gdf")
            || entry.schema.contains(['/', '\\', ':'])
        {
            return Err(format!("Invalid asset name/type: {}", entry.name));
        }
        if entry.id >= original_spans.len() {
            added.push_str(&render_entry(entry));
        }
    }
    replacements.push((closing, closing, added));
    replacements.sort_by_key(|item| (item.0, item.1));
    let mut result = String::new();
    let mut cursor = 0;
    for (start, end, replacement) in replacements {
        result.push_str(&original[cursor..start]);
        result.push_str(&replacement);
        cursor = end;
    }
    result.push_str(&original[cursor..]);
    // Reparse before writing so malformed generated text cannot reach the disk.
    parse_gdt(&result)?;
    Ok(result)
}
