//! Runtime control credential is separate from human and agent credentials.
use super::*;
use alt_json_serde::Serde;
use golia_platform_contract::lifecycle::Probe;
use std::io::Read;
fn unavailable() -> Response<ResponseBody> {
    failure(
        503,
        ErrorCode::TemporarilyUnavailable,
        "Runtime storage is not ready",
        None,
        Retry::SameRequest,
    )
}
pub(super) async fn handle(
    request: Request<Incoming>,
    state: Arc<State>,
) -> Response<ResponseBody> {
    let Some(cloud) = state.factory.cloud.clone() else {
        return failure(
            404,
            ErrorCode::InvalidRequest,
            "Unknown endpoint",
            None,
            Retry::Never,
        );
    };
    let headers = request.headers();
    if request.method() != hyper::Method::POST
        || request.uri().query().is_some()
        || [header::COOKIE, header::ORIGIN, header::CONTENT_ENCODING]
            .iter()
            .any(|h| headers.contains_key(h))
        || headers.get_all(header::AUTHORIZATION).iter().count() != 1
        || !headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .is_some_and(|token| cloud.authenticate_probe(token))
    {
        return failure(
            401,
            ErrorCode::Unauthenticated,
            "Runtime authentication required",
            None,
            Retry::Never,
        );
    }
    if headers.get_all(header::CONTENT_TYPE).iter().count() != 1
        || headers
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            != Some("application/json")
    {
        return failure(
            415,
            ErrorCode::InvalidRequest,
            "JSON required",
            None,
            Retry::Never,
        );
    }
    let permit = match state.workers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return unavailable(),
    };
    let source = match tokio::time::timeout(
        state.options.body_timeout,
        spool(request.into_body(), 16_384),
    )
    .await
    {
        Ok(Ok(source)) => source,
        _ => {
            return failure(
                400,
                ErrorCode::InvalidRequest,
                "Invalid probe",
                None,
                Retry::Never,
            );
        }
    };
    let factory = state.factory.clone();
    match tokio::time::timeout(
        state.options.work_timeout,
        crate::http::spawn_blocking(move || {
            let _permit = permit;
            let mut source = source;
            source.as_file_mut().rewind()?;
            let mut bytes = Vec::new();
            source.as_file_mut().read_to_end(&mut bytes)?;
            let Serde(probe): Serde<Probe> = alt_json::from_slice(&bytes)?;
            let mut store = factory.open()?;
            let result = cloud.readiness(&mut store, probe)?;
            alt_json::to_vec(&Serde(&result)).map_err(Error::from)
        }),
    )
    .await
    {
        Ok(Ok(Ok(body))) => response(200, "application/json", full(body)),
        Ok(Ok(Err(Error::Unauthorized))) => failure(
            403,
            ErrorCode::Forbidden,
            "Runtime binding mismatch",
            None,
            Retry::Never,
        ),
        _ => unavailable(),
    }
}
