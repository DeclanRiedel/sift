//! Native ownership and permission-loss acceptance through the real HTTP/SDK path.
#![cfg(feature = "live-mssql")]
use futures::FutureExt;
use sift_client_sdk::Client;
use sift_driver_api::{ConnHandle, Driver};
use sift_driver_sqlserver::MssqlDriver;
use sift_protocol::{
    ApplySqlServerSecurityRequest, ConnectionId, ConnectionSpec, Engine, EngineConnectionSpec,
    ExecuteRequest, MssqlConnectionSpec, OpenConnectionRequest, Page, SessionId,
    SqlServerOwnedObjectKind as Kind, SqlServerSecurityAction as Action, Value,
};
use sift_server::{
    http::{app, AppState, AuthState},
    DriverRegistry, SessionStore,
};

fn admin_spec() -> ConnectionSpec {
    ConnectionSpec {
        host: std::env::var("SIFT_MSSQL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: Some(
            std::env::var("SIFT_MSSQL_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1433),
        ),
        database: Some(std::env::var("SIFT_MSSQL_DB").unwrap_or_else(|_| "master".into())),
        user: std::env::var("SIFT_MSSQL_USER").unwrap_or_else(|_| "sa".into()),
        password: Some(std::env::var("SIFT_MSSQL_PASSWORD").expect("SIFT_MSSQL_PASSWORD required")),
        ssl_mode: Some(sift_protocol::SslMode::Require),
        engine_specific: Some(EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
            trust_server_certificate: Some(true),
            ..Default::default()
        })),
    }
}
fn quote(value: &str) -> String {
    format!("[{}]", value.replace(']', "]]"))
}
async fn admin(driver: &MssqlDriver, connection: &ConnHandle, sql: String) -> anyhow::Result<()> {
    // Setup SQL can contain generated credentials; report native codes only.
    let mut stream = driver
        .execute(connection.clone(), ExecuteRequest::new(sql))
        .await
        .map_err(|e| anyhow::anyhow!("fixture execute failed ({:?})", e.native_code))?;
    while let Some(page) = stream.rows.recv().await {
        match page {
            Page::Done { .. } => return Ok(()),
            Page::Error { error } => {
                anyhow::bail!("fixture stream failed ({:?})", error.native_code)
            }
            _ => {}
        }
    }
    anyhow::bail!("fixture SQL ended without completion")
}
fn change(kind: Kind, name: &str, owner: &str) -> Action {
    Action::ChangeOwner {
        object_kind: kind,
        name: name.into(),
        new_owner: owner.into(),
    }
}
async fn apply(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    action: Action,
) -> anyhow::Result<()> {
    let preview = client
        .preview_sqlserver_security(session, connection, action)
        .await?;
    client
        .apply_sqlserver_security(
            session,
            connection,
            ApplySqlServerSecurityRequest {
                action: preview.action,
                precondition: preview.precondition,
                production_confirmed: true,
            },
        )
        .await?;
    Ok(())
}
async fn scalar(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    sql: &str,
) -> anyhow::Result<i64> {
    let result = client.execute(session, connection, sql).await?;
    match result.rows[0].values[0] {
        Value::Int64(v) => Ok(v),
        _ => anyhow::bail!("unexpected count type"),
    }
}

