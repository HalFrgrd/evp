//! VHS `.tape` script parsing.

pub mod ast;
pub mod parser;

pub use ast::{Event, KeyAction, KeySpec, ModSet, NamedKey, Script, Settings, WaitScope};
pub use parser::{parse, parse_path};
