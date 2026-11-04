//! Native policy lifecycle, row enforcement and audit privacy through real HTTP.
#![cfg(all(unix, feature = "live-pg"))]
#[path = "common/postgres.rs"]
mod postgres_fixture;
use postgres_fixture::Cluster;
use sift_client_sdk::Client;
use sift_metadata::{MemorySecretStore, MetadataStore};
use sift_protocol::{
    ApplyPostgresObjectRequest, ConnectionId, PostgresObjectAction as Action,
    PostgresPolicyCommand as Command, PostgresPolicyDefinition, PostgresPolicyRole as Role,
    PostgresRowSecuritySetting as Setting, SessionId, SslMode, Value,
};
use sift_server::{
    http::{app, AppState, AuthState},
    DriverRegistry, SessionStore,
};
use std::sync::Arc;

async fn apply(client: &Client, session: SessionId, connection: ConnectionId, action: Action) {
    let preview = client
        .preview_postgres_object(session, connection, action)
        .await
        .unwrap();
    client
        .apply_postgres_object(
            session,
            connection,
            ApplyPostgresObjectRequest {
                action: preview.action,
                precondition: preview.precondition,
                confirmed: true,
                production_confirmed: true,
            },
        )
        .await
        .unwrap();
}
async fn count(client: &Client, session: SessionId, connection: ConnectionId) -> i64 {
    let response = client
        .execute(session, connection, "SELECT count(*) FROM proof.rows")
        .await
        .unwrap();
    match response.rows[0].values[0] {
        Value::Int64(value) => value,
        _ => panic!("native count type"),
    }
}
fn setting(setting: Setting) -> Action {
    Action::SetRowSecurity {
        schema: "proof".into(),
        table: "rows".into(),
        setting,
    }
}
fn policy(name: &str, using: &str) -> Action {
    Action::CreatePolicy {
        schema: "proof".into(),
        table: "rows".into(),
        name: name.into(),
        definition: PostgresPolicyDefinition {
            command: Command::All,
            permissive: true,
            roles: vec![Role::Named("policy_reader".into())],
            using_expression: Some(using.into()),
            check_expression: Some("owner_name = CURRENT_USER".into()),
        },
    }
}

