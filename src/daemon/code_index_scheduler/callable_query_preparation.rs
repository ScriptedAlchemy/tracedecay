//! Request-local callable-code generation binding.
//!
//! Ordinary reads resolve through the registry freshness ladder. Multi-root
//! reads bind the immutable generation owner sampled for that root, including
//! cursor routing, so a concurrent publication cannot change the query input.

use std::sync::Arc;

use tracedecay_application::{PageRequest, RequestAdmission, RetrievalPortContext};
use tracedecay_domain::{CodeGenerationId, ManifestDigest, TemporalModeV1};
use tracedecay_query::retrieval::{
    PreparedQueryV1, QueryAuthorityV1, route_authenticated_prepared_query_cursor,
};

use super::queries::{
    CallableCodeCursorError, PreparedCallableQueryV1, base_request, current_utc_micros,
    is_unpinned_latest, prepared_routing_bindings, remaining_generation_resolution_wait,
};
use super::{CodeIndexSchedulerRegistryV1, LatestCompleteCodeIndexV1};

impl CodeIndexSchedulerRegistryV1 {
    pub(in crate::daemon) fn bind_callable_query_generation(
        &self,
        scope: tracedecay_application::ResolvedScope,
        generation: LatestCompleteCodeIndexV1,
    ) -> Option<Self> {
        if scope.validate().is_err()
            || !Self::latest_matches_scope(&generation, &scope)
            || generation.production_query_owners().is_err()
        {
            return None;
        }
        let mut bound = self.clone();
        bound.query_generation_pin = Some((scope, generation));
        Some(bound)
    }

    pub(super) async fn prepare_callable_query(
        &self,
        context: &RetrievalPortContext<'_>,
        requested_generation: &CodeGenerationId,
        page: &PageRequest,
        temporal: TemporalModeV1,
        operation: &'static str,
        query_binding_digest: ManifestDigest,
    ) -> Result<PreparedCallableQueryV1, CallableCodeCursorError> {
        let authority = self
            .query_authority_for_scope(context.request.scope())
            .await
            .ok_or(CallableCodeCursorError::Unavailable)?;
        let routing = prepared_routing_bindings(
            context,
            temporal,
            operation,
            query_binding_digest,
            page.page_size,
        )?;
        let latest = match &self.query_generation_pin {
            Some((scope, generation)) => {
                self.resolve_bound_query_generation(
                    context,
                    requested_generation,
                    page,
                    authority.as_ref(),
                    &routing,
                    scope,
                    generation,
                )
                .await?
            }
            None => {
                self.resolve_serving_generation(
                    context.request,
                    requested_generation,
                    page,
                    authority.as_ref(),
                    &routing,
                )
                .await?
            }
        };
        prepare_query(context, page, temporal, authority, latest)
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_bound_query_generation(
        &self,
        context: &RetrievalPortContext<'_>,
        requested_generation: &CodeGenerationId,
        page: &PageRequest,
        authority: &QueryAuthorityV1,
        routing: &tracedecay_query::retrieval::PreparedQueryRoutingBindingsV1,
        scope: &tracedecay_application::ResolvedScope,
        pinned: &LatestCompleteCodeIndexV1,
    ) -> Result<LatestCompleteCodeIndexV1, CallableCodeCursorError> {
        if context.request.scope() != scope || !Self::latest_matches_scope(pinned, scope) {
            return Err(CallableCodeCursorError::Unavailable);
        }
        let generation = &pinned.generation().manifest().generation_id;
        if !is_unpinned_latest(requested_generation) && requested_generation != generation {
            return Err(CallableCodeCursorError::Unavailable);
        }
        let wait = remaining_generation_resolution_wait(context.request)
            .ok_or(CallableCodeCursorError::Unavailable)?;
        let resolution = async {
            if let Some(cursor) = page.cursor.as_ref() {
                let expected = generation.clone();
                let closure_expected = expected.clone();
                let pinned = pinned.clone();
                route_authenticated_prepared_query_cursor(
                    authority,
                    routing,
                    cursor.as_str(),
                    current_utc_micros()?,
                    Some(&expected),
                    move |cursor_generation| {
                        let expected = closure_expected.clone();
                        let pinned = pinned.clone();
                        async move { (cursor_generation == expected).then_some(pinned) }
                    },
                )?
                .await
                .ok_or(CallableCodeCursorError::Unavailable)
            } else {
                Ok(pinned.clone())
            }
        };
        let latest = tokio::time::timeout(wait, resolution)
            .await
            .map_err(|_| CallableCodeCursorError::Unavailable)??;
        if context.request.admission_at(current_utc_micros()?) != RequestAdmission::Admitted {
            return Err(CallableCodeCursorError::Unavailable);
        }
        Ok(latest)
    }
}

fn prepare_query(
    context: &RetrievalPortContext<'_>,
    page: &PageRequest,
    temporal: TemporalModeV1,
    authority: Arc<QueryAuthorityV1>,
    latest: LatestCompleteCodeIndexV1,
) -> Result<PreparedCallableQueryV1, CallableCodeCursorError> {
    let base = base_request(context, &latest, temporal, authority.profile())
        .map_err(|_| CallableCodeCursorError::Unavailable)?;
    let query = PreparedQueryV1::prepare(
        authority,
        base,
        page.cursor
            .as_ref()
            .map(tracedecay_application::OpaqueCursor::as_str),
    )?;
    Ok(PreparedCallableQueryV1 { latest, query })
}
