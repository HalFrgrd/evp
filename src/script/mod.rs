//! VHS `.tape` script parsing.

pub mod ast;
pub mod parser;

pub use ast::{Event, KeySpec, ModSet, NamedKey, Script, Settings, WaitScope};
pub use parser::parse;
