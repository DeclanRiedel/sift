//! Live server plan and transaction acceptance; no pre-seeded table dependency.
#![cfg(any(feature = "live-pg", feature = "live-mssql"))]

use sift_protocol::*;
use sift_server::{plan, process, DriverRegistry, SessionStore};

async fn acceptance(engine: Engine) {
    let registry = match engine {
        Engine::Sqlite => panic!("SQLite uses the local sqlite_provider fixture"),
        Engine::Postgres => DriverRegistry::builder()
            .register(sift_driver_postgres::PgDriver::new())
            .build(),
        Engine::SqlServer => DriverRegistry::builder()
            .register(sift_driver_sqlserver::MssqlDriver::new())
            .build(),
    };
    let store = SessionStore::new(registry);
    let pg = engine == Engine::Postgres;
    let prefix = if pg { "SIFT_PG" } else { "SIFT_MSSQL" };
    let get = |key: &str, default: &str| {
        std::env::var(format!("{prefix}_{key}")).unwrap_or_else(|_| default.into())
    };
    let session = store.open_session(OpenSessionRequest {
        tag: Some("live-server-acceptance".into()),
        tenant_id: None,
    });
    let connection = store
        .open_connection(
            session.id,
            engine,
            ConnectionSpec {
                host: get(
                    "HOST",
                    if pg {
                        "/tmp/sift-demo-pg-socket"
                    } else {
                        "127.0.0.1"
                    },
                ),
                port: Some(
                    get("PORT", if pg { "5433" } else { "1433" })
                        .parse()
                        .unwrap(),
                ),
                database: Some(get("DB", if pg { "sifttest" } else { "master" })),
                user: get("USER", if pg { "sift" } else { "sa" }),
                password: std::env::var(format!("{prefix}_PASSWORD")).ok(),
                ssl_mode: Some(SslMode::Disable),
                engine_specific: if pg {
                    None
                } else {
                    Some(EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
                        trust_server_certificate: Some(true),
                        ..Default::default()
                    }))
                },
            },
        )
        .await
        .unwrap();
    let request = |sql: String, tx: Option<TxId>| ExecuteRequestHttp {
        connection: connection.id,
        sql,
        params: vec![],
        tx: tx.map(|tx_id| TxHandleRef {
            tx_id,
            connection: connection.id,
            mode: TxMode::default(),
        }),
        room_id: None,
        connection_profile_id: None,
        transform: None,
        source: None,
    };
    let table = format!("sift_plan_{}", uuid::Uuid::new_v4().simple());
    store
        .execute_http(
            session.id,
            request(
                format!("CREATE TABLE {table} (id int NOT NULL PRIMARY KEY, amount int NOT NULL)"),
                None,
            ),
        )
        .await
        .unwrap();
    let tx = store
        .begin_transaction(
            session.id,
            BeginTransactionRequest {
                connection: connection.id,
                mode: TxMode::default(),
            },
        )
        .await
        .unwrap();
    store
        .execute_http(
            session.id,
            request(format!("INSERT INTO {table} VALUES (1,17)"), Some(tx.tx_id)),
        )
        .await
        .unwrap();
    let savepoint = SavepointRequest {
        connection: connection.id,
        tx_id: tx.tx_id,
        name: "keep_first".into(),
    };
    store
        .create_savepoint(session.id, savepoint.clone())
        .await
        .unwrap();
    store
        .execute_http(
            session.id,
            request(format!("INSERT INTO {table} VALUES (2,99)"), Some(tx.tx_id)),
        )
        .await
        .unwrap();
    store
        .rollback_to_savepoint(session.id, savepoint)
        .await
        .unwrap();
    store
        .commit_transaction(
            session.id,
            EndTransactionRequest {
                connection: connection.id,
                tx_id: tx.tx_id,
            },
        )
        .await
        .unwrap();
    let rows = store
        .execute_http(
            session.id,
            request(format!("SELECT id,amount FROM {table}"), None),
        )
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    for analyze in [false, true] {
        let response = plan::explain(
            &store,
            session.id,
            connection.id,
            &ExplainRequest {
                connection: connection.id,
                sql: format!(
                    "SELECT * FROM {table} WHERE id = {}",
                    if pg { "$1" } else { "@P1" }
                ),
                params: vec![Value::Int32(1)],
                analyze,
            },
        )
        .await;
        if !pg && analyze {
            assert!(matches!(
                response,
                Err(sift_server::error::ApiError::Driver(DriverError {
                    code: Code::UnsupportedForEngine,
                    ..
                }))
            ));
            continue;
        }
        let response = response.unwrap();
        assert_eq!(response.engine, engine);
        assert_eq!(response.analyzed, analyze);
        assert!(!response.root.op.is_empty());
        assert!(!response.raw.is_empty());
    }
    if pg {
        let created = format!("{table}_analyze");
        plan::explain(
            &store,
            session.id,
            connection.id,
            &ExplainRequest {
                connection: connection.id,
                sql: format!("SELECT * INTO {created} FROM {table}"),
                params: vec![],
                analyze: true,
            },
        )
        .await
        .unwrap();
        let result = store
            .execute_http(
                session.id,
                request(format!("SELECT to_regclass('{created}') IS NULL"), None),
            )
            .await
            .unwrap();
        assert!(matches!(result.rows[0].values[0], Value::Bool(true)));
    }
    process::list(&store, session.id, connection.id)
        .await
        .unwrap();
    store
        .execute_http(session.id, request(format!("DROP TABLE {table}"), None))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let mut stream = store.execute_stream(session.id, connection.id, ExecuteRequest::new(
        if pg { "SELECT generate_series(1,100000)" } else {
            "SELECT TOP (100000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) FROM sys.all_objects a CROSS JOIN sys.all_objects b"
        }), None).await.unwrap();
    let mut count = 0;
    let mut completed = false;
    while let Some(page) = stream.rows.recv().await {
        match page {
            Page::Rows { rows } => {
                assert!(rows.len() <= 1024);
                count += rows.len();
            }
            Page::Error { error } => panic!("large stream failed: {error}"),
            Page::Done { .. } => completed = true,
            _ => {}
        }
    }
    assert!(completed);
    assert_eq!(count, 100000);
    eprintln!("{engine}: 100000 streamed rows in {:?}", started.elapsed());
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    store.set_request_timeout(std::time::Duration::from_millis(150));
    let started = std::time::Instant::now();
    let timed = store
        .execute_http(
            session.id,
            request(
                if pg {
                    "SELECT pg_sleep(5)".into()
                } else {
                    "WAITFOR DELAY '00:00:05'; SELECT 1".into()
                },
                None,
            ),
        )
        .await;
    assert!(timed.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    if !pg {
        return;
    } // SQL Server timeout deliberately invalidates the connection.
    store.set_request_timeout(std::time::Duration::from_secs(30));
    store
        .execute_http(session.id, request("SELECT 1".into(), None))
        .await
        .unwrap();
    store
        .close_connection(session.id, connection.id)
        .await
        .unwrap();
}

#[cfg(feature = "live-pg")]
#[tokio::test]
async fn postgres_server_plans_processes_and_savepoints() {
    acceptance(Engine::Postgres).await;
}

#[cfg(feature = "live-mssql")]
#[tokio::test]
async fn sqlserver_server_plans_processes_and_savepoints() {
    acceptance(Engine::SqlServer).await;
}
