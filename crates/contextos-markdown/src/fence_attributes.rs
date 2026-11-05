//! The `{key=value key="quoted" flag}` attribute list on a fence's opening line.

use crate::fence_vocabulary::{AttributeType, FenceVocabulary, is_fence_name};

/// A checked attribute value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttributeValue {
    Text(String),
    Token(String),
    Integer(u32),
    Boolean(bool),
}

/// The attribute list to read, and the vocabulary that says which attributes exist.
pub struct AttributeInput<'a> {
    /// The text between the braces.
    pub raw: &'a str,
    pub vocabulary: &'a FenceVocabulary,
}

/// The attributes that were read, in the order written, and a description of every one that was not.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FenceAttributes {
    pub values: Vec<(String, AttributeValue)>,
    pub problems: Vec<String>,
}

impl From<AttributeInput<'_>> for FenceAttributes {
    fn from(input: AttributeInput<'_>) -> Self {
        let written = match written_attributes(input.raw) {
            Ok(written) => written,
            Err(reason) => {
                return Self {
                    values: Vec::new(),
                    problems: vec![format!(
                        "the attributes {{{}}} could not be read: {reason}",
                        input.raw.trim()
                    )],
                };
            }
        };
        let mut checked = Self::default();
        for (key, value) in written {
            if checked.values.iter().any(|(seen, _)| *seen == key) {
                checked
                    .problems
                    .push(format!("{key} is given more than once; the first is used"));
                continue;
            }
            let Some(spec) = input.vocabulary.attribute(&key) else {
                checked
                    .problems
                    .push(format!("{key} is not an attribute this vault's fences take"));
                continue;
            };
            match checked_value(spec.kind, spec.values.as_deref(), value.as_deref()) {
                Ok(value) => checked.values.push((key, value)),
                Err(reason) => checked.problems.push(format!("{key} {reason}")),
            }
        }
        checked
    }
}

/// The key and raw value of each attribute, in order, or why the list does not parse.
fn written_attributes(raw: &str) -> Result<Vec<(String, Option<String>)>, &'static str> {
    let mut written = Vec::new();
    let mut chars = raw.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            return Ok(written);
        }
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| *c != '=' && !c.is_whitespace()) {
            key.push(c);
        }
        if !is_fence_name(&key) {
            return Err("each key is lower-case letters, digits, and -");
        }
        if chars.next_if_eq(&'=').is_none() {
            written.push((key, None));
            continue;
        }
        let mut value = String::new();
        if chars.next_if_eq(&'"').is_some() {
            loop {
                match chars.next() {
                    None => return Err("a quoted value is not closed"),
                    Some('"') => break,
                    Some('\\') => match chars.next() {
                        Some(escaped @ ('"' | '\\')) => value.push(escaped),
                        _ => return Err("a backslash in a quoted value escapes only \" or \\"),
                    },
                    Some(c) => value.push(c),
                }
            }
            if chars.peek().is_some_and(|c| !c.is_whitespace()) {
                return Err("a quoted value is followed by more text without a space");
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                value.push(c);
            }
        }
        written.push((key, Some(value)));
    }
}

fn is_word(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// `raw` checked against its attribute's type, or why it does not fit.
fn checked_value(kind: AttributeType, allowed: Option<&[String]>, raw: Option<&str>) -> Result<AttributeValue, String> {
    match (kind, raw) {
        (AttributeType::Boolean, None | Some("true")) => Ok(AttributeValue::Boolean(true)),
        (AttributeType::Boolean, Some("false")) => Ok(AttributeValue::Boolean(false)),
        (AttributeType::Boolean, Some(_)) => Err("is true or false".to_owned()),
        (_, None) => Err("needs a value".to_owned()),
        (_, Some("")) => Err("has no value".to_owned()),
        (AttributeType::Text, Some(text)) => Ok(AttributeValue::Text(text.to_owned())),
        (AttributeType::Id, Some(id)) if is_word(id) => Ok(AttributeValue::Token(id.to_owned())),
        (AttributeType::Id, Some(_)) => Err("is letters, digits, - and _".to_owned()),
        (AttributeType::Token, Some(word)) if is_word(word) => match allowed {
            Some(allowed) if !allowed.iter().any(|value| value == word) => {
                Err(format!("is one of {}", allowed.join(", ")))
            }
            _ => Ok(AttributeValue::Token(word.to_owned())),
        },
        (AttributeType::Token, Some(_)) => Err("is one word".to_owned()),
        (AttributeType::Integer, Some(digits))
            if (1..=6).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit()) =>
        {
            digits
                .parse()
                .map(AttributeValue::Integer)
                .map_err(|_| "is a whole number".to_owned())
        }
        (AttributeType::Integer, Some(_)) => Err("is a whole number of up to six digits".to_owned()),
    }
}

impl FenceAttributes {
    /// The value of `key`, when it was given and valid.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&AttributeValue> {
        self.values.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }
}

#[cfg(test)]
#[path = "fence_attributes_test.rs"]
mod tests;
