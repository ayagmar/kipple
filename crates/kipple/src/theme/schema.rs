//! The JSON schema of a theme file, built from the same tables the reader checks, so
//! an editor validates a shared theme exactly as kipple will.

use serde_json::{Map, Value, json};

use super::Role;
use super::parse::{ANSI, MODIFIERS};

/// The JSON schema of a theme file, version 1.
#[must_use]
pub(crate) fn schema() -> schemars::Schema {
    let names: Vec<&str> = ANSI
        .iter()
        .map(|(name, _)| *name)
        .chain(["reset"])
        .collect();
    let colour = json!({
        "description": "An ANSI colour name, #rrggbb, an index from 0 to 255, or reset for the terminal's own.",
        "oneOf": [
            { "type": "string", "enum": names },
            { "type": "string", "pattern": "^#[0-9A-Fa-f]{6}$" },
            { "type": "integer", "minimum": 0, "maximum": 255 }
        ]
    });
    let mut style = Map::new();
    style.insert("fg".to_owned(), json!({ "$ref": "#/$defs/colour" }));
    style.insert("bg".to_owned(), json!({ "$ref": "#/$defs/colour" }));
    for (name, _) in MODIFIERS {
        style.insert(name.to_owned(), json!({ "type": "boolean" }));
    }
    let roles: Map<String, Value> = Role::ALL
        .iter()
        .map(|role| (role.name().to_owned(), json!({ "$ref": "#/$defs/style" })))
        .collect();
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "kipple theme",
        "description": "A kipple theme file: a style for each role. A role left out comes from the theme it extends, else the terminal's own style.",
        "type": "object",
        "properties": {
            "version": { "const": 1 },
            "extends": {
                "description": "The theme this one starts from: a built-in or another theme of yours.",
                "type": "string",
                "pattern": "^[a-z0-9-]+$"
            },
            "roles": {
                "type": "object",
                "properties": roles,
                "additionalProperties": false
            }
        },
        "required": ["version"],
        "additionalProperties": false,
        "$defs": {
            "colour": colour,
            "style": {
                "type": "object",
                "properties": style,
                "additionalProperties": false
            }
        }
    });
    schemars::Schema::try_from(schema).unwrap_or_default()
}
