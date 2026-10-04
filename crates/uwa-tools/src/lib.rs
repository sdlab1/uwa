//! # uwa-tools
//!
//! Tool-call support for a web-UI-bridged LLM.
//!
//! ## Passport (public API)
//! - [`ToolDefinition`] + [`ToolDefinition::from_openai_array`]
//! - [`ToolCall`], [`ToolParseOutcome`], [`parse`], [`parse_with_defs`]
//! - [`build_system_prompt`], [`already_injected`]
//! - [`render_tool_response`], [`compose_browser_turn`]
//! - [`has_tool_marker`], [`tool_marker_index`]

pub mod definition;
pub mod feedback;
pub mod parser;
pub mod prompt;

pub use definition::ToolDefinition;
pub use feedback::{compose_browser_turn, render_tool_response};
pub use parser::{
    has_tool_marker, parse, parse_with_defs, tool_marker_index, ToolCall, ToolParseOutcome,
};
pub use prompt::{already_injected, build_system_prompt};
