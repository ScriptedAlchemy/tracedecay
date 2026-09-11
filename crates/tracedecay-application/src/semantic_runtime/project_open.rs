//! Project-open semantic activation classification.

use crate::semantic_runtime::RetrievalProfileActivationObserverErrorV1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialSemanticActivationRestoreV1 {
    Mounted,
    Deferred,
}

pub fn classify_initial_semantic_activation_restore(
    observed: Result<(), RetrievalProfileActivationObserverErrorV1>,
) -> Result<InitialSemanticActivationRestoreV1, RetrievalProfileActivationObserverErrorV1> {
    match observed {
        Ok(()) => Ok(InitialSemanticActivationRestoreV1::Mounted),
        Err(RetrievalProfileActivationObserverErrorV1::Unavailable) => {
            Ok(InitialSemanticActivationRestoreV1::Deferred)
        }
        Err(
            error @ (RetrievalProfileActivationObserverErrorV1::Rejected
            | RetrievalProfileActivationObserverErrorV1::Conflict),
        ) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{InitialSemanticActivationRestoreV1, classify_initial_semantic_activation_restore};
    use crate::semantic_runtime::RetrievalProfileActivationObserverErrorV1;

    #[test]
    fn transient_restore_is_deferred_but_refusals_are_terminal() {
        assert_eq!(
            classify_initial_semantic_activation_restore(Err(
                RetrievalProfileActivationObserverErrorV1::Unavailable,
            )),
            Ok(InitialSemanticActivationRestoreV1::Deferred),
        );
        for refusal in [
            RetrievalProfileActivationObserverErrorV1::Rejected,
            RetrievalProfileActivationObserverErrorV1::Conflict,
        ] {
            assert_eq!(
                classify_initial_semantic_activation_restore(Err(refusal)),
                Err(refusal),
            );
        }
        assert_eq!(
            classify_initial_semantic_activation_restore(Ok(())),
            Ok(InitialSemanticActivationRestoreV1::Mounted),
        );
    }
}
