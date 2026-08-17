mod path;
mod payload;

pub use path::{PathPolicyError, validate_relative_path};
pub use payload::*;

pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 0;
