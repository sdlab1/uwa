//! Workflow execution. The types (`Workflow`, `Action`, ...) live in
//! [`uwa_core::workflow`] so `uwa-config` can embed them without a
//! circular dependency; this module owns the executor.

mod recorder;
mod runner;

pub use recorder::{propose_workflow, workflow_to_toml_snippet, ProposedWorkflow};
pub use runner::{WorkflowError, WorkflowResult, WorkflowRunner};
pub use uwa_core::workflow::{Action, CaptureSource, Condition, Workflow};
