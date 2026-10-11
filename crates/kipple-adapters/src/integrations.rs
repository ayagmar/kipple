//! The built-in integrations. Each one only reads, inside the roots it declared, and
//! proposes findings and advice; the core decides what they mean.

mod cargo;
mod claude;
mod node;
mod project;
mod python;
mod system;

pub(crate) use cargo::Cargo;
pub(crate) use claude::ClaudeCode;
pub(crate) use node::Node;
pub(crate) use python::Python;
pub(crate) use system::{Journald, Pacman, Trash};
