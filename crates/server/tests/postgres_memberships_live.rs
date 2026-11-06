//! Native membership options, grantor identity, authority and stale review acceptance.
#![cfg(all(unix, feature = "live-pg"))]
#[path = "common/postgres.rs"]
mod postgres_fixture;
use postgres_fixture::Cluster;
use sift_client_sdk::Client;
use sift_metadata::{MemorySecretStore, MetadataStore};
use sift_protocol::{
    ApplyPostgresObjectRequest, ConnectionId, PostgresObjectAction as Action,
    PostgresObjectPageQuery, PostgresObjectPreview, SessionId, SslMode, Value,
};
use sift_server::{
    http::{app, AppState, AuthState},
    DriverRegistry, SessionStore,
};
use std::sync::Arc;

fn grant(role: &str, member: &str, grantor: &str, flags: [bool; 3]) -> Action {
    Action::GrantRoleMembership {
        role: role.into(),
        member: member.into(),
        grantor: grantor.into(),
        admin_option: flags[0],
        inherit_option: flags[1],
        set_option: flags[2],
    }
}
fn revoke(role: &str, member: &str, grantor: &str) -> Action {
    Action::RevokeRoleMembership {
        role: role.into(),
        member: member.into(),
        grantor: grantor.into(),
    }
}
fn confirmed(preview: PostgresObjectPreview) -> ApplyPostgresObjectRequest {
    ApplyPostgresObjectRequest {
        action: preview.action,
        precondition: preview.precondition,
        confirmed: true,
        production_confirmed: true,
    }
}
async fn apply(client: &Client, session: SessionId, connection: ConnectionId, action: Action) {
    let preview = client
        .preview_postgres_object(session, connection, action)
        .await
        .unwrap();
    client
        .apply_postgres_object(session, connection, confirmed(preview))
        .await
        .unwrap();
}
async fn open(client: &Client, session: SessionId, cluster: &Cluster, user: &str) -> ConnectionId {
    client.open_connection(session,serde_json::from_value(serde_json::json!({"provider_id":"sift/postgres","host":cluster.0.path().to_str().unwrap(),"database":"postgres","user":user,"ssl_mode":SslMode::Disable})).unwrap()).await.unwrap().id
}
#[tokio::test]
async fn membership_options_grantors_and_stale_identities_use_native_authority() {
    let cluster = Cluster::start();
    cluster.sql("CREATE ROLE members_group; CREATE ROLE members_person; CREATE ROLE members_admin LOGIN; CREATE ROLE members_plain LOGIN; CREATE ROLE members_child; GRANT members_group TO members_admin WITH ADMIN TRUE, INHERIT TRUE, SET TRUE GRANTED BY sift");
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("membership acceptance").unwrap();
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
        metadata: Some(metadata),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let session = client
        .open_session(Some("membership acceptance".into()))
        .await
        .unwrap()
        .id;
    let root = open(&client, session, &cluster, "sift").await;
    let admin = open(&client, session, &cluster, "members_admin").await;
    let plain = open(&client, session, &cluster, "members_plain").await;
    privilege_matrix(&client, session, root, plain, &cluster, &sessions).await;
    role_lifecycle(&client, session, root, plain, &cluster).await;
    role_defaults(&client, session, root, plain, &cluster).await;
    let action = grant(
        "members_group",
        "members_person",
        "members_admin",
        [false, false, true],
    );
    assert!(client
        .preview_postgres_object(session, plain, action.clone())
        .await
        .is_err());
    let preview = client
        .preview_postgres_object(session, admin, action.clone())
        .await
        .unwrap();
    assert!(preview.sql.contains("GRANTED BY \"members_admin\""));
    let mut request = confirmed(preview.clone());
    request.production_confirmed = false;
    assert!(client
        .apply_postgres_object(session, admin, request)
        .await
        .is_err());
    let mut request = confirmed(preview);
    request.confirmed = false;
    assert!(client
        .apply_postgres_object(session, admin, request)
        .await
        .is_err());
    apply(&client, session, admin, action.clone()).await;
    let actual=client.execute(session,root,"SELECT pg_has_role('members_person','members_group','MEMBER'),pg_has_role('members_person','members_group','USAGE'),pg_has_role('members_person','members_group','SET')").await.unwrap();
    assert_eq!(
        actual.rows[0].values,
        vec![Value::Bool(true), Value::Bool(false), Value::Bool(true)]
    );
    assert!(client
        .preview_postgres_object(session, admin, action)
        .await
        .is_err());
    apply(
        &client,
        session,
        admin,
        grant(
            "members_group",
            "members_person",
            "members_admin",
            [true, true, false],
        ),
    )
    .await;
    let actual=client.execute(session,root,"SELECT pg_has_role('members_person','members_group','USAGE'),pg_has_role('members_person','members_group','SET')").await.unwrap();
    assert_eq!(
        actual.rows[0].values,
        vec![Value::Bool(true), Value::Bool(false)]
    );
    let page = client
        .list_postgres_role_memberships(
            session,
            root,
            PostgresObjectPageQuery {
                offset: 0,
                limit: Some(200),
            },
        )
        .await
        .unwrap();
    assert!(page.items.iter().any(|item| item.role == "members_group"
        && item.member == "members_person"
        && item.grantor == "members_admin"
        && item.admin_option
        && item.inherit_option
        && !item.set_option));
    let first = client
        .list_postgres_role_memberships(
            session,
            root,
            PostgresObjectPageQuery {
                offset: 0,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.next_offset, Some(1));
    let second = client
        .list_postgres_role_memberships(
            session,
            root,
            PostgresObjectPageQuery {
                offset: 1,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_ne!(
        (
            &first.items[0].role,
            &first.items[0].member,
            &first.items[0].grantor
        ),
        (
            &second.items[0].role,
            &second.items[0].member,
            &second.items[0].grantor
        )
    );
    // Distinct grantors are separate edges; revoking one must preserve the other.
    apply(
        &client,
        session,
        root,
        grant(
            "members_group",
            "members_person",
            "sift",
            [false, true, true],
        ),
    )
    .await;
    let preview = client
        .preview_postgres_object(
            session,
            admin,
            revoke("members_group", "members_person", "members_admin"),
        )
        .await
        .unwrap();
    assert!(preview.warning.contains("1 other explicit grants"));
    client
        .apply_postgres_object(session, admin, confirmed(preview))
        .await
        .unwrap();
    assert_eq!(
        cluster.sql("SELECT pg_has_role('members_person','members_group','MEMBER')"),
        "t"
    );
    assert!(client
        .preview_postgres_object(
            session,
            admin,
            revoke("members_group", "members_person", "members_admin")
        )
        .await
        .is_err());
    // Native dependency refusal must leave both grant chains intact.
    apply(
        &client,
        session,
        admin,
        grant(
            "members_group",
            "members_child",
            "members_admin",
            [false, true, true],
        ),
    )
    .await;
    let preview = client
        .preview_postgres_object(
            session,
            root,
            revoke("members_group", "members_admin", "sift"),
        )
        .await
        .unwrap();
    assert!(client
        .apply_postgres_object(session, root, confirmed(preview))
        .await
        .is_err());
    assert_eq!(
        cluster.sql("SELECT pg_has_role('members_child','members_group','MEMBER')"),
        "t"
    );
    // A native cycle is rejected at apply; no successful receipt is fabricated.
    let preview = client
        .preview_postgres_object(
            session,
            root,
            grant(
                "members_person",
                "members_group",
                "sift",
                [false, true, true],
            ),
        )
        .await
        .unwrap();
    assert!(client
        .apply_postgres_object(session, root, confirmed(preview))
        .await
        .is_err());
    // Changing a selected edge, even without replacing names, invalidates review.
    let preview = client
        .preview_postgres_object(
            session,
            root,
            grant(
                "members_group",
                "members_person",
                "sift",
                [false, false, false],
            ),
        )
        .await
        .unwrap();
    cluster.sql("GRANT members_group TO members_person WITH ADMIN TRUE, INHERIT TRUE, SET TRUE GRANTED BY sift");
    assert!(client
        .apply_postgres_object(session, root, confirmed(preview))
        .await
        .is_err());
    apply(
        &client,
        session,
        root,
        revoke("members_group", "members_person", "sift"),
    )
    .await;
    let preview = client
        .preview_postgres_object(
            session,
            root,
            grant(
                "members_group",
                "members_person",
                "sift",
                [false, true, true],
            ),
        )
        .await
        .unwrap();
    cluster.sql("DROP ROLE members_person; CREATE ROLE members_person");
    assert!(client
        .apply_postgres_object(session, root, confirmed(preview))
        .await
        .is_err());
    // An identically named, equally authorized replacement grantor is a new identity.
    let grantor_preview = client
        .preview_postgres_object(
            session,
            root,
            grant(
                "members_group",
                "members_person",
                "members_admin",
                [false, true, true],
            ),
        )
        .await
        .unwrap();
    client.close_connection(session, admin).await.unwrap();
    cluster.sql("REVOKE members_group FROM members_child GRANTED BY members_admin RESTRICT; REVOKE members_group FROM members_admin GRANTED BY sift RESTRICT; DROP ROLE members_admin; CREATE ROLE members_admin LOGIN; GRANT members_group TO members_admin WITH ADMIN TRUE, INHERIT TRUE, SET TRUE GRANTED BY sift");
    assert!(client
        .apply_postgres_object(session, root, confirmed(grantor_preview))
        .await
        .is_err());
    let preview = client
        .preview_postgres_object(
            session,
            root,
            grant(
                "members_group",
                "members_person",
                "sift",
                [false, true, true],
            ),
        )
        .await
        .unwrap();
    client
        .execute(session, root, "SET ROLE members_plain")
        .await
        .unwrap();
    assert!(client
        .apply_postgres_object(session, root, confirmed(preview))
        .await
        .is_err());
    client.execute(session, root, "RESET ROLE").await.unwrap();
    assert_eq!(
        cluster.sql("SELECT pg_has_role('members_person','members_group','MEMBER')"),
        "f"
    );
    let audit = serde_json::to_string(&sessions.list_operations()).unwrap();
    assert!(audit.contains("role_membership"));
    assert!(!audit.contains("rolpassword"));
    for connection in [plain, root] {
        client.close_connection(session, connection).await.unwrap();
    }
    server.abort();
}

async fn role_defaults(
    client: &Client,
    session: SessionId,
    root: ConnectionId,
    plain: ConnectionId,
    cluster: &Cluster,
) {
    use sift_protocol::{PostgresRoleDefaultScope as Scope, PostgresRoleSetting as Setting};
    cluster.sql("CREATE ROLE defaults_target LOGIN; CREATE ROLE defaults_admin LOGIN CREATEROLE; GRANT defaults_target TO defaults_admin WITH ADMIN TRUE GRANTED BY sift; ALTER ROLE defaults_target SET app.password = 'PRIVATE-DEFAULT-SENTINEL'");
    let admin = open(client, session, cluster, "defaults_admin").await;
    let set = |scope, setting, value: &str| Action::SetRoleDefault {
        name: "defaults_target".into(),
        scope,
        setting,
        value: value.into(),
    };
    let reset = |scope, setting| Action::ResetRoleDefault {
        name: "defaults_target".into(),
        scope,
        setting,
    };
    let fresh_login = |user: &str, sql: &str| {
        let output = cluster
            .psql()
            .args(["-U", user, "-c", sql])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    assert!(client
        .preview_postgres_object(
            session,
            plain,
            set(Scope::AllDatabases, Setting::WorkMem, "1024")
        )
        .await
        .is_err());
    let existing = open(client, session, cluster, "defaults_target").await;
    let before = client
        .execute(session, existing, "SHOW work_mem")
        .await
        .unwrap()
        .rows[0]
        .values
        .clone();
    apply(
        client,
        session,
        admin,
        set(Scope::AllDatabases, Setting::WorkMem, "1024"),
    )
    .await;
    assert_eq!(
        client
            .execute(session, existing, "SHOW work_mem")
            .await
            .unwrap()
            .rows[0]
            .values,
        before,
        "existing sessions must keep their settings"
    );
    client.close_connection(session, existing).await.unwrap();
    assert_eq!(fresh_login("defaults_target", "SHOW work_mem"), "1MB");
    apply(
        client,
        session,
        root,
        set(Scope::CurrentDatabase, Setting::WorkMem, "2048"),
    )
    .await;
    assert_eq!(fresh_login("defaults_target", "SHOW work_mem"), "2MB");
    let before = client
        .execute(session, root, "SHOW work_mem")
        .await
        .unwrap()
        .rows[0]
        .values
        .clone();
    client
        .execute(session, root, "SET ROLE defaults_target")
        .await
        .unwrap();
    assert_eq!(
        client
            .execute(session, root, "SHOW work_mem")
            .await
            .unwrap()
            .rows[0]
            .values,
        before,
        "SET ROLE must not load login defaults"
    );
    client.execute(session, root, "RESET ROLE").await.unwrap();
    let own = Action::SetRoleDefault {
        name: "members_plain".into(),
        scope: Scope::CurrentDatabase,
        setting: Setting::LockTimeout,
        value: "13".into(),
    };
    apply(client, session, plain, own).await;
    assert_eq!(fresh_login("members_plain", "SHOW lock_timeout"), "13ms");
    apply(
        client,
        session,
        plain,
        Action::ResetRoleDefault {
            name: "members_plain".into(),
            scope: Scope::CurrentDatabase,
            setting: Setting::LockTimeout,
        },
    )
    .await;
    let catalog = client
        .list_postgres_roles(
            session,
            root,
            PostgresObjectPageQuery {
                limit: Some(200),
                offset: 0,
            },
        )
        .await
        .unwrap();
    let defaults = &catalog
        .items
        .iter()
        .find(|r| r.name == "defaults_target")
        .unwrap()
        .details
        .as_ref()
        .unwrap()
        .defaults;
    assert_eq!(defaults.len(), 2);
    assert!(defaults
        .iter()
        .any(|d| d.scope == Scope::CurrentDatabase && d.value == "2048kB"));
    assert!(!serde_json::to_string(&catalog)
        .unwrap()
        .contains("PRIVATE-DEFAULT-SENTINEL"));
    let stale = client
        .preview_postgres_object(
            session,
            root,
            set(Scope::CurrentDatabase, Setting::WorkMem, "4096"),
        )
        .await
        .unwrap();
    cluster.sql("ALTER ROLE defaults_target IN DATABASE postgres SET work_mem='3MB'");
    assert!(client
        .apply_postgres_object(session, root, confirmed(stale))
        .await
        .is_err());
    apply(
        client,
        session,
        root,
        reset(Scope::CurrentDatabase, Setting::WorkMem),
    )
    .await;
    assert_eq!(fresh_login("defaults_target", "SHOW work_mem"), "1MB");
    let stale = client
        .preview_postgres_object(
            session,
            admin,
            set(Scope::AllDatabases, Setting::Jit, "off"),
        )
        .await
        .unwrap();
    cluster.sql("REVOKE ADMIN OPTION FOR defaults_target FROM defaults_admin GRANTED BY sift");
    assert!(client
        .apply_postgres_object(session, admin, confirmed(stale))
        .await
        .is_err());
    for (setting, value) in [
        (Setting::MaintenanceWorkMem, "1024"),
        (Setting::StatementTimeout, "11000"),
        (Setting::LockTimeout, "17"),
        (Setting::IdleInTransactionSessionTimeout, "22000"),
        (Setting::Jit, "off"),
        (Setting::DefaultTransactionReadOnly, "on"),
        (Setting::MaxParallelWorkersPerGather, "1"),
    ] {
        let preview = client
            .preview_postgres_object(session, root, set(Scope::AllDatabases, setting, value))
            .await
            .unwrap();
        assert!(!serde_json::to_string(&preview)
            .unwrap()
            .contains("PRIVATE-DEFAULT-SENTINEL"));
        let mut unconfirmed = confirmed(preview.clone());
        unconfirmed.production_confirmed = false;
        assert!(client
            .apply_postgres_object(session, root, unconfirmed)
            .await
            .is_err());
        client
            .apply_postgres_object(session, root, confirmed(preview))
            .await
            .unwrap();
        apply(client, session, root, reset(Scope::AllDatabases, setting)).await;
    }
    let stale = client
        .preview_postgres_object(session, root, set(Scope::AllDatabases, Setting::Jit, "off"))
        .await
        .unwrap();
    client
        .execute(session, root, "SET ROLE defaults_target")
        .await
        .unwrap();
    assert!(
        client
            .apply_postgres_object(session, root, confirmed(stale.clone()))
            .await
            .is_err(),
        "an independently authorized changed actor must invalidate review"
    );
    client.execute(session, root, "RESET ROLE").await.unwrap();
    cluster.sql("DROP ROLE defaults_target; CREATE ROLE defaults_target LOGIN");
    assert!(client
        .apply_postgres_object(session, root, confirmed(stale))
        .await
        .is_err());
    assert!(client
        .preview_postgres_object(session, root, reset(Scope::AllDatabases, Setting::Jit))
        .await
        .is_err());
    client.close_connection(session, admin).await.unwrap();
    cluster.sql("DROP ROLE defaults_target; DROP ROLE defaults_admin");
}

async fn role_lifecycle(
    client: &Client,
    session: SessionId,
    root: ConnectionId,
    plain: ConnectionId,
    cluster: &Cluster,
) {
    use sift_protocol::PostgresRoleAttributePatch as Patch;
    let create = Action::CreateRoleWithAttributes {
        name: "life_target".into(),
        attributes: Patch {
            login: Some(true),
            connection_limit: Some(3),
            valid_until: Some(sift_protocol::PostgresRoleValidity::Until {
                at: "2030-01-01T00:00:00Z".parse().unwrap(),
            }),
            ..Default::default()
        },
    };
    assert!(client
        .preview_postgres_object(session, plain, create.clone())
        .await
        .is_err());
    apply(client, session, root, create).await;
    assert!(
        client
            .preview_postgres_object(
                session,
                root,
                Action::AlterRoleAttributes {
                    name: "life_target".into(),
                    attributes: Patch {
                        valid_until: Some(sift_protocol::PostgresRoleValidity::Until {
                            at: "2030-01-01T00:00:00Z".parse().unwrap()
                        }),
                        ..Default::default()
                    }
                }
            )
            .await
            .is_err(),
        "normalized timestamp no-op must refuse"
    );
    let alter = Action::AlterRoleAttributes {
        name: "life_target".into(),
        attributes: Patch {
            inherit: Some(false),
            create_database: Some(true),
            ..Default::default()
        },
    };
    apply(client, session, root, alter.clone()).await;
    let roles = client
        .list_postgres_roles(
            session,
            root,
            PostgresObjectPageQuery {
                limit: Some(200),
                offset: 0,
            },
        )
        .await
        .unwrap();
    let role = roles
        .items
        .iter()
        .find(|r| r.name == "life_target")
        .unwrap();
    assert!(role.can_login);
    let details = role.details.as_ref().unwrap();
    assert_eq!(details.connection_limit, 3);
    assert!(!details.inherit && details.create_database);
    assert!(
        client
            .preview_postgres_object(session, root, alter)
            .await
            .is_err(),
        "no-op patch must refuse"
    );
    cluster.sql("CREATE ROLE life_manager LOGIN CREATEROLE; CREATE ROLE life_unmanaged; GRANT life_target TO life_manager WITH ADMIN TRUE");
    let manager = open(client, session, cluster, "life_manager").await;
    apply(
        client,
        session,
        manager,
        Action::CreateRoleWithAttributes {
            name: "life_created".into(),
            attributes: Patch {
                superuser: Some(false),
                ..Default::default()
            },
        },
    )
    .await;
    apply(
        client,
        session,
        manager,
        Action::DropRole {
            name: "life_created".into(),
        },
    )
    .await;
    apply(
        client,
        session,
        root,
        Action::CreateRoleWithAttributes {
            name: "life_privileged".into(),
            attributes: Patch {
                superuser: Some(true),
                replication: Some(true),
                bypass_rls: Some(true),
                create_role: Some(true),
                ..Default::default()
            },
        },
    )
    .await;
    apply(
        client,
        session,
        root,
        Action::DropRole {
            name: "life_privileged".into(),
        },
    )
    .await;
    let update = Action::AlterRoleAttributes {
        name: "life_target".into(),
        attributes: Patch {
            login: Some(false),
            ..Default::default()
        },
    };
    apply(client, session, manager, update).await;
    assert!(client
        .preview_postgres_object(
            session,
            manager,
            Action::AlterRoleAttributes {
                name: "life_unmanaged".into(),
                attributes: Patch {
                    login: Some(true),
                    ..Default::default()
                }
            }
        )
        .await
        .is_err());
    for patch in [
        Patch {
            superuser: Some(true),
            ..Default::default()
        },
        Patch {
            replication: Some(true),
            ..Default::default()
        },
        Patch {
            bypass_rls: Some(true),
            ..Default::default()
        },
        Patch {
            create_database: Some(false),
            ..Default::default()
        },
    ] {
        assert!(
            client
                .preview_postgres_object(
                    session,
                    manager,
                    Action::AlterRoleAttributes {
                        name: "life_target".into(),
                        attributes: patch
                    }
                )
                .await
                .is_err(),
            "privileged flag must use native authority"
        );
    }
    let rename = Action::RenameRole {
        name: "life_target".into(),
        new_name: "life_renamed".into(),
    };
    let stale = client
        .preview_postgres_object(session, root, rename.clone())
        .await
        .unwrap();
    assert!(stale.warning.contains("MD5"));
    cluster.sql("ALTER ROLE life_target CONNECTION LIMIT 4");
    assert!(client
        .apply_postgres_object(session, root, confirmed(stale))
        .await
        .is_err());
    apply(client, session, manager, rename).await;
    let drop = Action::DropRole {
        name: "life_renamed".into(),
    };
    let stale = client
        .preview_postgres_object(session, root, drop.clone())
        .await
        .unwrap();
    cluster.sql("CREATE SCHEMA life_owned AUTHORIZATION life_renamed");
    assert!(
        client
            .apply_postgres_object(session, root, confirmed(stale))
            .await
            .is_err(),
        "changed dependency snapshot must invalidate review"
    );
    let blocked = client
        .preview_postgres_object(session, root, drop.clone())
        .await
        .unwrap();
    assert!(blocked
        .impact
        .as_ref()
        .unwrap()
        .objects
        .iter()
        .any(|d| d.contains("Shared dependency")));
    assert!(
        client
            .apply_postgres_object(session, root, confirmed(blocked))
            .await
            .is_err(),
        "native dependencies must block drop"
    );
    cluster.sql("DROP SCHEMA life_owned");
    apply(client, session, manager, drop).await;
    let stale = client
        .preview_postgres_object(
            session,
            root,
            Action::DropRole {
                name: "life_unmanaged".into(),
            },
        )
        .await
        .unwrap();
    cluster.sql("DROP ROLE life_unmanaged; CREATE ROLE life_unmanaged");
    assert!(
        client
            .apply_postgres_object(session, root, confirmed(stale))
            .await
            .is_err(),
        "recreated role identity must invalidate review"
    );
    apply(
        client,
        session,
        root,
        Action::DropRole {
            name: "life_unmanaged".into(),
        },
    )
    .await;
    client.close_connection(session, manager).await.unwrap();
    cluster.sql("DROP ROLE life_manager");
}

async fn privilege_matrix(
    client: &Client,
    session: SessionId,
    root: ConnectionId,
    plain: ConnectionId,
    cluster: &Cluster,
    sessions: &SessionStore,
) {
    use sift_protocol::{PostgresPrivilegeObjectKind as Kind, PostgresPrivilegeQuery as Query};
    cluster.sql(r#"
CREATE ROLE matrix_group;
CREATE ROLE "matrix & subject" LOGIN;
CREATE ROLE matrix_noninherit LOGIN;
GRANT matrix_group TO "matrix & subject" WITH INHERIT TRUE, SET TRUE;
GRANT matrix_group TO matrix_noninherit WITH INHERIT FALSE, SET TRUE;
GRANT "matrix & subject",matrix_noninherit TO members_plain WITH INHERIT FALSE, SET TRUE;
CREATE SCHEMA matrix_visible;
GRANT USAGE ON SCHEMA matrix_visible TO matrix_group;
CREATE TABLE matrix_visible.t(a integer,b integer);
INSERT INTO matrix_visible.t VALUES (1,2);
ALTER TABLE matrix_visible.t ENABLE ROW LEVEL SECURITY;
ALTER TABLE matrix_visible.t FORCE ROW LEVEL SECURITY;
GRANT SELECT(a) ON matrix_visible.t TO "matrix & subject";
GRANT UPDATE ON matrix_visible.t TO matrix_group WITH GRANT OPTION;
SET ROLE matrix_group;
GRANT UPDATE ON matrix_visible.t TO "matrix & subject";
RESET ROLE;
CREATE TABLE matrix_visible.default_table(a integer);
CREATE FUNCTION matrix_visible.default_exec() RETURNS integer LANGUAGE sql AS 'SELECT 1';
CREATE TABLE matrix_visible.bounded(a integer);
DO $fixture$ BEGIN
 FOR i IN 1..12 LOOP
  EXECUTE format('CREATE ROLE matrix_acl_%s',i);
  EXECUTE format('GRANT ALL ON matrix_visible.bounded TO matrix_acl_%s',i);
 END LOOP;
END $fixture$;
CREATE TABLE matrix_visible.public_table(a integer);
GRANT SELECT ON matrix_visible.public_table TO PUBLIC;
CREATE TABLE matrix_visible.owner_table(a integer);
ALTER TABLE matrix_visible.owner_table OWNER TO "matrix & subject";
REVOKE ALL ON matrix_visible.owner_table FROM "matrix & subject";
CREATE SEQUENCE matrix_visible.seq;
GRANT USAGE ON SEQUENCE matrix_visible.seq TO matrix_group;
CREATE FUNCTION matrix_visible.overloaded(integer) RETURNS integer LANGUAGE sql AS 'SELECT $1';
CREATE FUNCTION matrix_visible.overloaded(text) RETURNS text LANGUAGE sql AS 'SELECT $1';
REVOKE ALL ON FUNCTION matrix_visible.overloaded(integer),matrix_visible.overloaded(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION matrix_visible.overloaded(integer) TO matrix_group;
"#);
    let query = |role: &str| Query {
        role: Some(role.into()),
        ..Default::default()
    };
    let report = client
        .postgres_privileges(session, root, query("matrix & subject"))
        .await
        .unwrap();
    assert_eq!(report.actor, "sift");
    assert_eq!(report.role, "matrix & subject");
    assert!(!report.role_superuser && !report.role_bypass_rls);
    assert!(report.warning.contains("not a stable snapshot"));
    let object = |kind, name: &str| {
        report
            .objects
            .iter()
            .find(|o| {
                o.kind == kind
                    && o.name == name
                    && o.schema.as_deref() == Some("matrix_visible")
                    && (kind != Kind::Column || o.relation.as_deref() == Some("t"))
            })
            .unwrap()
    };
    let check = |object: &sift_protocol::PostgresPrivilegeObject, privilege: &str| {
        let c = object
            .checks
            .iter()
            .find(|c| c.privilege == privilege)
            .unwrap();
        (c.allowed, c.grantable)
    };
    assert_eq!(
        check(object(Kind::Table, "t"), "UPDATE"),
        (true, true),
        "inherited table grant and grant option"
    );
    assert_eq!(
        check(object(Kind::Table, "t"), "SELECT"),
        (false, false),
        "column SELECT must not become table SELECT"
    );
    assert_eq!(check(object(Kind::Column, "a"), "SELECT"), (true, false));
    assert_eq!(check(object(Kind::Column, "b"), "SELECT"), (false, false));
    assert!(
        object(Kind::Table, "t").row_security_enabled
            && object(Kind::Table, "t").row_security_forced
    );
    assert_eq!(
        check(object(Kind::Table, "owner_table"), "SELECT"),
        (false, true),
        "owner may revoke ordinary access while retaining grant options"
    );
    assert_eq!(check(object(Kind::Sequence, "seq"), "USAGE"), (true, false));
    assert!(object(Kind::Table, "t")
        .checks
        .iter()
        .any(|c| c.privilege == "MAINTAIN"));
    let routines = report
        .objects
        .iter()
        .filter(|o| o.kind == Kind::Routine && o.name == "overloaded")
        .collect::<Vec<_>>();
    assert_eq!(routines.len(), 2);
    assert_ne!(routines[0].object_oid, routines[1].object_oid);
    assert_eq!(routines.iter().filter(|o| check(o, "EXECUTE").0).count(), 1);
    let database = report
        .objects
        .iter()
        .find(|o| o.kind == Kind::Database)
        .unwrap()
        .acl
        .as_ref()
        .unwrap();
    assert!(
        database.defaulted
            && database
                .grants
                .iter()
                .any(|g| g.grantee_oid == 0 && g.privilege == "CONNECT" && g.applies_to_subject)
    );
    for (kind, name, privilege) in [
        (Kind::Schema, "matrix_visible", "USAGE"),
        (Kind::Sequence, "seq", "USAGE"),
    ] {
        let acl = object(kind, name).acl.as_ref().unwrap();
        assert!(
            !acl.defaulted
                && acl
                    .grants
                    .iter()
                    .any(|g| g.grantee.as_deref() == Some("matrix_group")
                        && g.privilege == privilege
                        && g.applies_to_subject)
        );
    }
    assert!(routines.iter().filter(|o| check(o, "EXECUTE").0).all(|o| o
        .acl
        .as_ref()
        .unwrap()
        .grants
        .iter()
        .any(|g| g.grantee.as_deref() == Some("matrix_group")
            && g.privilege == "EXECUTE"
            && g.applies_to_subject)));
    let acl = object(Kind::Table, "t").acl.as_ref().unwrap();
    assert!(!acl.defaulted && !acl.truncated);
    assert_eq!(acl.owner.as_deref(), Some("sift"));
    assert!(acl
        .grants
        .iter()
        .any(|g| g.grantor.as_deref() == Some("sift")
            && g.grantee.as_deref() == Some("matrix_group")
            && g.privilege == "UPDATE"
            && g.grantable
            && g.applies_to_subject));
    assert!(acl
        .grants
        .iter()
        .any(|g| g.grantor.as_deref() == Some("matrix_group")
            && g.grantee.as_deref() == Some("matrix & subject")
            && g.privilege == "UPDATE"
            && !g.grantable
            && g.applies_to_subject));
    let column = object(Kind::Column, "a").acl.as_ref().unwrap();
    assert!(
        !column.defaulted
            && column.grants.iter().any(|g| g.privilege == "SELECT"
                && g.grantee.as_deref() == Some("matrix & subject")
                && g.applies_to_subject)
    );
    let column = object(Kind::Column, "b").acl.as_ref().unwrap();
    assert!(
        column.defaulted && column.grants.is_empty(),
        "column ACL must not copy table grants"
    );
    assert_eq!(
        check(object(Kind::Column, "b"), "UPDATE"),
        (true, true),
        "native column checks still inherit table privileges"
    );
    let owner = object(Kind::Table, "owner_table").acl.as_ref().unwrap();
    assert!(
        !owner.defaulted && owner.grants.is_empty(),
        "explicit empty ACL must not expand defaults"
    );
    let defaults = object(Kind::Table, "default_table").acl.as_ref().unwrap();
    assert!(defaults.defaulted && !defaults.grants.is_empty());
    let defaults = object(Kind::Routine, "default_exec").acl.as_ref().unwrap();
    assert!(
        defaults.defaulted
            && defaults.grants.iter().any(|g| g.grantee_oid == 0
                && g.grantee.is_none()
                && g.privilege == "EXECUTE"
                && g.applies_to_subject)
    );
    let public_acl = object(Kind::Table, "public_table").acl.as_ref().unwrap();
    assert!(
        !public_acl.defaulted
            && public_acl
                .grants
                .iter()
                .any(|g| g.grantee_oid == 0 && g.privilege == "SELECT" && g.applies_to_subject)
    );
    let bounded = object(Kind::Table, "bounded").acl.as_ref().unwrap();
    assert!(
        bounded.truncated && bounded.grants.len() == 64,
        "large ACL provenance must be bounded and disclosed"
    );
    assert!(report
        .objects
        .iter()
        .all(|o| o.acl.as_ref().is_some_and(|a| a.grants.len() <= 64)));
    let noninherit = client
        .postgres_privileges(session, root, query("matrix_noninherit"))
        .await
        .unwrap();
    let t = noninherit
        .objects
        .iter()
        .find(|o| o.kind == Kind::Table && o.name == "t")
        .unwrap();
    assert_eq!(
        check(t, "UPDATE"),
        (false, false),
        "SET membership alone is not effective inherited access"
    );
    assert!(
        t.acl
            .as_ref()
            .unwrap()
            .grants
            .iter()
            .filter(|g| g.grantee.as_deref() == Some("matrix_group"))
            .all(|g| !g.applies_to_subject),
        "SET-only membership must not be labeled inherited grantee access"
    );
    let public = noninherit
        .objects
        .iter()
        .find(|o| o.kind == Kind::Table && o.name == "public_table")
        .unwrap();
    assert_eq!(check(public, "SELECT"), (true, false));
    let hidden = client
        .postgres_privileges(session, plain, query("matrix & subject"))
        .await
        .unwrap();
    assert!(
        hidden
            .objects
            .iter()
            .all(|o| o.schema.as_deref() != Some("matrix_visible")),
        "subject access must not broaden connected actor visibility"
    );
    let mut offset = 0;
    let mut keys = std::collections::HashSet::new();
    loop {
        let page = client
            .postgres_privileges(
                session,
                root,
                Query {
                    role: Some("matrix & subject".into()),
                    offset,
                    limit: Some(2),
                },
            )
            .await
            .unwrap();
        assert!(page.objects.len() <= 2);
        for o in page.objects {
            assert!(keys.insert((format!("{:?}", o.kind), o.object_oid, o.column_number)));
        }
        match page.next_offset {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(keys.len(), report.objects.len());
    assert!(client
        .postgres_privileges(session, root, query("no such role"))
        .await
        .is_err());
    client
        .execute(session, plain, r#"SET ROLE "matrix & subject""#)
        .await
        .unwrap();
    let subject = plain;
    assert!(
        client
            .execute(session, subject, "SELECT a FROM matrix_visible.t")
            .await
            .unwrap()
            .rows
            .is_empty(),
        "ACL SELECT does not bypass RLS"
    );
    client.execute(session, plain, "RESET ROLE").await.unwrap();
    let member = plain;
    client
        .execute(session, member, "SET ROLE matrix_group")
        .await
        .unwrap();
    let current = client
        .postgres_privileges(session, member, Query::default())
        .await
        .unwrap();
    assert_eq!(current.actor, "matrix_group");
    assert_eq!(current.role, "matrix_group");
    assert_eq!(
        client
            .execute(session, member, "SELECT current_user::text")
            .await
            .unwrap()
            .rows[0]
            .values[0],
        Value::Text("matrix_group".into()),
        "inspection preserves current actor"
    );
    client.execute(session, member, "RESET ROLE").await.unwrap();
    cluster.sql("REVOKE UPDATE ON matrix_visible.t FROM matrix_group CASCADE");
    let revoked = client
        .postgres_privileges(session, root, query("matrix & subject"))
        .await
        .unwrap();
    let t = revoked
        .objects
        .iter()
        .find(|o| o.kind == Kind::Table && o.name == "t")
        .unwrap();
    assert_eq!(check(t, "UPDATE"), (false, false));
    assert!(
        t.acl
            .as_ref()
            .unwrap()
            .grants
            .iter()
            .all(|g| g.privilege != "UPDATE" || !g.applies_to_subject),
        "revoked delegated edges must disappear from provenance"
    );
    assert!(sessions.list_operations().iter().any(|e| matches!(
        e.operation,
        sift_protocol::Operation::ReadPostgresPrivileges { .. }
    ) && e.status
        == sift_protocol::OperationStatus::Succeeded));
}
