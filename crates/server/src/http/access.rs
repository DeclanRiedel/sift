use sift_metadata::TenantId;
use sift_protocol::{CursorId, SessionId};

pub(super) fn tenant_from_path(path: &str) -> Option<TenantId> {
    ["/v1/metadata/tenants/", "/v1/admin/tenants/"]
        .iter()
        .find_map(|prefix| {
            path.strip_prefix(prefix)?
                .split('/')
                .next()?
                .parse::<i64>()
                .ok()
                .map(TenantId)
        })
}

#[derive(Clone, Copy)]
pub(super) enum MetadataTenantResource {
    Room(i64),
    Document(i64),
    Connection(i64),
    SavedQuery(i64),
}

pub(super) fn metadata_tenant_resource(path: &str) -> Option<MetadataTenantResource> {
    for (prefix, constructor) in [
        (
            "/v1/metadata/rooms/",
            MetadataTenantResource::Room as fn(i64) -> MetadataTenantResource,
        ),
        (
            "/v1/metadata/documents/",
            MetadataTenantResource::Document as fn(i64) -> MetadataTenantResource,
        ),
        (
            "/v1/metadata/connections/",
            MetadataTenantResource::Connection as fn(i64) -> MetadataTenantResource,
        ),
        (
            "/v1/metadata/saved-queries/",
            MetadataTenantResource::SavedQuery as fn(i64) -> MetadataTenantResource,
        ),
    ] {
        if let Some(id) = path
            .strip_prefix(prefix)
            .and_then(|rest| rest.split('/').next())
            .and_then(|value| value.parse::<i64>().ok())
        {
            return Some(constructor(id));
        }
    }
    None
}

pub(super) fn is_public_path(path: &str) -> bool {
    matches!(
        path,
        "/v1/handshake"
            | "/v1/health"
            | "/v1/ready"
            | "/v1/openapi.json"
            | "/v1/auth/login"
            | "/v1/auth/password/reset"
            | "/v1/auth/refresh"
            | "/v1/auth/github/start"
            | "/v1/auth/github/callback"
            | "/v1/auth/github/exchange"
            | "/v1/auth/keys/challenge"
            | "/v1/auth/keys/authenticate"
            | "/v1/auth/ssh-proxy/exchange"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RouteAccess {
    Public,
    Authenticated,
    Session(SessionId),
    Cursor(CursorId),
}

/// Classify every current route family at the authentication boundary.
/// Tenant/room/admin detail is evaluated by the typed handler after this
/// authenticated floor; session-derived resources are enforced here because
/// every operation below them inherits the session owner.
pub(super) fn route_access(path: &str) -> RouteAccess {
    if is_public_path(path) {
        return RouteAccess::Public;
    }
    if let Some(rest) = path.strip_prefix("/v1/sessions/") {
        if let Some(id) = rest.split('/').next().and_then(|part| part.parse().ok()) {
            return RouteAccess::Session(SessionId(id));
        }
    }
    if let Some(rest) = path.strip_prefix("/v1/cursors/") {
        if let Some(id) = rest.split('/').next().and_then(|part| part.parse().ok()) {
            return RouteAccess::Cursor(CursorId(id));
        }
    }
    RouteAccess::Authenticated
}
