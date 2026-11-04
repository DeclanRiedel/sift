//! Native attachment, routing, stale-preview and ownership acceptance.
#![cfg(all(unix, feature = "live-pg"))]

#[path = "common/postgres.rs"]
mod postgres_fixture;

use postgres_fixture::Cluster;
use sift_protocol::{
    ApplyPostgresObjectRequest, ConnectionSpec, Engine, ExecuteRequestHttp, OpenSessionRequest,
    PostgresObjectAction, PostgresPartitionBound as Bound, PostgresPartitionValue as BoundValue,
    SslMode, Value,
};
use sift_server::{postgres_workbench, DriverRegistry, SessionStore};

#[tokio::test]
async fn typed_attachments_preserve_routing_and_refuse_changed_catalogs() {
    let cluster = Cluster::start();
    cluster.sql("CREATE SCHEMA proof");
    let store = SessionStore::new(
        DriverRegistry::builder()
            .register(sift_driver_postgres::PgDriver::new())
            .build(),
    );
    let session = store
        .open_session(OpenSessionRequest {
            tag: None,
            tenant_id: None,
        })
        .id;
    let connection = store
        .open_connection(
            session,
            Engine::Postgres,
            ConnectionSpec {
                host: cluster.0.path().to_str().unwrap().into(),
                port: Some(5432),
                database: Some("postgres".into()),
                user: "sift".into(),
                password: None,
                ssl_mode: Some(SslMode::Disable),
                engine_specific: None,
            },
        )
        .await
        .unwrap()
        .id;
    let request = |sql| ExecuteRequestHttp {
        connection,
        sql,
        params: vec![],
        tx: None,
        room_id: None,
        connection_profile_id: None,
        transform: None,
        source: None,
    };
    let cases = [
        (
            "bigint",
            Bound::Range {
                from: vec![BoundValue::Literal("0".into())],
                to: vec![BoundValue::Literal("100".into())],
            },
            "42",
        ),
        (
            "text",
            Bound::List {
                values: vec![
                    BoundValue::Literal("north'west".into()),
                    BoundValue::Literal("NULL".into()),
                    BoundValue::Null,
                ],
            },
            "E'north''west'",
        ),
        (
            "bigint",
            Bound::Hash {
                modulus: 1,
                remainder: 0,
            },
            "42",
        ),
        ("bigint", Bound::Default, "42"),
    ];
    for (index, (data_type, bound, inserted)) in cases.into_iter().enumerate() {
        let strategy = match bound {
            Bound::List { .. } => "LIST",
            Bound::Hash { .. } => "HASH",
            _ => "RANGE",
        };
        let parent = format!("p{index}");
        let child = format!("c{index}");
        cluster.sql(&format!("CREATE TABLE proof.{parent}(id {data_type}) PARTITION BY {strategy}(id); CREATE TABLE proof.{child}(id {data_type})"));
        let action = PostgresObjectAction::AttachPartition {
            parent_schema: "proof".into(),
            parent: parent.clone(),
            child_schema: "proof".into(),
            child: child.clone(),
            bound,
        };
        let preview = postgres_workbench::preview(&store, session, connection, action.clone())
            .await
            .unwrap();
        if index == 0 {
            cluster.sql(&format!("CREATE INDEX changed_index ON proof.{child}(id)"));
            assert!(postgres_workbench::apply(
                &store,
                session,
                connection,
                ApplyPostgresObjectRequest {
                    action: action.clone(),
                    precondition: preview.precondition,
                    confirmed: true,
                    production_confirmed: false,
                }
            )
            .await
            .is_err());
            let still_unattached = cluster.sql(&format!(
                "SELECT NOT relispartition FROM pg_class WHERE oid='proof.{child}'::regclass"
            ));
            assert_eq!(still_unattached, "t");
        }
        let preview = postgres_workbench::preview(&store, session, connection, action)
            .await
            .unwrap();
        if index == 0 {
            assert!(preview
                .impact
                .as_ref()
                .unwrap()
                .objects
                .iter()
                .any(|object| object.contains("changed_index")));
        }
        postgres_workbench::apply(
            &store,
            session,
            connection,
            ApplyPostgresObjectRequest {
                action: preview.action,
                precondition: preview.precondition,
                confirmed: true,
                production_confirmed: false,
            },
        )
        .await
        .unwrap();
        store
            .execute_http(
                session,
                request(format!("INSERT INTO proof.{parent} VALUES ({inserted})")),
            )
            .await
            .unwrap();
        let routed = store
            .execute_http(
                session,
                request(format!(
                    "SELECT tableoid::regclass::text FROM proof.{parent}"
                )),
            )
            .await
            .unwrap();
        assert_eq!(
            routed.rows[0].values,
            vec![Value::Text(format!("proof.{child}"))]
        );
        let detach = PostgresObjectAction::DetachPartition {
            parent_schema: "proof".into(),
            parent,
            child_schema: "proof".into(),
            child: child.clone(),
        };
        let preview = postgres_workbench::preview(&store, session, connection, detach)
            .await
            .unwrap();
        postgres_workbench::apply(
            &store,
            session,
            connection,
            ApplyPostgresObjectRequest {
                action: preview.action,
                precondition: preview.precondition,
                confirmed: true,
                production_confirmed: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            cluster.sql(&format!("SELECT count(*) FROM proof.{child}")),
            "1"
        );
    }
    cluster.sql("CREATE ROLE restricted NOLOGIN; CREATE TABLE proof.wrong(id text)");
    let wrong = PostgresObjectAction::AttachPartition {
        parent_schema: "proof".into(),
        parent: "p0".into(),
        child_schema: "proof".into(),
        child: "wrong".into(),
        bound: Bound::Default,
    };
    assert!(
        postgres_workbench::preview(&store, session, connection, wrong)
            .await
            .is_err()
    );
    store
        .execute_http(session, request("SET ROLE restricted".into()))
        .await
        .unwrap();
    let denied = PostgresObjectAction::AttachPartition {
        parent_schema: "proof".into(),
        parent: "p0".into(),
        child_schema: "proof".into(),
        child: "c0".into(),
        bound: Bound::Default,
    };
    assert!(matches!(
        postgres_workbench::preview(&store, session, connection, denied).await,
        Err(sift_server::error::ApiError::Forbidden(_))
    ));
    store.close_session(session).unwrap();
}
