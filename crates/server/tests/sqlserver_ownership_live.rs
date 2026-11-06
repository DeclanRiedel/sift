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

async fn effective_privileges(
    client: &Client,
    session: SessionId,
    root: ConnectionId,
    database: &str,
    login: &str,
    password: &str,
    sessions: &SessionStore,
) -> anyhow::Result<()> {
    use sift_protocol::{SqlServerPrivilegeQuery as Query, SqlServerPrivilegeReport as Report};
    let reader = "priv_reader']";
    let schema = "priv ] schema";
    let table = "table.dot";
    let column = "col]name";
    client.execute(session,root,&format!("CREATE USER {} WITHOUT LOGIN; CREATE USER priv_fixed WITHOUT LOGIN; CREATE USER priv_public WITHOUT LOGIN; CREATE USER priv_owner WITHOUT LOGIN; CREATE ROLE priv_role; ALTER ROLE priv_role ADD MEMBER {}; ALTER ROLE db_datareader ADD MEMBER priv_fixed",quote(reader),quote(reader))).await?;
    client
        .execute(session, root, &format!("CREATE SCHEMA {}", quote(schema)))
        .await?;
    client
        .execute(
            session,
            root,
            "CREATE SCHEMA priv_owned AUTHORIZATION priv_owner",
        )
        .await?;
    client.execute(session,root,&format!("CREATE TABLE {}.{}({} int,hidden int); INSERT {}.{} VALUES(1,1),(2,2),(3,3),(4,4); GRANT SELECT ON SCHEMA::{} TO priv_role; DENY SELECT ON OBJECT::{}.{} TO {}; GRANT SELECT ON OBJECT::{}.{} ({}) TO {}; GRANT INSERT ON SCHEMA::{} TO public; CREATE SEQUENCE {}.seq AS bigint",quote(schema),quote(table),quote(column),quote(schema),quote(table),quote(schema),quote(schema),quote(table),quote(reader),quote(schema),quote(table),quote(column),quote(reader),quote(schema),quote(schema))).await?;
    client
        .execute(
            session,
            root,
            &format!("CREATE PROCEDURE {}.p AS SELECT 1", quote(schema)),
        )
        .await?;
    let query = |principal: Option<&str>| Query {
        principal: principal.map(str::to_string),
        offset: 0,
        limit: Some(100),
    };
    let allowed = |r: &Report, kind: &str, name: &str, permission: &str| -> Option<bool> {
        r.objects
            .iter()
            .find(|o| format!("{:?}", o.kind).eq_ignore_ascii_case(kind) && o.name == name)
            .unwrap()
            .checks
            .iter()
            .find(|c| c.privilege == permission)
            .unwrap()
            .allowed
    };
    let report = client
        .sqlserver_privileges(session, root, query(Some(reader)))
        .await?;
    assert!(report.database_user_token);
    assert_eq!(report.principal, reader);
    assert_eq!(allowed(&report, "column", column, "SELECT"), Some(true));
    assert_eq!(allowed(&report, "column", "hidden", "SELECT"), Some(false));
    let fixed = client
        .sqlserver_privileges(session, root, query(Some("priv_fixed")))
        .await?;
    assert_eq!(allowed(&fixed, "table", table, "SELECT"), Some(true));
    let public = client
        .sqlserver_privileges(session, root, query(Some("priv_public")))
        .await?;
    assert_eq!(allowed(&public, "table", table, "INSERT"), Some(true));
    let owner = client
        .sqlserver_privileges(session, root, query(Some("priv_owner")))
        .await?;
    assert_eq!(
        allowed(&owner, "schema", "priv_owned", "CONTROL"),
        Some(true)
    );
    let first = client
        .sqlserver_privileges(
            session,
            root,
            Query {
                limit: Some(2),
                ..query(Some(reader))
            },
        )
        .await?;
    assert_eq!(first.objects.len(), 2);
    assert_eq!(first.next_offset, Some(2));
    let next = client
        .sqlserver_privileges(
            session,
            root,
            Query {
                offset: 2,
                limit: Some(2),
                ..query(Some(reader))
            },
        )
        .await?;
    assert!(!first
        .objects
        .iter()
        .any(|a| next.objects.iter().any(|b| a.object_id == b.object_id
            && a.column_number == b.column_number
            && std::mem::discriminant(&a.kind) == std::mem::discriminant(&b.kind))));
    for principal in ["priv_role", "missing"] {
        assert!(client
            .sqlserver_privileges(session, root, query(Some(principal)))
            .await
            .is_err());
    }
    assert!(client
        .sqlserver_privileges(
            session,
            root,
            Query {
                limit: Some(101),
                ..query(None)
            }
        )
        .await
        .is_err());
    client
        .execute(
            session,
            root,
            &format!(
                "ALTER LOGIN {} ENABLE; CREATE USER priv_operator FOR LOGIN {}",
                quote(login),
                quote(login)
            ),
        )
        .await?;
    let mut spec = admin_spec();
    spec.database = Some(database.into());
    spec.user = login.into();
    spec.password = Some(password.into());
    if let Some(EngineConnectionSpec::SqlServer(options)) = &mut spec.engine_specific {
        options.startup_sql = vec!["SET ROWCOUNT 2".into()];
    }
    let operator = client
        .open_connection(
            session,
            OpenConnectionRequest {
                provider_id: Engine::SqlServer.provider_id(),
                spec,
            },
        )
        .await?
        .id;
    assert!(client
        .sqlserver_privileges(session, operator, query(None))
        .await
        .is_err());
    client.execute(session,root,&format!("GRANT VIEW DEFINITION,VIEW SECURITY DEFINITION TO priv_operator; GRANT SELECT ON OBJECT::{}.{} TO priv_operator",quote(schema),quote(table))).await?;
    let own = client
        .sqlserver_privileges(session, operator, query(None))
        .await?;
    assert_eq!(own.principal, "priv_operator");
    assert!(!own.database_user_token);
    assert!(client
        .sqlserver_privileges(session, operator, query(Some(reader)))
        .await
        .is_err());
    client
        .execute(
            session,
            root,
            &format!(
                "GRANT IMPERSONATE ON USER::{} TO priv_operator",
                quote(reader)
            ),
        )
        .await?;
    let selected = client
        .sqlserver_privileges(session, operator, query(Some(reader)))
        .await?;
    assert_eq!(allowed(&selected, "column", column, "SELECT"), Some(true));
    client
        .execute(
            session,
            root,
            &format!(
                "DENY VIEW DEFINITION ON OBJECT::{}.{} TO priv_operator",
                quote(schema),
                quote(table)
            ),
        )
        .await?;
    assert!(client
        .sqlserver_privileges(session, operator, query(Some(reader)))
        .await
        .is_err());
    client
        .execute(
            session,
            root,
            &format!(
                "REVOKE VIEW DEFINITION ON OBJECT::{}.{} FROM priv_operator",
                quote(schema),
                quote(table)
            ),
        )
        .await?;
    assert!(client
        .sqlserver_privileges(session, operator, query(Some(reader)))
        .await
        .is_ok());
    assert_eq!(
        client
            .execute(
                session,
                operator,
                &format!(
                    "SELECT {} FROM {}.{}",
                    quote(column),
                    quote(schema),
                    quote(table)
                )
            )
            .await?
            .rows
            .len(),
        2
    );
    assert_eq!(
        scalar(
            client,
            session,
            operator,
            "SELECT CONVERT(bigint,USER_ID())"
        )
        .await?,
        i64::from(own.principal_id)
    );
    client.close_connection(session, operator).await?;
    client
        .execute(
            session,
            root,
            &format!(
                "DROP USER priv_operator; ALTER LOGIN {} DISABLE",
                quote(login)
            ),
        )
        .await?;
    assert!(sessions.list_operations().iter().any(|entry|matches!(entry.operation,sift_protocol::Operation::ReadSqlServerPrivileges {connection,..} if connection==root)&&entry.status==sift_protocol::OperationStatus::Succeeded));
    // Inject native DDL after the production query captures identities. This
    // deterministically models interleaving DDL without sleeps or production hooks.
    let mut proof_spec = admin_spec();
    proof_spec.database = Some(database.into());
    let table_path = format!("{}.{}", quote(schema), quote(table));
    let renamed_path = format!("{}.[renamed table]", quote(schema));
    for (mutation, restore) in [
        (
            format!(
                "EXEC sys.sp_rename N'{}',N'renamed table',N'OBJECT';",
                table_path.replace('\'', "''")
            ),
            Some(format!(
                "EXEC sys.sp_rename N'{}',N'{}',N'OBJECT'",
                renamed_path.replace('\'', "''"),
                table.replace('\'', "''")
            )),
        ),
        (
            format!(
                "DROP USER {}; CREATE USER {} WITHOUT LOGIN;",
                quote(reader),
                quote(reader)
            ),
            None,
        ),
    ] {
        let native = MssqlDriver::new();
        let handle = native.open(&proof_spec).await?;
        let sql = include_str!("../src/sqlserver_privileges/state.sql").replacen(
            "BEGIN TRY",
            &format!("{mutation}\nBEGIN TRY"),
            1,
        );
        let sql = format!(
            "DECLARE @P1 nvarchar(128)=N'{}',@P2 int=0,@P3 int=101;\n{sql}",
            reader.replace('\'', "''")
        );
        let code = match native
            .execute(handle.clone(), ExecuteRequest::new(sql))
            .await
        {
            Err(error) => error.native_code,
            Ok(mut stream) => {
                let mut code = None;
                while let Some(page) = stream.rows.recv().await {
                    if let Page::Error { error } = page {
                        code = error.native_code;
                    }
                }
                code
            }
        };
        native.close(handle).await?;
        if let Some(sql) = restore {
            client.execute(session, root, &sql).await?;
        }
        assert_eq!(
            code.as_deref(),
            Some("50003"),
            "native identity guard rejects interleaved replacement"
        );
    }
    Ok(())
}

