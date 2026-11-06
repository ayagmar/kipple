//! The built-in integrations. Each one only reads, inside the roots it declared, and
//! proposes findings and advice; the core decides what they mean.

mod cargo;
mod claude;
mod codex;
mod cursor;
mod git;
mod node;
mod npm;
mod pi;
mod project;
mod python;
mod system;

pub(crate) use cargo::Cargo;
pub(crate) use claude::ClaudeCode;
pub(crate) use codex::Codex;
pub(crate) use cursor::Cursor;
pub(crate) use git::Git;
pub use git::{Anomaly, Unregistered, registration_advice};
pub(crate) use node::Node;
pub(crate) use npm::Npm;
pub(crate) use pi::Pi;
pub(crate) use python::Python;
pub(crate) use system::{Journald, Pacman, Trash};