#[tokio::test]
async fn policies_enforce_rows_and_writes_and_keep_conditions_out_of_audit() {
    let cluster = Cluster::start();
    cluster.sql("CREATE ROLE policy_owner NOLOGIN; CREATE ROLE policy_reader NOLOGIN; CREATE SCHEMA proof AUTHORIZATION policy_owner; SET ROLE policy_owner; CREATE TABLE proof.rows(owner_name text, value integer); INSERT INTO proof.rows VALUES ('policy_reader',1),('other',2); GRANT USAGE ON SCHEMA proof TO policy_reader; GRANT SELECT,INSERT ON proof.rows TO policy_reader");
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("RLS acceptance").unwrap();
    let sessions = SessionStore::new(
        DriverRegistry::builder()
            .register(sift_driver_postgres::PgDriver::new())
            .build(),
    );
    sessions.set_audit_store(metadata.clone());
    let router = app(AppState {
        sessions: sessions.clone(),
        rooms: Default::default(),
        shutdown: Default::default(),
        auth: AuthState::default(),
        metadata: Some(metadata.clone()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let session = client
        .open_session(Some("RLS acceptance".into()))
        .await
        .unwrap()
        .id;
    let connection = client.open_connection(session, serde_json::from_value(serde_json::json!({"provider_id":"sift/postgres", "host":cluster.0.path().to_str().unwrap(), "database":"postgres", "user":"sift", "ssl_mode":SslMode::Disable})).unwrap()).await.unwrap().id;
    client
        .execute(session, connection, "SET ROLE policy_owner")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET standard_conforming_strings=off")
        .await
        .unwrap();
    assert!(client
        .preview_postgres_object(session, connection, policy("legacy_string_mode", "true"))
        .await
        .is_err());
    client
        .execute(session, connection, "SET standard_conforming_strings=on")
        .await
        .unwrap();
    apply(
        &client,
        session,
        connection,
        policy(
            "quoted_literal",
            "owner_name <> 'x''; DROP TABLE proof.rows; --'",
        ),
    )
    .await;
    apply(
        &client,
        session,
        connection,
        Action::DropPolicy {
            schema: "proof".into(),
            table: "rows".into(),
            name: "quoted_literal".into(),
        },
    )
    .await;
    let action = policy("owned_rows", "owner_name = CURRENT_USER");
    let preview = client
        .preview_postgres_object(session, connection, action.clone())
        .await
        .unwrap();
    assert!(client
        .apply_postgres_object(
            session,
            connection,
            ApplyPostgresObjectRequest {
                action: action.clone(),
                precondition: preview.precondition.clone(),
                confirmed: true,
                production_confirmed: false
            }
        )
        .await
        .is_err());
    client
        .apply_postgres_object(
            session,
            connection,
            ApplyPostgresObjectRequest {
                action,
                precondition: preview.precondition,
                confirmed: true,
                production_confirmed: true,
            },
        )
        .await
        .unwrap();
    // Exercise each native command/mode grammar, including command-specific clauses.
    for command in [
        Command::Select,
        Command::Insert,
        Command::Update,
        Command::Delete,
    ] {
        let create = Action::CreatePolicy {
            schema: "proof".into(),
            table: "rows".into(),
            name: "command_probe".into(),
            definition: PostgresPolicyDefinition {
                command,
                permissive: false,
                roles: vec![Role::Public],
                using_expression: (command != Command::Insert).then(|| "value < 2".into()),
                check_expression: matches!(command, Command::Insert | Command::Update)
                    .then(|| "value < 2".into()),
            },
        };
        apply(&client, session, connection, create).await;
        apply(
            &client,
            session,
            connection,
            Action::DropPolicy {
                schema: "proof".into(),
                table: "rows".into(),
                name: "command_probe".into(),
            },
        )
        .await;
    }
    apply(&client, session, connection, setting(Setting::Enable)).await;
    assert_eq!(count(&client, session, connection).await, 2); // owner bypass
    apply(&client, session, connection, setting(Setting::Force)).await;
    assert_eq!(count(&client, session, connection).await, 0); // owner has no applicable policy
    apply(&client, session, connection, setting(Setting::NoForce)).await;
    assert_eq!(count(&client, session, connection).await, 2);
    client
        .execute(session, connection, "RESET ROLE")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET ROLE policy_reader")
        .await
        .unwrap();
    assert_eq!(count(&client, session, connection).await, 1);
    assert!(client
        .execute(
            session,
            connection,
            "INSERT INTO proof.rows VALUES ('other',3)"
        )
        .await
        .is_err());
    client
        .execute(
            session,
            connection,
            "INSERT INTO proof.rows VALUES ('policy_reader',3)",
        )
        .await
        .unwrap();
    assert_eq!(count(&client, session, connection).await, 2);
    let drop = Action::DropPolicy {
        schema: "proof".into(),
        table: "rows".into(),
        name: "owned_rows".into(),
    };
    assert!(client
        .preview_postgres_object(session, connection, drop.clone())
        .await
        .is_err());
    client
        .execute(session, connection, "RESET ROLE")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET ROLE policy_owner")
        .await
        .unwrap();
    let stale = client
        .preview_postgres_object(session, connection, drop.clone())
        .await
        .unwrap();
    apply(
        &client,
        session,
        connection,
        Action::AlterPolicy {
            schema: "proof".into(),
            table: "rows".into(),
            name: "owned_rows".into(),
            roles: Some(vec![Role::Public]),
            using_expression: Some("false".into()),
            check_expression: None,
        },
    )
    .await;
    assert!(client
        .apply_postgres_object(
            session,
            connection,
            ApplyPostgresObjectRequest {
                action: drop.clone(),
                precondition: stale.precondition,
                confirmed: true,
                production_confirmed: true
            }
        )
        .await
        .is_err());
    client
        .execute(session, connection, "RESET ROLE")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET ROLE policy_reader")
        .await
        .unwrap();
    assert_eq!(count(&client, session, connection).await, 0);
    client
        .execute(session, connection, "RESET ROLE")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET ROLE policy_owner")
        .await
        .unwrap();
    apply(&client, session, connection, drop).await;
    client
        .execute(session, connection, "RESET ROLE")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET ROLE policy_reader")
        .await
        .unwrap();
    assert_eq!(count(&client, session, connection).await, 0); // enabled without policies denies
    client
        .execute(session, connection, "RESET ROLE")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET ROLE policy_owner")
        .await
        .unwrap();
    apply(&client, session, connection, setting(Setting::Disable)).await;
    assert_eq!(count(&client, session, connection).await, 3);
    assert!(client
        .preview_postgres_object(
            session,
            connection,
            policy("injection", "true); DROP TABLE proof.rows; --")
        )
        .await
        .is_err());
    assert_eq!(count(&client, session, connection).await, 3);
    // Restrictive policies combine with the permissive policy instead of granting access alone.
    let mut restrictive = policy("restricted", "false");
    if let Action::CreatePolicy { definition, .. } = &mut restrictive {
        definition.permissive = false;
    }
    apply(&client, session, connection, policy("permitted", "true")).await;
    apply(&client, session, connection, restrictive).await;
    apply(&client, session, connection, setting(Setting::Enable)).await;
    client
        .execute(session, connection, "RESET ROLE")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET ROLE policy_reader")
        .await
        .unwrap();
    assert_eq!(count(&client, session, connection).await, 0);
    client
        .execute(session, connection, "RESET ROLE")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET ROLE policy_owner")
        .await
        .unwrap();
    apply(
        &client,
        session,
        connection,
        Action::DropPolicy {
            schema: "proof".into(),
            table: "rows".into(),
            name: "restricted".into(),
        },
    )
    .await;
    client
        .execute(session, connection, "RESET ROLE")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET ROLE policy_reader")
        .await
        .unwrap();
    assert_eq!(count(&client, session, connection).await, 3);
    client
        .execute(session, connection, "RESET ROLE")
        .await
        .unwrap();
    client
        .execute(session, connection, "SET ROLE policy_owner")
        .await
        .unwrap();
    let private = "private_policy_literal_93817";
    let bad = client
        .preview_postgres_object(
            session,
            connection,
            policy("bad_type", &format!("value = '{private}'")),
        )
        .await
        .unwrap();
    let error = client
        .apply_postgres_object(
            session,
            connection,
            ApplyPostgresObjectRequest {
                action: bad.action,
                precondition: bad.precondition,
                confirmed: true,
                production_confirmed: true,
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains(private)); // requester keeps native diagnostic
    let correlation = match &error {
        sift_client_sdk::Error::Server { error, .. } => error.correlation_id.clone().unwrap(),
        _ => panic!("expected correlated native error"),
    };
    assert!(!serde_json::to_string(&sessions.list_operations())
        .unwrap()
        .contains(private));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let rows = metadata.list_operation_audit(1000).unwrap();
            assert!(!format!("{rows:?}").contains(private));
            if rows.iter().any(|row| {
                row.correlation_id.as_deref() == Some(correlation.as_str())
                    && row.error_message.as_deref() == Some("PostgreSQL RLS action failed")
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    client.close_session(session).await.unwrap();
    server.abort();
}
