//! Receiving boundary for queued extension requests; assertions confer no authority.
use super::*;
use sift_protocol::handshake::{
    ExtensionCallerIdentity, EXPECTED_AUTH_SESSION_HEADER, EXPECTED_DAEMON_HEADER,
    EXPECTED_INSTANCE_HEADER, EXPECTED_PRINCIPAL_HEADER,
};

fn value<'a>(headers: &'a HeaderMap, name: &'static str) -> ApiResult<&'a str> {
    if headers.get_all(name).iter().count() != 1 {
        return Err(ApiError::BadRequest(
            "extension caller scope is required".into(),
        ));
    }
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .ok_or_else(|| ApiError::BadRequest("invalid extension caller scope".into()))
}

pub(super) fn receive(state: &AppState, headers: &HeaderMap) -> ApiResult<ExtensionCallerIdentity> {
    let instance = value(headers, EXPECTED_INSTANCE_HEADER)?;
    let daemon = value(headers, EXPECTED_DAEMON_HEADER)?;
    let principal = value(headers, EXPECTED_PRINCIPAL_HEADER)?
        .parse::<i64>()
        .map_err(|_| ApiError::BadRequest("invalid extension caller scope".into()))?;
    let session = value(headers, EXPECTED_AUTH_SESSION_HEADER)?;
    if instance != state.auth.instance_id || daemon != state.auth.daemon_generation {
        return Err(ApiError::Forbidden(
            "extension server changed; review again".into(),
        ));
    }
    Ok(ExtensionCallerIdentity {
        instance_id: instance.into(),
        daemon_generation: daemon.into(),
        principal_id: principal,
        auth_session_id: (session != "-").then(|| session.to_owned()),
    })
}

pub(super) fn authorize(req: &Request<Body>, actor: &AuthContext) -> ApiResult<()> {
    if let Some(expected) = req.extensions().get::<ExtensionCallerIdentity>() {
        if expected.principal_id != actor.principal_id.0
            || expected.auth_session_id != actor.auth_session_id
        {
            return Err(ApiError::Forbidden(
                "extension account changed; review again".into(),
            ));
        }
    }
    Ok(())
}