async fn role_and_grant_reviews_bind_native_identity(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    database: &str,
) -> anyhow::Result<()> {
    let role = "reviewed]role";
    let role_sql = quote(role);
    client.execute(session,connection,&format!("CREATE ROLE {role_sql}; CREATE USER reviewed_member WITHOUT LOGIN; CREATE USER role_editor WITHOUT LOGIN; CREATE USER schema_owner WITHOUT LOGIN")).await?;
    let add = || Action::AddRoleMember {
        role: role.into(),
        member: "reviewed_member".into(),
    };
    let stale = client
        .preview_sqlserver_security(session, connection, add())
        .await?;
    client
        .execute(
            session,
            connection,
            "DROP USER reviewed_member; CREATE USER reviewed_member WITHOUT LOGIN",
        )
        .await?;
    assert!(
        client
            .apply_sqlserver_security(
                session,
                connection,
                ApplySqlServerSecurityRequest {
                    action: stale.action,
                    precondition: stale.precondition,
                    production_confirmed: true
                }
            )
            .await
            .is_err(),
        "a recreated member must invalidate review"
    );
    let stale = client
        .preview_sqlserver_security(session, connection, add())
        .await?;
    client
        .execute(
            session,
            connection,
            &format!("DROP ROLE {role_sql}; CREATE ROLE {role_sql}"),
        )
        .await?;
    assert!(
        client
            .apply_sqlserver_security(
                session,
                connection,
                ApplySqlServerSecurityRequest {
                    action: stale.action,
                    precondition: stale.precondition,
                    production_confirmed: true
                }
            )
            .await
            .is_err(),
        "a recreated role must invalidate review"
    );
    apply(client, session, connection, add()).await?;
    let drop = || Action::DropRoleMember {
        role: role.into(),
        member: "reviewed_member".into(),
    };
    let stale = client
        .preview_sqlserver_security(session, connection, drop())
        .await?;
    client.execute(session,connection,&format!("DROP USER reviewed_member; CREATE USER reviewed_member WITHOUT LOGIN; ALTER ROLE {role_sql} ADD MEMBER reviewed_member")).await?;
    assert!(client
        .apply_sqlserver_security(
            session,
            connection,
            ApplySqlServerSecurityRequest {
                action: stale.action,
                precondition: stale.precondition,
                production_confirmed: true
            }
        )
        .await
        .is_err());
    apply(client, session, connection, drop()).await?;
    let schema = "reviewed]schema";
    let schema_sql = quote(schema);
    client
        .execute(
            session,
            connection,
            &format!("CREATE SCHEMA {schema_sql} AUTHORIZATION dbo"),
        )
        .await?;
    let grant = || Action::GrantSchemaSelect {
        schema: schema.into(),
        grantee: "reviewed_member".into(),
    };
    let revoke = || Action::RevokeSchemaSelect {
        schema: schema.into(),
        grantee: "reviewed_member".into(),
    };
    let stale = client
        .preview_sqlserver_security(session, connection, grant())
        .await?;
    client
        .execute(
            session,
            connection,
            "DROP USER reviewed_member; CREATE USER reviewed_member WITHOUT LOGIN",
        )
        .await?;
    assert!(
        client
            .apply_sqlserver_security(
                session,
                connection,
                ApplySqlServerSecurityRequest {
                    action: stale.action,
                    precondition: stale.precondition,
                    production_confirmed: true
                }
            )
            .await
            .is_err(),
        "a recreated grantee must invalidate review"
    );
    apply(client, session, connection, grant()).await?;
    let stale = client
        .preview_sqlserver_security(session, connection, revoke())
        .await?;
    client.execute(session,connection,&format!("ALTER AUTHORIZATION ON SCHEMA::{schema_sql} TO schema_owner; GRANT SELECT ON SCHEMA::{schema_sql} TO reviewed_member")).await?;
    assert!(client
        .apply_sqlserver_security(
            session,
            connection,
            ApplySqlServerSecurityRequest {
                action: stale.action,
                precondition: stale.precondition,
                production_confirmed: true
            }
        )
        .await
        .is_err());
    apply(client, session, connection, revoke()).await?;
    // A narrow role editor must work with identifier-bearing permission names.
    client.execute(session,connection,&format!("GRANT ALTER ON ROLE::{role_sql} TO role_editor; GRANT VIEW DEFINITION ON USER::reviewed_member TO role_editor; GRANT CREATE ROLE TO role_editor")).await?;
    let create = || Action::CreateDatabaseRole {
        name: "reviewed_actor_role".into(),
    };
    let stale = client
        .preview_sqlserver_security(session, connection, create())
        .await?;
    client
        .execute(session, connection, "EXECUTE AS USER='role_editor'")
        .await?;
    assert!(
        client
            .apply_sqlserver_security(
                session,
                connection,
                ApplySqlServerSecurityRequest {
                    action: stale.action,
                    precondition: stale.precondition,
                    production_confirmed: true
                }
            )
            .await
            .is_err(),
        "a changed database actor must invalidate review"
    );
    apply(client, session, connection, create()).await?;
    apply(client, session, connection, add()).await?;
    apply(client, session, connection, drop()).await?;
    client.execute(session, connection, "REVERT").await?;
    let create = Action::CreateDatabaseRole {
        name: "reviewed_context_role".into(),
    };
    let before = client
        .preview_sqlserver_security(session, connection, create.clone())
        .await?;
    client.execute(session, connection, "USE master").await?;
    let elsewhere = client
        .preview_sqlserver_security(session, connection, create)
        .await?;
    client
        .execute(session, connection, &format!("USE {}", quote(database)))
        .await?;
    assert_ne!(
        before.precondition, elsewhere.precondition,
        "USE must change review identity"
    );
    // Independent grantors cannot be reduced to one arbitrary catalog row.
    client.execute(session,connection,&format!("CREATE USER grantor_one WITHOUT LOGIN; CREATE USER grantor_two WITHOUT LOGIN; GRANT SELECT ON SCHEMA::{schema_sql} TO grantor_one WITH GRANT OPTION; GRANT SELECT ON SCHEMA::{schema_sql} TO grantor_two WITH GRANT OPTION; GRANT SELECT ON SCHEMA::{schema_sql} TO reviewed_member AS grantor_one; GRANT SELECT ON SCHEMA::{schema_sql} TO reviewed_member AS grantor_two")).await?;
    let refused = client
        .preview_sqlserver_security(session, connection, revoke())
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("grantor"));
    Ok(())
}

