use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_query::retrieval::ports::RetrievalExecutionControl;

pub(super) struct ActiveControl;

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

impl RetrievalExecutionControl for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn elapsed_micros(&self) -> u64 {
        0
    }
}

pub(super) struct CancelledControl;

impl CodeIndexExecutionControlV1 for CancelledControl {
    fn is_cancelled(&self) -> bool {
        true
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}
