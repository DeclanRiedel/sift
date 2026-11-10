//! Saved provider profiles must load schema through HTTP, not just connect.
use axum::{
    body::{to_bytes, Body},
    http::{Request, Response, StatusCode},
};
use sift_driver_api::mock::MockDriver;
use sift_metadata as meta;
use sift_protocol::*;
use sift_server::{
    http::{app, AppState, AuthState},
    DriverRegistry, RoomRuntime, SessionStore, Shutdown,
};
use std::sync::Arc;
use tower::ServiceExt;

async fn json<T: serde::de::DeserializeOwned>(response: Response<Body>) -> T {
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn provider_configuration_loads_schema_over_http_and_reuses_legacy_cache() {
    for engine in [Engine::SqlServer, Engine::Postgres] {
        let schema_name = if engine == Engine::SqlServer {
            "dbo"
        } else {
            "public"
        };
        let options = if engine == Engine::SqlServer {
            serde_json::json!({"mars":false,"encrypt":true,"trust_server_certificate":false})
        } else {
            serde_json::json!({"application_name":"sift"})
        };
        let configuration = serde_json::json!({"host":"fixture.invalid","database":"app","user":"reader","engine_specific":options});
        let mut shallow = SchemaSnapshot::empty(SchemaScope::shallow());
        shallow.trees = vec![CatalogTree {
            name: "app".into(),
            schemas: vec![SchemaTree {
                name: schema_name.into(),
                objects: vec![ObjectInfo::new("items", ObjectKind::Table)],
            }],
        }];
        let mut path = ObjectPath::new("items");
        path.schema = Some(schema_name.into());
        let mut deep = shallow.clone();
        deep.scope = SchemaScope::deep(path);
        deep.trees[0].schemas[0].objects[0].columns = vec![ColumnMetadata::new(
            "id",
            TypeRef::Primitive(PrimitiveType::Int32),
        )];
        let info = ServerInfo {
            provider: engine.provider_ref("test"),
            server_version: "mock".into(),
            current_database: "app".into(),
            current_user: "reader".into(),
            pool_warm_slots: None,
        };
        let driver = Arc::new(
            MockDriver::builder()
                .engine(engine)
                .ping_ok(info.clone())
                .ping_ok(info)
                .schema_ok(shallow)
                .schema_ok(deep)
                .build(),
        );
        let registry = DriverRegistry::new();
        registry
            .providers()
            .replace(
                [Arc::new(sift_server::registry::BuiltinProviderAdapter::new(
                    driver.clone(),
                ))
                    as Arc<dyn sift_server::registry::DatabaseProvider>],
            )
            .unwrap();
        let metadata =
            meta::MetadataStore::open_in_memory(Arc::new(meta::MemorySecretStore::new())).unwrap();
        metadata.bootstrap_local("schema cache regression").unwrap();
        let router = app(AppState {
            sessions: SessionStore::new(registry),
            rooms: RoomRuntime::default(),
            auth: AuthState::default(),
            metadata: Some(metadata.clone()),
            shutdown: Shutdown::default(),
        });
        let session: SessionInfo = json(
            router
                .clone()
                .oneshot(
                    Request::post("/v1/sessions")
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        for legacy in [false, true] {
            let mut saved = configuration.clone();
            if legacy {
                saved["engine_specific"]["engine"] =
                    serde_json::json!(if engine == Engine::SqlServer {
                        "sql_server"
                    } else {
                        "postgres"
                    });
            }
            let profile = metadata
                .upsert_connection_profile(
                    meta::TenantId(1),
                    meta::PrincipalId(1),
                    meta::NewConnectionProfile {
                        name: if legacy { "legacy" } else { "provider" }.into(),
                        provider_id: engine.provider_id(),
                        configuration: saved.clone(),
                        semantic_engine: Some(engine),
                        credentials: None,
                        credential_mode: meta::CredentialMode::Shared,
                        tags: vec![],
                    },
                )
                .await
                .unwrap();
            assert_eq!(profile.configuration, saved);
            let connection: ConnectionInfo = json(
                router
                    .clone()
                    .oneshot(
                        Request::post(format!(
                            "/v1/sessions/{}/connections/from-profile",
                            session.id
                        ))
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::json!({"tenant_id":1,"profile_id":profile.id.0})
                                .to_string(),
                        ))
                        .unwrap(),
                    )
                    .await
                    .unwrap(),
            )
            .await;
            for depth in [false, true] {
                let query = if depth {
                    format!("?depth=deep&schema={schema_name}&object=items")
                } else {
                    String::new()
                };
                for _ in 0..2 {
                    let response = router
                        .clone()
                        .oneshot(
                            Request::get(format!(
                                "/v1/sessions/{}/connections/{}/schema{query}",
                                session.id, connection.id
                            ))
                            .body(Body::empty())
                            .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::OK,
                        "provider {engine:?}, legacy {legacy}, deep {depth}"
                    );
                    let snapshot: SchemaSnapshot = json(response).await;
                    let table = &snapshot.trees[0].schemas[0].objects[0];
                    assert_eq!(table.name, "items");
                    if depth {
                        assert_eq!(table.columns[0].name, "id");
                    }
                }
            }
        }
        assert_eq!(
            driver
                .invocations()
                .iter()
                .filter(|&&name| name == "schema")
                .count(),
            2
        );
    }
}
