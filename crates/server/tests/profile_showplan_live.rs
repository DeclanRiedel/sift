//! Disposable SQL Server login acceptance for measured plans.
#![cfg(feature = "live-mssql")]

use sift_driver_api::{ConnHandle, Driver};
use sift_driver_sqlserver::MssqlDriver;
use sift_protocol::{
    Code, ConnectionSpec, Engine, EngineConnectionSpec, ExecuteRequest, ExecuteRequestHttp,
    MssqlConnectionSpec, OpenSessionRequest, Page, ProfileRequest, SslMode,
};
use sift_server::{DriverRegistry, SessionStore};

fn admin_spec() -> ConnectionSpec {
    ConnectionSpec {
        host: std::env::var("SIFT_MSSQL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: Some(
            std::env::var("SIFT_MSSQL_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1433),
        ),
        database: Some(std::env::var("SIFT_MSSQL_DB").unwrap_or_else(|_| "master".into())),
        user: std::env::var("SIFT_MSSQL_USER").unwrap_or_else(|_| "sa".into()),
        password: Some(std::env::var("SIFT_MSSQL_PASSWORD").expect("SIFT_MSSQL_PASSWORD required")),
        ssl_mode: Some(SslMode::Require),
        engine_specific: Some(EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
            trust_server_certificate: Some(true),
            ..Default::default()
        })),
    }
}

async fn run_admin(
    driver: &MssqlDriver,
    connection: &ConnHandle,
    sql: String,
) -> Result<(), sift_protocol::DriverError> {
    let mut stream = driver
        .execute(connection.clone(), ExecuteRequest::new(sql))
        .await?;
    while let Some(page) = stream.rows.recv().await {
        match page {
            Page::Done { .. } => return Ok(()),
            Page::Error { error } => return Err(error),
            _ => {}
        }
    }
    Err(sift_protocol::DriverError::new(
        Code::DriverInternal,
        "fixture SQL ended without completion",
    ))
}

#[tokio::test]
async fn restricted_login_requires_showplan_and_profile_cleanup_preserves_source() {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let login = format!("sift_profile_{suffix}");
    let table = format!("sift_profile_{suffix}");
    // Generated only in process memory. Never print SQL containing this value.
    let password = format!("Sift!{suffix}aA9");
    let driver = MssqlDriver::new();
    let admin = driver.open(&admin_spec()).await.expect("admin opens");
    let store = SessionStore::new(
        DriverRegistry::builder()
            .register(MssqlDriver::new())
            .build(),
    );
    let mut restricted_connection = None;
    let outcome: Result<(), String> = async {
        run_admin(
            &driver,
            &admin,
            format!("CREATE TABLE dbo.[{table}] (id int NOT NULL PRIMARY KEY)"),
        )
        .await
        .map_err(|error| format!("fixture operation failed ({:?})", error.native_code))?;
        run_admin(
            &driver,
            &admin,
            format!("CREATE LOGIN [{login}] WITH PASSWORD = '{password}'"),
        )
        .await
        .map_err(|error| format!("fixture operation failed ({:?})", error.native_code))?;
        run_admin(
            &driver,
            &admin,
            format!("CREATE USER [{login}] FOR LOGIN [{login}]"),
        )
        .await
        .map_err(|error| format!("fixture operation failed ({:?})", error.native_code))?;
        run_admin(
            &driver,
            &admin,
            format!("GRANT SELECT ON OBJECT::dbo.[{table}] TO [{login}]"),
        )
        .await
        .map_err(|error| format!("fixture operation failed ({:?})", error.native_code))?;
        run_admin(&driver, &admin, format!("GRANT SHOWPLAN TO [{login}]"))
            .await
            .map_err(|error| format!("fixture operation failed ({:?})", error.native_code))?;

        let session = store.open_session(OpenSessionRequest {
            tag: Some("restricted-showplan-acceptance".into()),
            tenant_id: None,
        });
        let mut spec = admin_spec();
        spec.user = login.clone();
        spec.password = Some(password);
        let source = store
            .open_connection(session.id, Engine::SqlServer, spec)
            .await
            .map_err(|error| error.to_string())?;
        restricted_connection = Some((session.id, source.id));
        let request = || ProfileRequest {
            connection: source.id,
            run_id: uuid::Uuid::new_v4(),
            sql: format!("SELECT id FROM dbo.[{table}]"),
            params: Vec::new(),
            timeout_ms: 10_000,
            workload_confirmed: true,
        };
        let allowed = store
            .profile(session.id, source.id, request())
            .await
            .map_err(|error| error.to_string())?;
        if !allowed.plan.analyzed || allowed.plan.root.actual_rows.is_none() {
            return Err("SHOWPLAN grant did not produce an actual plan".into());
        }
        run_admin(&driver, &admin, format!("REVOKE SHOWPLAN FROM [{login}]"))
            .await
            .map_err(|error| format!("fixture revoke failed ({:?})", error.native_code))?;
        let denied = match store.profile(session.id, source.id, request()).await {
            Ok(_) => return Err("Profile unexpectedly succeeded without SHOWPLAN".into()),
            Err(error) => error,
        };
        if !matches!(
            denied,
            sift_server::error::ApiError::Driver(ref error)
                if error.message.to_ascii_lowercase().contains("showplan")
        ) {
            return Err("Profile refusal was not a SHOWPLAN permission error".into());
        }
        let read = store
            .execute_http(
                session.id,
                ExecuteRequestHttp {
                    connection: source.id,
                    sql: format!("SELECT id FROM dbo.[{table}]"),
                    params: Vec::new(),
                    tx: None,
                    room_id: None,
                    connection_profile_id: None,
                    transform: None,
                    source: None,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        if !read.rows.is_empty() {
            return Err("fixture table unexpectedly contained rows".into());
        }
        Ok(())
    }
    .await;

    if let Some((session, connection)) = restricted_connection {
        let _ = store.close_connection(session, connection).await;
    }
    let cleanup = run_admin(
        &driver,
        &admin,
        format!(
            "IF USER_ID('{login}') IS NOT NULL DROP USER [{login}]; \
             IF SUSER_ID('{login}') IS NOT NULL DROP LOGIN [{login}]; \
             IF OBJECT_ID('dbo.{table}', 'U') IS NOT NULL DROP TABLE dbo.[{table}]"
        ),
    )
    .await;
    let _ = driver.close(admin).await;
    assert!(
        cleanup.is_ok(),
        "disposable SQL Server fixture cleanup failed"
    );
    outcome.expect("restricted SHOWPLAN acceptance failed");
}