#[tokio::test]
async fn ownership_reviews_native_permission_loss_authority_and_catalog_identity() {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let database = format!("sift_ownership_{suffix}");
    let login_a = format!("sift_owner_a_{suffix}");
    let login_b = format!("sift_owner_b_{suffix}");
    let password = format!("Sift!{suffix}Aa9"); // process memory only
    let driver = MssqlDriver::new();
    let admin_connection = driver
        .open(&admin_spec())
        .await
        .expect("native admin opens");
    let sessions = SessionStore::new(
        DriverRegistry::builder()
            .register(MssqlDriver::new())
            .build(),
    );
    let router = app(AppState {
        sessions: sessions.clone(),
        rooms: Default::default(),
        shutdown: Default::default(),
        auth: AuthState::default(),
        metadata: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut session_to_close = None;
    let outcome = std::panic::AssertUnwindSafe(async {
        admin(&driver, &admin_connection, format!("CREATE DATABASE {}",quote(&database))).await?;
        for login in [&login_a, &login_b] {
            admin(&driver, &admin_connection, format!("CREATE LOGIN {} WITH PASSWORD='{password}'; ALTER LOGIN {} DISABLE",quote(login),quote(login))).await?;
        }
        let session = client.open_session(Some("ownership acceptance".into())).await?.id;
        session_to_close = Some(session);
        let mut spec = admin_spec(); spec.database = Some(database.clone());
        let connection = client.open_connection(session,OpenConnectionRequest { provider_id: Engine::SqlServer.provider_id(),spec }).await?.id;
        role_and_grant_reviews_bind_native_identity(&client,session,connection,&database).await?;
        let initial_owner = client.read_sqlserver_security(session,connection).await?.database_owner.expect("initial native owner resolves");
        let schema = "owned]; DROP DATABASE accidental; --";
        let schema_sql = quote(schema);
        client.execute(session, connection, "CREATE USER owner_one WITHOUT LOGIN; CREATE USER owner_two WITHOUT LOGIN; CREATE USER viewer WITHOUT LOGIN; CREATE USER limited WITHOUT LOGIN; CREATE ROLE owner_role").await?;
        client.execute(session, connection, &format!("CREATE SCHEMA {schema_sql} AUTHORIZATION owner_one")).await?;
        client.execute(session, connection, &format!(r#"CREATE TABLE {schema_sql}.inherited(id int); CREATE TABLE {schema_sql}.explicit_owner(id int); ALTER AUTHORIZATION ON OBJECT::{schema_sql}.explicit_owner TO owner_role; CREATE TYPE {schema_sql}.alias_type FROM int; CREATE XML SCHEMA COLLECTION {schema_sql}.xml_type AS N'<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="value" type="xs:int"/></xs:schema>'"#)).await?;
        client.execute(session, connection, &format!("GRANT SELECT ON SCHEMA::{schema_sql} TO viewer; GRANT SELECT ON OBJECT::{schema_sql}.inherited TO viewer; GRANT UPDATE(id) ON OBJECT::{schema_sql}.inherited TO viewer; GRANT SELECT ON OBJECT::{schema_sql}.explicit_owner TO viewer; GRANT REFERENCES ON TYPE::{schema_sql}.alias_type TO viewer; GRANT REFERENCES ON XML SCHEMA COLLECTION::{schema_sql}.xml_type TO viewer")).await?;
        let action = change(Kind::Schema,schema,"owner_two");
        let preview = client.preview_sqlserver_security(session,connection,action.clone()).await?;
        assert_eq!(preview.sql,format!("ALTER AUTHORIZATION ON SCHEMA::{schema_sql} TO [owner_two]"));
        assert!(preview.warning.contains("1 explicit schema permission rows"));
        assert!(preview.warning.contains("4 permission rows on schema-contained"));
        assert!(client.apply_sqlserver_security(session,connection,ApplySqlServerSecurityRequest { action: action.clone(),precondition:preview.precondition.clone(),production_confirmed:false }).await.is_err());
        // A grant introduced after review must invalidate the permission-loss review.
        client.execute(session,connection,&format!("GRANT INSERT ON OBJECT::{schema_sql}.inherited TO viewer")).await?;
        assert!(client.apply_sqlserver_security(session,connection,ApplySqlServerSecurityRequest {action:action.clone(),precondition:preview.precondition,production_confirmed:true}).await.is_err());
        apply(&client,session,connection,action).await?;
        let report = client.read_sqlserver_security(session,connection).await?;
        assert!(report.schemas.items.iter().any(|item|item.schema==schema && item.owner=="owner_two"));
        assert_eq!(scalar(&client,session,connection,"SELECT COUNT_BIG(*) FROM sys.database_permissions p JOIN sys.database_principals u ON u.principal_id=p.grantee_principal_id WHERE u.name='viewer' AND p.class IN (1,3,6,10)").await?,1);
        assert_eq!(scalar(&client,session,connection,&format!("SELECT COUNT_BIG(*) FROM sys.database_permissions p WHERE p.class=1 AND p.major_id=OBJECT_ID(N'{}.explicit_owner') AND p.permission_name='SELECT'",schema_sql.replace("'","''"))).await?,1);
        assert!(client.preview_sqlserver_security(session,connection,change(Kind::Schema,schema,"owner_two")).await.is_err());
        admin(&driver,&admin_connection,format!("USE {}; CREATE APPLICATION ROLE owner_application WITH PASSWORD='{password}'; USE master",quote(&database))).await?;
        apply(&client,session,connection,change(Kind::Schema,schema,"owner_application")).await?;
        admin(&driver,&admin_connection,format!("USE {}; CREATE CERTIFICATE owner_certificate ENCRYPTION BY PASSWORD='{password}' WITH SUBJECT='ownership acceptance'; CREATE USER certificate_owner FROM CERTIFICATE owner_certificate; CREATE ASYMMETRIC KEY owner_key WITH ALGORITHM=RSA_2048 ENCRYPTION BY PASSWORD='{password}'; CREATE USER key_owner FROM ASYMMETRIC KEY owner_key; USE master",quote(&database))).await?;
        apply(&client,session,connection,change(Kind::Schema,schema,"certificate_owner")).await?;
        apply(&client,session,connection,change(Kind::Schema,schema,"key_owner")).await?;
        apply(&client,session,connection,change(Kind::Schema,schema,"owner_role")).await?;
        client.execute(session,connection,"EXECUTE AS USER='limited'").await?;
        assert!(client.preview_sqlserver_security(session,connection,change(Kind::Schema,schema,"owner_one")).await.is_err());
        client.execute(session,connection,"REVERT").await?;
        // TAKE OWNERSHIP and target IMPERSONATE alone cannot attest complete impact.
        client.execute(session,connection,&format!("GRANT TAKE OWNERSHIP ON SCHEMA::{schema_sql} TO limited; GRANT IMPERSONATE ON USER::owner_one TO limited")).await?;
        client.execute(session,connection,"EXECUTE AS USER='limited'").await?;
        let denied = client.preview_sqlserver_security(session,connection,change(Kind::Schema,schema,"owner_one")).await.unwrap_err();
        assert!(denied.to_string().contains("VIEW DEFINITION"));
        client.execute(session,connection,"REVERT").await?;
        client.execute(session,connection,"GRANT VIEW DEFINITION TO limited").await?;
        // Database VIEW DEFINITION alone does not prevent hidden affected objects.
        client.execute(session,connection,&format!("CREATE ROLE metadata_hider; ALTER ROLE metadata_hider ADD MEMBER limited; DENY VIEW DEFINITION ON OBJECT::{schema_sql}.inherited TO metadata_hider")).await?;
        client.execute(session,connection,"EXECUTE AS USER='limited'").await?;
        let denied = client.preview_sqlserver_security(session,connection,change(Kind::Schema,schema,"owner_one")).await.unwrap_err();
        assert!(denied.to_string().contains("metadata DENYs"));
        client.execute(session,connection,"REVERT").await?;
        client.execute(session,connection,&format!("REVOKE VIEW DEFINITION ON OBJECT::{schema_sql}.inherited FROM metadata_hider; DENY VIEW DEFINITION ON USER::viewer TO limited")).await?;
        client.execute(session,connection,"EXECUTE AS USER='limited'").await?;
        let denied = client.preview_sqlserver_security(session,connection,change(Kind::Schema,schema,"owner_one")).await.unwrap_err();
        assert!(denied.to_string().contains("metadata DENYs"));
        client.execute(session,connection,"REVERT").await?;
        client.execute(session,connection,"REVOKE VIEW DEFINITION ON USER::viewer FROM limited").await?;
        client.execute(session,connection,"EXECUTE AS USER='limited'").await?;
        apply(&client,session,connection,change(Kind::Schema,schema,"owner_one")).await?;
        client.execute(session,connection,"REVERT").await?;
        for protected in ["dbo","sys","INFORMATION_SCHEMA"] {
            assert!(client.preview_sqlserver_security(session,connection,change(Kind::Schema,protected,"owner_two")).await.is_err());
        }
        assert!(client.preview_sqlserver_security(session,connection,change(Kind::Database,"master",&login_a)).await.is_err());
        apply(&client,session,connection,change(Kind::Database,&database,&login_a)).await?;
        assert_eq!(client.read_sqlserver_security(session,connection).await?.database_owner.as_deref(),Some(login_a.as_str()));
        let stale = client.preview_sqlserver_security(session,connection,change(Kind::Database,&database,&login_b)).await?;
        admin(&driver,&admin_connection,format!("DROP LOGIN {}; CREATE LOGIN {} WITH PASSWORD='{password}'; ALTER LOGIN {} DISABLE",quote(&login_b),quote(&login_b),quote(&login_b))).await?;
        assert!(client.apply_sqlserver_security(session,connection,ApplySqlServerSecurityRequest {action:stale.action,precondition:stale.precondition,production_confirmed:true}).await.is_err());
        apply(&client,session,connection,change(Kind::Database,&database,&login_b)).await?;
        apply(&client,session,connection,change(Kind::Database,&database,&initial_owner)).await?;
        let master = client.open_connection(session,OpenConnectionRequest {provider_id:Engine::SqlServer.provider_id(),spec:admin_spec()}).await?.id;
        assert!(client.preview_sqlserver_security(session,master,change(Kind::Database,"master",&login_a)).await.is_err());
        assert!(sessions.list_operations().iter().any(|entry| matches!(entry.operation,sift_protocol::Operation::ApplySqlServerSecurity { action: Action::ChangeOwner { .. },.. }) && entry.status==sift_protocol::OperationStatus::Succeeded));
        Ok::<(), anyhow::Error>(())
    }).catch_unwind().await;
    if let Some(session) = session_to_close {
        let _ = client.close_session(session).await;
    }
    server.abort();
    let cleanup = admin(&driver,&admin_connection,format!("USE master; IF DB_ID('{database}') IS NOT NULL BEGIN ALTER DATABASE {} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {}; END; IF SUSER_ID('{login_a}') IS NOT NULL DROP LOGIN {}; IF SUSER_ID('{login_b}') IS NOT NULL DROP LOGIN {}",quote(&database),quote(&database),quote(&login_a),quote(&login_b))).await;
    let _ = driver.close(admin_connection).await;
    cleanup.expect("disposable ownership fixture cleanup");
    match outcome {
        Ok(result) => result.expect("native ownership acceptance"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
