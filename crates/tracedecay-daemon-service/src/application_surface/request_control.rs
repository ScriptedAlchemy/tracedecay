//! HTTP request-identity admission and active-cancellation ownership.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tracedecay_api::HttpApplicationControls;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{
    APPLICATION_REQUEST_ID_HEADER, ApplicationRequestControlV1, CancellationSignal, Deadline,
    RequestId,
};
use tracedecay_domain::UtcMicros;

use super::problems::current_micros;
use super::{DEFAULT_DEADLINE_MICROS, HTTP_DEADLINE_HEADER, retained};

pub(super) type HttpCancellationRegistry = Arc<Mutex<BTreeMap<RequestId, CancellationSignal>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RequestControlError {
    DuplicateHeader,
    InvalidHeader,
    ActiveCollision,
    RegistryUnavailable,
}

pub(super) fn supplied_request_id(
    headers: &HeaderMap,
) -> Result<Option<RequestId>, RequestControlError> {
    let mut values = headers.get_all(APPLICATION_REQUEST_ID_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(RequestControlError::DuplicateHeader);
    }
    let value = value
        .to_str()
        .map_err(|_| RequestControlError::InvalidHeader)?;
    let request_id =
        RequestId::new(value.to_owned()).map_err(|_| RequestControlError::InvalidHeader)?;
    Ok(Some(
        ApplicationRequestControlV1::new(request_id).request_id,
    ))
}

pub(super) fn accepts_supplied_request_id(path: &str) -> bool {
    path == tracedecay_api::retained_route_path(
        tracedecay_contracts::retained_surfaces::RetainedSurfaceOperation::FactStoreCurate,
    )
}

pub(super) struct ActiveHttpRequest {
    registry: HttpCancellationRegistry,
    request_id: RequestId,
    armed: bool,
}

impl ActiveHttpRequest {
    pub(super) fn register(
        registry: HttpCancellationRegistry,
        request_id: RequestId,
        cancellation: CancellationSignal,
    ) -> Result<Self, RequestControlError> {
        let mut active = registry
            .lock()
            .map_err(|_| RequestControlError::RegistryUnavailable)?;
        if active.contains_key(&request_id) {
            return Err(RequestControlError::ActiveCollision);
        }
        active.insert(request_id.clone(), cancellation);
        drop(active);
        Ok(Self {
            registry,
            request_id,
            armed: true,
        })
    }

    pub(super) fn finish(mut self) {
        self.remove();
        self.armed = false;
    }

    fn remove(&self) {
        if let Ok(mut active) = self.registry.lock() {
            active.remove(&self.request_id);
        }
    }
}

impl Drop for ActiveHttpRequest {
    fn drop(&mut self) {
        if self.armed {
            self.remove();
        }
    }
}

#[hotpath::measure(label = "application_surface.http_context")]
pub(super) async fn application_http_context(
    State(cancellations): State<HttpCancellationRegistry>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let supplied_request_id = match supplied_request_id(request.headers()) {
        Ok(request_id) => request_id,
        Err(RequestControlError::DuplicateHeader | RequestControlError::InvalidHeader) => {
            return invalid_http_request_control_response();
        }
        Err(RequestControlError::ActiveCollision | RequestControlError::RegistryUnavailable) => {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if supplied_request_id.is_some() && !accepts_supplied_request_id(request.uri().path()) {
        return invalid_http_request_control_response();
    }
    let request_id = match supplied_request_id {
        Some(request_id) => request_id,
        None => {
            let Ok(request_id) = mint_global_request_id(GlobalRequestSurface::Http) else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            request_id
        }
    };
    let Ok(cancellation) =
        CancellationSignal::active(format!("cancellation.http.{}", request_id.as_str()))
    else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Ok(observed_at) = current_micros() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let default_expires_at = observed_at.0.saturating_add(DEFAULT_DEADLINE_MICROS);
    let caller_expires_at = match request.headers().get(HTTP_DEADLINE_HEADER) {
        Some(value) => match value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
        {
            Some(expires_at) => expires_at,
            None => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => default_expires_at,
    };
    let effective_expires_at = caller_expires_at.min(default_expires_at);
    let Ok(deadline) = Deadline::new(UtcMicros(effective_expires_at)) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let active = match ActiveHttpRequest::register(
        Arc::clone(&cancellations),
        request_id.clone(),
        cancellation.clone(),
    ) {
        Ok(active) => active,
        Err(RequestControlError::ActiveCollision) => {
            return retained::active_request_conflict_response(request_id);
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    request.extensions_mut().insert(request_id.clone());
    request.extensions_mut().insert(cancellation.clone());
    request.extensions_mut().insert(HttpApplicationControls {
        deadline,
        cancellation: cancellation.clone(),
    });
    let response = hotpath::future!(
        next.run(request),
        label = "application_surface.http.dispatch"
    )
    .await;
    active.finish();
    response
}

pub(super) fn invalid_http_request_control_response() -> Response {
    match mint_global_request_id(GlobalRequestSurface::Http) {
        Ok(request_id) => tracedecay_api::retained_invalid_request_response(request_id),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use tracedecay_contracts::{APPLICATION_REQUEST_ID_HEADER, CancellationSignal, RequestId};

    use super::*;

    #[test]
    fn caller_request_identity_is_exact_and_duplicate_values_fail_closed() {
        let mut headers = HeaderMap::new();
        headers.insert(
            APPLICATION_REQUEST_ID_HEADER,
            HeaderValue::from_static("request.sdk.curate"),
        );
        assert_eq!(
            supplied_request_id(&headers).expect("valid request control"),
            Some(RequestId::new("request.sdk.curate").expect("request id"))
        );
        headers.append(
            APPLICATION_REQUEST_ID_HEADER,
            HeaderValue::from_static("request.sdk.other"),
        );
        assert_eq!(
            supplied_request_id(&headers),
            Err(RequestControlError::DuplicateHeader)
        );
        assert!(accepts_supplied_request_id("/retained/fact_store_curate"));
        assert!(!accepts_supplied_request_id("/retained/fact_store_add"));
    }

    #[test]
    fn malformed_caller_request_identity_fails_closed() {
        let mut headers = HeaderMap::new();
        headers.insert(APPLICATION_REQUEST_ID_HEADER, HeaderValue::from_static(""));
        assert_eq!(
            supplied_request_id(&headers),
            Err(RequestControlError::InvalidHeader)
        );
    }

    #[test]
    fn active_identity_cannot_replace_its_cancellation_owner_but_reopens_after_finish() {
        let registry = HttpCancellationRegistry::default();
        let request_id = RequestId::new("request.sdk.replay").expect("request id");
        let cancellation =
            CancellationSignal::active("cancellation.sdk.first").expect("cancellation signal");
        let active =
            ActiveHttpRequest::register(Arc::clone(&registry), request_id.clone(), cancellation)
                .expect("first admission");
        let collision = ActiveHttpRequest::register(
            Arc::clone(&registry),
            request_id.clone(),
            CancellationSignal::active("cancellation.sdk.second").expect("cancellation signal"),
        );
        assert!(matches!(
            collision,
            Err(RequestControlError::ActiveCollision)
        ));
        active.finish();
        ActiveHttpRequest::register(
            registry,
            request_id,
            CancellationSignal::active("cancellation.sdk.replay").expect("cancellation signal"),
        )
        .expect("terminal replay admission");
    }
}