async fn index_maintenance(
    client: &Client,
    session: SessionId,
    root: ConnectionId,
    database: &str,
    login: &str,
    password: &str,
    sessions: &SessionStore,
) -> anyhow::Result<()> {
    use sift_protocol::{SqlServerIndexAction as Kind, SqlServerIndexRequest};
    client.execute(session,root,&format!("ALTER LOGIN {} ENABLE; CREATE USER index_operator FOR LOGIN {}; CREATE TABLE dbo.index_data(id int PRIMARY KEY,v int, doubled AS (v*2) PERSISTED); INSERT dbo.index_data(id,v) VALUES(1,1),(2,2),(3,3); CREATE INDEX [index]] quoted] ON dbo.index_data(doubled) INCLUDE(v) WHERE v>0 WITH (DATA_COMPRESSION=PAGE)",quote(login),quote(login))).await?;
    let mut spec = admin_spec();
    spec.database = Some(database.into());
    spec.user = login.into();
    spec.password = Some(password.into());
    if let Some(EngineConnectionSpec::SqlServer(options)) = &mut spec.engine_specific {
        options.startup_sql = vec![
            "SET ROWCOUNT 2".into(),
            "SET NUMERIC_ROUNDABORT ON".into(),
            "SET QUOTED_IDENTIFIER OFF".into(),
            "SET ARITHABORT OFF".into(),
        ];
    }
    let operator = client
        .open_connection(
            session,
            OpenConnectionRequest {
                provider_id: Engine::SqlServer.provider_id(),
                spec,
            },
        )
        .await?
        .id;
    let request = |action| SqlServerIndexRequest {
        schema: "dbo".into(),
        table: "index_data".into(),
        index: "index] quoted".into(),
        action,
        apply: false,
        production_confirmed: false,
        backup_verified: false,
        precondition: None,
    };
    assert!(client
        .sqlserver_index_maintenance(session, operator, request(Kind::Rebuild))
        .await
        .is_err());
    client
        .execute(
            session,
            root,
            "GRANT ALTER, VIEW DEFINITION ON OBJECT::dbo.index_data TO index_operator",
        )
        .await?;
    for action in [Kind::Rebuild, Kind::Reorganize, Kind::UpdateStatistics] {
        let before_hobt=scalar(client,session,root,"SELECT hobt_id FROM sys.partitions WHERE object_id=OBJECT_ID('dbo.index_data') AND index_id=INDEXPROPERTY(OBJECT_ID('dbo.index_data'),'index] quoted','IndexID')").await?;
        let before_stats=scalar(client,session,root,"SELECT DATEDIFF_BIG(millisecond,'20000101',STATS_DATE(OBJECT_ID('dbo.index_data'),INDEXPROPERTY(OBJECT_ID('dbo.index_data'),'index] quoted','IndexID')))").await?;
        let before = client
            .execute(
                session,
                operator,
                "SELECT id FROM dbo.index_data ORDER BY id",
            )
            .await;
        assert!(
            before.is_err(),
            "ALTER/VIEW DEFINITION do not grant data SELECT"
        );
        let review = client
            .sqlserver_index_maintenance(session, operator, request(action))
            .await?;
        assert!(!review.applied);
        assert!(review.sql.contains("[index]] quoted]"));
        let mut apply = request(action);
        apply.apply = true;
        apply.precondition = Some(review.precondition.clone());
        assert!(client
            .sqlserver_index_maintenance(session, operator, apply.clone())
            .await
            .is_err());
        apply.production_confirmed = true;
        assert!(client
            .sqlserver_index_maintenance(session, operator, apply.clone())
            .await
            .is_err());
        apply.backup_verified = true;
        let report = client
            .sqlserver_index_maintenance(session, operator, apply)
            .await?;
        assert!(report.applied);
        if action == Kind::Rebuild {
            assert_ne!(scalar(client,session,root,"SELECT hobt_id FROM sys.partitions WHERE object_id=OBJECT_ID('dbo.index_data') AND index_id=INDEXPROPERTY(OBJECT_ID('dbo.index_data'),'index] quoted','IndexID')").await?,before_hobt,"native rebuild replaces physical index identity");
        }
        if action == Kind::UpdateStatistics {
            assert!(scalar(client,session,root,"SELECT DATEDIFF_BIG(millisecond,'20000101',STATS_DATE(OBJECT_ID('dbo.index_data'),INDEXPROPERTY(OBJECT_ID('dbo.index_data'),'index] quoted','IndexID')))").await?>before_stats,"statistics capture must actually advance");
        }
        assert_eq!(
            scalar(
                client,
                session,
                operator,
                "SELECT CONVERT(bigint,SESSIONPROPERTY('NUMERIC_ROUNDABORT'))"
            )
            .await?,
            1
        );
        assert_eq!(
            scalar(
                client,
                session,
                operator,
                "SELECT CONVERT(bigint,SESSIONPROPERTY('QUOTED_IDENTIFIER'))"
            )
            .await?,
            0
        );
        assert_eq!(
            scalar(
                client,
                session,
                root,
                "SELECT SUM(CONVERT(bigint,v)) FROM dbo.index_data"
            )
            .await?,
            6
        );
        assert_eq!(scalar(client,session,root,"SELECT CONVERT(bigint,data_compression) FROM sys.partitions WHERE object_id=OBJECT_ID('dbo.index_data') AND index_id=INDEXPROPERTY(OBJECT_ID('dbo.index_data'),'index] quoted','IndexID')").await?,2);
    }
    client
        .execute(
            session,
            root,
            "GRANT SELECT ON OBJECT::dbo.index_data TO index_operator",
        )
        .await?;
    assert_eq!(
        client
            .execute(
                session,
                operator,
                "SELECT id FROM dbo.index_data ORDER BY id"
            )
            .await?
            .rows
            .len(),
        2,
        "owned normalization preserves editor ROWCOUNT"
    );
    let mut stale = request(Kind::Rebuild);
    stale.apply = true;
    stale.production_confirmed = true;
    stale.backup_verified = true;
    stale.precondition = Some(
        client
            .sqlserver_index_maintenance(session, operator, request(Kind::Rebuild))
            .await?
            .precondition,
    );
    client
        .execute(
            session,
            root,
            "REVOKE ALTER ON OBJECT::dbo.index_data FROM index_operator",
        )
        .await?;
    assert!(client
        .sqlserver_index_maintenance(session, operator, stale.clone())
        .await
        .is_err());
    client
        .execute(
            session,
            root,
            "GRANT ALTER ON OBJECT::dbo.index_data TO index_operator",
        )
        .await?;
    stale.precondition = Some(
        client
            .sqlserver_index_maintenance(session, operator, request(Kind::Rebuild))
            .await?
            .precondition,
    );
    client
        .execute(
            session,
            root,
            "ALTER INDEX [index]] quoted] ON dbo.index_data SET (ALLOW_PAGE_LOCKS=OFF)",
        )
        .await?;
    assert!(client
        .sqlserver_index_maintenance(session, operator, stale.clone())
        .await
        .is_err());
    assert!(client
        .sqlserver_index_maintenance(session, operator, request(Kind::Reorganize))
        .await
        .is_err());
    client
        .execute(
            session,
            root,
            "ALTER INDEX [index]] quoted] ON dbo.index_data SET (ALLOW_PAGE_LOCKS=ON)",
        )
        .await?;
    stale.precondition = Some(
        client
            .sqlserver_index_maintenance(session, operator, request(Kind::Rebuild))
            .await?
            .precondition,
    );
    client.execute(session,root,"DROP INDEX [index]] quoted] ON dbo.index_data; CREATE INDEX [index]] quoted] ON dbo.index_data(doubled) INCLUDE(v) WHERE v>0 WITH (DATA_COMPRESSION=PAGE)").await?;
    assert!(client
        .sqlserver_index_maintenance(session, operator, stale)
        .await
        .is_err());
    client
        .execute(session, root, "EXECUTE AS USER='index_operator'")
        .await?;
    assert!(
        client
            .sqlserver_index_maintenance(session, root, request(Kind::Rebuild))
            .await
            .is_err(),
        "source impersonation must not become original-login authority"
    );
    client.execute(session, root, "REVERT").await?;
    let tx = client
        .begin_transaction(session, operator, sift_protocol::TxMode::default())
        .await?;
    assert!(client
        .sqlserver_index_maintenance(session, operator, request(Kind::Rebuild))
        .await
        .is_err());
    client
        .rollback_transaction(session, operator, tx.tx_id)
        .await?;
    let mut blocker_spec = admin_spec();
    blocker_spec.database = Some(database.into());
    let blocker = client
        .open_connection(
            session,
            OpenConnectionRequest {
                provider_id: Engine::SqlServer.provider_id(),
                spec: blocker_spec,
            },
        )
        .await?
        .id;
    let blocker_spid = scalar(client, session, blocker, "SELECT CONVERT(bigint,@@SPID)").await?;
    let tx = client
        .begin_transaction(session, blocker, sift_protocol::TxMode::default())
        .await?;
    client
        .execute_in_tx(
            session,
            &tx,
            "SELECT id FROM dbo.index_data WITH (TABLOCKX,HOLDLOCK)",
        )
        .await?;
    let mut applying = request(Kind::Rebuild);
    applying.apply = true;
    applying.production_confirmed = true;
    applying.backup_verified = true;
    applying.precondition = Some(
        client
            .sqlserver_index_maintenance(session, operator, request(Kind::Rebuild))
            .await?
            .precondition,
    );
    let cancellation = tokio_util::sync::CancellationToken::new();
    let run_cancel = cancellation.clone();
    let run_store = sessions.clone();
    let task = tokio::spawn(async move {
        sift_server::sqlserver_index_maintenance::run(
            &run_store, session, operator, applying, run_cancel,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3),async {
        loop {
            if scalar(client,session,root,&format!("SELECT COUNT_BIG(*) FROM sys.dm_exec_requests r CROSS APPLY sys.dm_exec_sql_text(r.sql_handle) t WHERE r.blocking_session_id={blocker_spid} AND t.text LIKE '%ALTER INDEX%'")).await.unwrap()>0 {break;}
            assert!(!task.is_finished(),"observe blocked native ALTER INDEX before cancellation");
            tokio::task::yield_now().await;
        }
    }).await?;
    cancellation.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(8), task).await??;
    assert!(
        matches!(result,Err(sift_server::error::ApiError::Driver(ref error)) if error.code==sift_protocol::Code::QueryCanceled),
        "{result:?}"
    );
    client
        .rollback_transaction(session, blocker, tx.tx_id)
        .await?;
    client.close_connection(session, blocker).await?;
    assert_eq!(
        client
            .execute(
                session,
                operator,
                "SELECT id FROM dbo.index_data ORDER BY id"
            )
            .await?
            .rows
            .len(),
        2,
        "cancellation does not discard the editor connection"
    );
    client
        .execute(
            session,
            root,
            "ALTER INDEX [index]] quoted] ON dbo.index_data DISABLE",
        )
        .await?;
    assert!(client
        .sqlserver_index_maintenance(session, operator, request(Kind::Rebuild))
        .await
        .is_err());
    client
        .execute(
            session,
            root,
            "ALTER INDEX [index]] quoted] ON dbo.index_data REBUILD",
        )
        .await?;
    assert_eq!(
        sessions.list_operations().iter().filter(|entry| {
            matches!(&entry.operation, sift_protocol::Operation::MaintainSqlServerIndex { request, .. } if request.apply)
                && entry.status == sift_protocol::OperationStatus::Succeeded
        }).count(),
        3,
        "each acknowledged native action must have a terminal audit record"
    );
    client.close_connection(session, operator).await?;
    client
        .execute(
            session,
            root,
            &format!(
                "DROP TABLE dbo.index_data; DROP USER index_operator; ALTER LOGIN {} DISABLE",
                quote(login)
            ),
        )
        .await?;
    Ok(())
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

async fn database_user_lifecycle(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    native: (&MssqlDriver, &ConnHandle),
    database: &str,
    logins: (&str, &str, &str),
    sessions: &SessionStore,
) -> anyhow::Result<()> {
    let (driver, admin_connection) = native;
    let (login_a, login_b, password) = logins;
    let name = "mapped]'; DROP USER dbo; --";
    let literal = name.replace(char::from(39), "''");
    let schema = "user]schema";
    let create = || Action::CreateDatabaseUser {
        name: name.into(),
        login: login_a.into(),
    };
    let remap = || Action::RemapDatabaseUser {
        name: name.into(),
        login: login_b.into(),
    };
    let drop_user = || Action::DropDatabaseUser { name: name.into() };
    let set_schema = || Action::SetUserDefaultSchema {
        name: name.into(),
        schema: schema.into(),
    };
    client
        .execute(
            session,
            connection,
            &format!("CREATE SCHEMA {} AUTHORIZATION dbo", quote(schema)),
        )
        .await?;
    client.execute(session, connection, &format!("CREATE TABLE {}.user_data(marker bigint); INSERT INTO {}.user_data VALUES(17); CREATE ROLE mapped_readers", quote(schema), quote(schema))).await?;
    let preview = client
        .preview_sqlserver_security(session, connection, create())
        .await?;
    assert!(preview.warning.contains("disabled"));
    assert_eq!(
        preview.sql,
        format!(
            "CREATE USER {} FOR LOGIN {} WITH DEFAULT_SCHEMA = [dbo]",
            quote(name),
            quote(login_a)
        )
    );
    assert!(client
        .apply_sqlserver_security(
            session,
            connection,
            ApplySqlServerSecurityRequest {
                action: preview.action,
                precondition: preview.precondition,
                production_confirmed: false,
            }
        )
        .await
        .is_err());
    apply(client, session, connection, create()).await?;
    let report = client.read_sqlserver_security(session, connection).await?;
    let user = report
        .principals
        .items
        .iter()
        .find(|user| user.name == name)
        .expect("created user in catalog");
    assert_eq!(user.mapped_login.as_deref(), Some(login_a));
    assert_eq!(user.default_schema.as_deref(), Some("dbo"));
    assert!(client
        .preview_sqlserver_security(session, connection, create())
        .await
        .is_err());
    let principal_id = scalar(
        client,
        session,
        connection,
        &format!("SELECT CONVERT(bigint, USER_ID(N'{literal}'))"),
    )
    .await?;
    client
        .execute(
            session,
            connection,
            &format!(
                "ALTER ROLE mapped_readers ADD MEMBER {}; GRANT SELECT ON SCHEMA::{} TO {}",
                quote(name),
                quote(schema),
                quote(name)
            ),
        )
        .await?;
    apply(client, session, connection, set_schema()).await?;
    assert!(client
        .preview_sqlserver_security(session, connection, set_schema())
        .await
        .is_err());
    client
        .execute(
            session,
            connection,
            &format!("EXECUTE AS USER=N'{literal}'"),
        )
        .await?;
    assert_eq!(
        scalar(client, session, connection, "SELECT marker FROM user_data").await?,
        17
    );
    client.execute(session, connection, "REVERT").await?;
    let normal = client
        .preview_sqlserver_security(session, connection, remap())
        .await?;
    client
        .execute(session, connection, "SET ROWCOUNT 1")
        .await?;
    let limited = client
        .preview_sqlserver_security(session, connection, remap())
        .await?;
    assert_eq!(
        normal.precondition, limited.precondition,
        "editor ROWCOUNT must not truncate nested identity/permission review"
    );
    assert_eq!(
        client
            .execute(
                session,
                connection,
                "SELECT v FROM (VALUES(1),(2),(3)) q(v)"
            )
            .await?
            .rows
            .len(),
        1,
        "review preserves editor ROWCOUNT"
    );
    client
        .execute(session, connection, "SET ROWCOUNT 0")
        .await?;
    let stale = client
        .preview_sqlserver_security(session, connection, remap())
        .await?;
    admin(
        driver,
        admin_connection,
        format!(
            "DROP LOGIN {}; CREATE LOGIN {} WITH PASSWORD='{password}'; ALTER LOGIN {} DISABLE",
            quote(login_b),
            quote(login_b),
            quote(login_b)
        ),
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
                    production_confirmed: true,
                }
            )
            .await
            .is_err(),
        "same-name recreated login invalidates review"
    );
    apply(client, session, connection, remap()).await?;
    assert!(client
        .preview_sqlserver_security(session, connection, remap())
        .await
        .is_err());
    let report = client.read_sqlserver_security(session, connection).await?;
    let user = report
        .principals
        .items
        .iter()
        .find(|user| user.name == name)
        .unwrap();
    assert_eq!(user.mapped_login.as_deref(), Some(login_b));
    assert_eq!(user.default_schema.as_deref(), Some(schema));
    assert_eq!(
        scalar(
            client,
            session,
            connection,
            &format!("SELECT CONVERT(bigint, USER_ID(N'{literal}'))")
        )
        .await?,
        principal_id
    );
    assert_eq!(scalar(client,session,connection,&format!("SELECT COUNT_BIG(*) FROM sys.database_role_members WHERE member_principal_id=USER_ID(N'{literal}')")).await?,1);
    let duplicate = client
        .preview_sqlserver_security(
            session,
            connection,
            Action::CreateDatabaseUser {
                name: "duplicate_login_mapping".into(),
                login: login_b.into(),
            },
        )
        .await
        .unwrap_err();
    assert!(duplicate.to_string().contains("already mapped"));
    let owner = client
        .read_sqlserver_security(session, connection)
        .await?
        .database_owner
        .unwrap();
    assert!(client
        .preview_sqlserver_security(
            session,
            connection,
            Action::CreateDatabaseUser {
                name: "owner_login_mapping".into(),
                login: owner
            }
        )
        .await
        .is_err());
    assert!(client
        .preview_sqlserver_security(
            session,
            connection,
            Action::SetUserDefaultSchema {
                name: name.into(),
                schema: "missing_user_schema".into()
            }
        )
        .await
        .is_err());
    // Recreating a login creates a genuine orphan, repaired without replacing the user.
    admin(
        driver,
        admin_connection,
        format!(
            "DROP LOGIN {}; CREATE LOGIN {} WITH PASSWORD='{password}'; ALTER LOGIN {} DISABLE",
            quote(login_b),
            quote(login_b),
            quote(login_b)
        ),
    )
    .await?;
    let report = client.read_sqlserver_security(session, connection).await?;
    assert!(report
        .principals
        .items
        .iter()
        .find(|u| u.name == name)
        .unwrap()
        .mapped_login
        .is_none());
    apply(client, session, connection, remap()).await?;
    assert_eq!(
        scalar(
            client,
            session,
            connection,
            &format!("SELECT CONVERT(bigint, USER_ID(N'{literal}'))")
        )
        .await?,
        principal_id
    );
    client
        .execute(
            session,
            connection,
            &format!("EXECUTE AS USER=N'{literal}'"),
        )
        .await?;
    assert_eq!(
        scalar(client, session, connection, "SELECT marker FROM user_data").await?,
        17,
        "remapping and orphan repair preserve real SELECT rights and default-schema resolution"
    );
    client.execute(session, connection, "REVERT").await?;
    // A direct permission change and an actor/context change each invalidate review.
    let stale = client
        .preview_sqlserver_security(session, connection, drop_user())
        .await?;
    client
        .execute(
            session,
            connection,
            &format!(
                "GRANT INSERT ON SCHEMA::{} TO {}",
                quote(schema),
                quote(name)
            ),
        )
        .await?;
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
    let stale = client
        .preview_sqlserver_security(
            session,
            connection,
            Action::SetUserDefaultSchema {
                name: name.into(),
                schema: "dbo".into(),
            },
        )
        .await?;
    client.execute(session,connection,&format!("DROP USER {}; WAITFOR DELAY '00:00:00.020'; CREATE USER {} FOR LOGIN {} WITH DEFAULT_SCHEMA = {}; ALTER ROLE mapped_readers ADD MEMBER {}; GRANT SELECT, INSERT ON SCHEMA::{} TO {}",quote(name),quote(name),quote(login_b),quote(schema),quote(name),quote(schema),quote(name))).await?;
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
        "same-name user identity must be re-reviewed"
    );
    let stale = client
        .preview_sqlserver_security(
            session,
            connection,
            Action::SetUserDefaultSchema {
                name: name.into(),
                schema: "dbo".into(),
            },
        )
        .await?;
    client
        .execute(
            session,
            connection,
            &format!("EXECUTE AS USER=N'{literal}'"),
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
                    production_confirmed: true,
                }
            )
            .await
            .is_err(),
        "a changed actor must re-review a user action even when self-edit authority exists"
    );
    client.execute(session, connection, "REVERT").await?;
    let context_action = Action::CreateDatabaseUser {
        name: format!("context_{database}"),
        login: login_a.into(),
    };
    let stale = client
        .preview_sqlserver_security(session, connection, context_action)
        .await?;
    client.execute(session, connection, "USE master").await?;
    assert!(
        client
            .apply_sqlserver_security(
                session,
                connection,
                ApplySqlServerSecurityRequest {
                    action: stale.action,
                    precondition: stale.precondition,
                    production_confirmed: true,
                }
            )
            .await
            .is_err(),
        "a changed current database must invalidate a user review"
    );
    client
        .execute(session, connection, &format!("USE {}", quote(database)))
        .await?;
    client
        .execute(
            session,
            connection,
            "CREATE SCHEMA user_schema_review AUTHORIZATION dbo",
        )
        .await?;
    let stale = client
        .preview_sqlserver_security(
            session,
            connection,
            Action::SetUserDefaultSchema {
                name: name.into(),
                schema: "user_schema_review".into(),
            },
        )
        .await?;
    client
        .execute(session, connection, "DROP SCHEMA user_schema_review")
        .await?;
    client
        .execute(
            session,
            connection,
            "CREATE SCHEMA user_schema_review AUTHORIZATION schema_owner",
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
                    production_confirmed: true,
                }
            )
            .await
            .is_err(),
        "a changed schema owner/identity must invalidate review"
    );
    // Owned securables are not silently dropped or transferred.
    client
        .execute(
            session,
            connection,
            &format!(
                "ALTER AUTHORIZATION ON SCHEMA::{} TO {}",
                quote(schema),
                quote(name)
            ),
        )
        .await?;
    assert!(client
        .preview_sqlserver_security(session, connection, drop_user())
        .await
        .is_err());
    client
        .execute(
            session,
            connection,
            &format!(
                "ALTER AUTHORIZATION ON SCHEMA::{} TO dbo; GRANT SELECT ON SCHEMA::{} TO {}",
                quote(schema),
                quote(schema),
                quote(name)
            ),
        )
        .await?;
    let preview = client
        .preview_sqlserver_security(session, connection, drop_user())
        .await?;
    assert!(preview.warning.contains("1 direct role memberships"));
    apply(client, session, connection, drop_user()).await?;
    assert_eq!(
        scalar(
            client,
            session,
            connection,
            &format!("SELECT COUNT_BIG(*) FROM sys.database_principals WHERE name=N'{literal}'")
        )
        .await?,
        0
    );
    assert_eq!(
        scalar(
            client,
            session,
            connection,
            &format!("SELECT COUNT_BIG(*) FROM sys.server_principals WHERE name=N'{login_b}'")
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(
            client,
            session,
            connection,
            &format!("SELECT COUNT_BIG(*) FROM {}.user_data", quote(schema))
        )
        .await?,
        1
    );
    // WITHOUT LOGIN is valid for schema/drop, never for remapping; built-ins are protected.
    client
        .execute(session, connection, "CREATE USER user_self WITHOUT LOGIN")
        .await?;
    assert!(client
        .preview_sqlserver_security(
            session,
            connection,
            Action::RemapDatabaseUser {
                name: "user_self".into(),
                login: login_b.into()
            }
        )
        .await
        .is_err());
    for name in [
        "dbo",
        "guest",
        "sys",
        "INFORMATION_SCHEMA",
        "mapped_readers",
    ] {
        assert!(client
            .preview_sqlserver_security(
                session,
                connection,
                Action::DropDatabaseUser { name: name.into() }
            )
            .await
            .is_err());
    }
    client
        .execute(session, connection, "EXECUTE AS USER='user_self'")
        .await?;
    apply(
        client,
        session,
        connection,
        Action::SetUserDefaultSchema {
            name: "user_self".into(),
            schema: schema.into(),
        },
    )
    .await?;
    assert!(client
        .preview_sqlserver_security(
            session,
            connection,
            Action::DropDatabaseUser {
                name: "user_self".into()
            }
        )
        .await
        .is_err());
    client.execute(session, connection, "REVERT").await?;
    apply(
        client,
        session,
        connection,
        Action::DropDatabaseUser {
            name: "user_self".into(),
        },
    )
    .await?;
    // A real SQL login gets only CREATE USER + security metadata and selected-login visibility.
    let editor = Action::CreateDatabaseUser {
        name: "user_editor".into(),
        login: login_a.into(),
    };
    let stale = client
        .preview_sqlserver_security(session, connection, editor.clone())
        .await?;
    admin(
        driver,
        admin_connection,
        format!("ALTER LOGIN {} ENABLE", quote(login_a)),
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
        "changed login enablement invalidates review"
    );
    apply(client, session, connection, editor).await?;
    client.execute(session,connection,"GRANT CREATE USER TO user_editor; GRANT VIEW DEFINITION TO user_editor; GRANT VIEW SECURITY DEFINITION TO user_editor").await?;
    admin(
        driver,
        admin_connection,
        format!(
            "ALTER LOGIN {} ENABLE; GRANT VIEW DEFINITION ON LOGIN::{} TO {}",
            quote(login_a),
            quote(login_b),
            quote(login_a)
        ),
    )
    .await?;
    let mut spec = admin_spec();
    spec.database = Some(database.into());
    spec.user = login_a.into();
    spec.password = Some(password.into());
    let narrow = client
        .open_connection(
            session,
            OpenConnectionRequest {
                provider_id: Engine::SqlServer.provider_id(),
                spec,
            },
        )
        .await?
        .id;
    let create_narrow = || Action::CreateDatabaseUser {
        name: "created_by_narrow".into(),
        login: login_b.into(),
    };
    apply(client, session, narrow, create_narrow()).await?;
    assert!(client
        .preview_sqlserver_security(
            session,
            narrow,
            Action::RemapDatabaseUser {
                name: "created_by_narrow".into(),
                login: login_a.into()
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("CONTROL"));
    client
        .execute(session, connection, "GRANT CONTROL TO user_editor")
        .await?;
    for action in [
        Action::RemapDatabaseUser {
            name: "user_editor".into(),
            login: login_b.into(),
        },
        Action::DropDatabaseUser {
            name: "user_editor".into(),
        },
    ] {
        let denied = client
            .preview_sqlserver_security(session, narrow, action)
            .await
            .unwrap_err();
        assert!(
            denied.to_string().contains("400"),
            "current-actor protection applies even with database CONTROL"
        );
    }
    client
        .execute(session, connection, "REVOKE CONTROL FROM user_editor")
        .await?;
    let drop_narrow = || Action::DropDatabaseUser {
        name: "created_by_narrow".into(),
    };
    assert!(client
        .preview_sqlserver_security(session, narrow, drop_narrow())
        .await
        .is_err());
    client
        .execute(session, connection, "GRANT ALTER ANY USER TO user_editor")
        .await?;
    let stale = client
        .preview_sqlserver_security(session, narrow, drop_narrow())
        .await?;
    client
        .execute(
            session,
            connection,
            "REVOKE ALTER ANY USER FROM user_editor",
        )
        .await?;
    assert!(client
        .apply_sqlserver_security(
            session,
            narrow,
            ApplySqlServerSecurityRequest {
                action: stale.action,
                precondition: stale.precondition,
                production_confirmed: true
            }
        )
        .await
        .is_err());
    client.execute(session,connection,"GRANT ALTER ANY USER TO user_editor; DENY VIEW DEFINITION ON USER::created_by_narrow TO user_editor").await?;
    let denied = client
        .preview_sqlserver_security(session, narrow, drop_narrow())
        .await
        .unwrap_err();
    assert!(denied.to_string().contains("metadata DENYs"));
    client
        .execute(
            session,
            connection,
            "REVOKE VIEW DEFINITION ON USER::created_by_narrow FROM user_editor",
        )
        .await?;
    apply(client, session, narrow, drop_narrow()).await?;
    client.close_connection(session, narrow).await?;
    apply(
        client,
        session,
        connection,
        Action::DropDatabaseUser {
            name: "user_editor".into(),
        },
    )
    .await?;
    admin(
        driver,
        admin_connection,
        format!(
            "REVOKE VIEW DEFINITION ON LOGIN::{} FROM {}",
            quote(login_b),
            quote(login_a)
        ),
    )
    .await?;
    assert!(!serde_json::to_string(&sessions.list_operations())?.contains(password));
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
        effective_privileges(&client,session,connection,&database,&login_a,&password,&sessions).await?;
        index_maintenance(&client,session,connection,&database,&login_a,&password,&sessions).await?;
        role_and_grant_reviews_bind_native_identity(&client,session,connection,&database).await?;
        database_user_lifecycle(&client,session,connection,(&driver,&admin_connection),&database,(&login_a,&login_b,&password),&sessions).await?;
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
        for name in ["certificate_owner", "key_owner"] {
            assert!(client.preview_sqlserver_security(session,connection,Action::DropDatabaseUser {name:name.into()}).await.is_err());
        }
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
