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
//! - [`estimate`], [`estimate_for_content`] (token estimation)
//! - [`validate_arguments`] (schema validation)
//! - [`validate_calls`], [`correction_prompt`] (emulation)

pub mod definition;
pub mod emulation;
pub mod feedback;
pub mod parser;
pub mod prompt;
pub mod schema;
pub mod tokenizer;

pub use definition::ToolDefinition;
pub use emulation::{correction_prompt, validate_calls, ValidationOutcome};
pub use feedback::{compose_browser_turn, render_tool_response};
pub use parser::{
    has_tool_marker, parse, parse_with_defs, safe_emit_end, safe_emit_end_for, tool_marker_index,
    unstreamed_text, ToolCall, ToolParseOutcome,
};
pub use prompt::{already_injected, build_system_prompt, tool_correction};
pub use schema::validate_arguments;
pub use tokenizer::{estimate, estimate_for_content};
