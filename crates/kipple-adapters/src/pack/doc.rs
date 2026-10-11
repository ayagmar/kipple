//! Reading a parsed pack document while collecting every error with its position.

use std::ops::Range;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use super::{ErrorCode, PackError};

/// A TOML table of the document.
pub(super) type Table<'i> = DeTable<'i>;

/// A TOML value of the document, with its span.
pub(super) type Value<'i> = Spanned<DeValue<'i>>;

/// Collects errors for one document, with line and column computed from byte spans.
#[derive(Debug)]
pub(super) struct Checker<'a> {
    file: &'a str,
    text: &'a str,
    errors: Vec<PackError>,
}

impl<'a> Checker<'a> {
    pub(super) const fn new(file: &'a str, text: &'a str) -> Self {
        Self {
            file,
            text,
            errors: Vec::new(),
        }
    }

    pub(super) const fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }

    /// How many errors have been reported so far, to tell whether a part added any.
    pub(super) const fn error_count(&self) -> usize {
        self.errors.len()
    }

    /// Every error, sorted by position.
    pub(super) fn into_errors(mut self) -> Vec<PackError> {
        self.errors.sort_by_key(|e| (e.line, e.column, e.code));
        self.errors
    }

    pub(super) fn error(&mut self, code: ErrorCode, at: Range<usize>, message: impl Into<String>) {
        let before = self.text.get(..at.start).unwrap_or(self.text);
        let line = before.matches('\n').count() + 1;
        let column = before
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count()
            + 1;
        self.errors.push(PackError {
            code,
            file: self.file.to_owned(),
            line,
            column,
            message: message.into(),
        });
    }

    /// Reports every key of `table` that isn't in `allowed`.
    pub(super) fn known_keys(&mut self, table: &Table<'_>, allowed: &[&str]) {
        for key in table.keys() {
            if !allowed.contains(&key.get_ref().as_ref()) {
                let message = format!("unknown key `{}`", key.get_ref());
                self.error(ErrorCode::UnknownKey, key.span(), message);
            }
        }
    }

    /// The value of `key` in the table that spans `at`, reporting it missing.
    pub(super) fn required<'t, 'i>(
        &mut self,
        table: &'t Table<'i>,
        at: Range<usize>,
        key: &str,
    ) -> Option<&'t Value<'i>> {
        let value = table.get(key);
        if value.is_none() {
            self.error(ErrorCode::MissingKey, at, format!("missing key `{key}`"));
        }
        value
    }

    fn wrong_type(&mut self, value: &Value<'_>, expected: &str) {
        let message = format!("expected {expected}, found {}", value.get_ref().type_str());
        self.error(ErrorCode::WrongType, value.span(), message);
    }

    /// A string as written, for values with a grammar of their own.
    pub(super) fn raw_string<'t>(&mut self, value: &'t Value<'_>) -> Option<&'t str> {
        let text = value.get_ref().as_str();
        if text.is_none() {
            self.wrong_type(value, "a string");
        }
        text
    }

    /// A string of at most `limit` characters with no control characters.
    pub(super) fn string<'t>(&mut self, value: &'t Value<'_>, limit: usize) -> Option<&'t str> {
        let text = self.raw_string(value)?;
        if text.chars().count() > limit {
            let message = format!("at most {limit} characters");
            self.error(ErrorCode::StringTooLong, value.span(), message);
            return None;
        }
        if text.chars().any(char::is_control) {
            let message = "control characters are not allowed";
            self.error(ErrorCode::StringTooLong, value.span(), message);
            return None;
        }
        Some(text)
    }

    pub(super) fn integer(&mut self, value: &Value<'_>) -> Option<i64> {
        let parsed = value
            .get_ref()
            .as_integer()
            .and_then(|n| i64::from_str_radix(n.as_str(), n.radix()).ok());
        if parsed.is_none() {
            self.wrong_type(value, "an integer");
        }
        parsed
    }

    /// An integer in `range`.
    pub(super) fn integer_in(&mut self, value: &Value<'_>, range: Range<i64>) -> Option<i64> {
        let n = self.integer(value)?;
        if !range.contains(&n) {
            let message = format!("{n} is out of range {}..={}", range.start, range.end - 1);
            self.error(ErrorCode::InvalidRange, value.span(), message);
            return None;
        }
        Some(n)
    }

    pub(super) fn bool(&mut self, value: &Value<'_>) -> Option<bool> {
        let parsed = value.get_ref().as_bool();
        if parsed.is_none() {
            self.wrong_type(value, "true or false");
        }
        parsed
    }

    pub(super) fn table<'t, 'i>(&mut self, value: &'t Value<'i>) -> Option<&'t Table<'i>> {
        let table = value.get_ref().as_table();
        if table.is_none() {
            self.wrong_type(value, "a table");
        }
        table
    }

    pub(super) fn array<'t, 'i>(&mut self, value: &'t Value<'i>) -> Option<&'t [Value<'i>]> {
        let array = value.get_ref().as_array().map(|array| &**array);
        if array.is_none() {
            self.wrong_type(value, "an array");
        }
        array
    }

    /// A string naming one of a fixed set of values, found by `from_name`.
    pub(super) fn one_of<T>(
        &mut self,
        value: &Value<'_>,
        from_name: impl Fn(&str) -> Option<T>,
        expected: &str,
    ) -> Option<T> {
        let text = self.string(value, usize::MAX)?;
        let found = from_name(text);
        if found.is_none() {
            self.error(
                ErrorCode::WrongType,
                value.span(),
                format!("expected {expected}, found {text:?}"),
            );
        }
        found
    }
}
