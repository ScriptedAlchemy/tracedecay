use tracedecay_domain::UtcMicros;

use crate::context::RequestContext;
use crate::handlers::ApplicationOperation;

/// Typed authorization input. It carries no transport-origin authority.
#[derive(Clone, Copy, Debug)]
pub struct AuthorizationRequest<'a> {
    pub context: &'a RequestContext,
    pub operation: &'a ApplicationOperation,
    pub observed_at: UtcMicros,
}
