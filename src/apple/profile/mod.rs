mod inspect;
mod recording;
pub(crate) mod runtime_support;
mod symbolicate;

use crate::cli::ProfileKind;

pub use self::inspect::inspect_trace_command;
pub(crate) use self::recording::{
    default_trace_output, ensure_simulator_profiling_supported,
    start_optional_launched_command_trace, trace_launch_environment, wait_for_launched_trace_exit,
};

impl ProfileKind {
    pub(crate) fn trace_label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "memory",
        }
    }

    pub(crate) fn trace_slug(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
        }
    }
}
