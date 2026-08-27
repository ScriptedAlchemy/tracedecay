//! Turtle (Terse RDF Triple Language) serialization and parsing.
//!
//! Implements the [W3C Turtle](https://www.w3.org/TR/turtle/) format for RDF data.
//! This module provides zero-dependency Turtle support using Grafeo's native RDF types.

mod parser;
mod serializer;

pub use parser::{TurtleError, TurtleParser};
pub use serializer::TurtleSerializer;
