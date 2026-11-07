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
    object_privilege_editor(&client, session, root, plain, &cluster, &sessions).await;
    default_privileges(&client, session, root, plain, &cluster).await;
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

async fn object_privilege_editor(
    client: &Client,
    session: SessionId,
    root: ConnectionId,
    plain: ConnectionId,
    cluster: &Cluster,
    sessions: &SessionStore,
) {
    use sift_protocol::{
        PostgresObjectPrivilege as P, PostgresPrivilegeChange as C, PostgresPrivilegeGrantee as G,
        PostgresPrivilegeObjectKind as K, PostgresPrivilegeTarget as T,
    };
    cluster.sql(
        r#"
CREATE ROLE edit_actor; CREATE ROLE edit_subject; CREATE ROLE edit_peer; CREATE ROLE edit_other; CREATE ROLE "PUBLIC";
CREATE ROLE "edit $1 $5 ' \ $native$";
GRANT edit_actor TO members_plain WITH INHERIT FALSE, SET TRUE;
CREATE SCHEMA edit_scope;
GRANT USAGE ON SCHEMA edit_scope TO edit_actor,edit_subject,edit_peer,edit_other;
CREATE TABLE edit_scope.items(a integer,b integer);
CREATE SEQUENCE edit_scope.counter;
ALTER SEQUENCE edit_scope.counter OWNER TO edit_other;
CREATE FUNCTION edit_scope.f(integer) RETURNS integer LANGUAGE sql AS 'SELECT $1';
CREATE FUNCTION edit_scope.f(text) RETURNS text LANGUAGE sql AS 'SELECT $1';
CREATE PROCEDURE edit_scope.p() LANGUAGE plpgsql AS 'BEGIN NULL; END';
REVOKE EXECUTE ON ALL ROUTINES IN SCHEMA edit_scope FROM PUBLIC;
"#,
    );
    let report = client
        .postgres_privileges(
            session,
            root,
            sift_protocol::PostgresPrivilegeQuery {
                role: Some("edit_subject".into()),
                limit: Some(200),
                offset: 0,
            },
        )
        .await
        .unwrap();
    let target = |kind, name: &str| {
        let object = report
            .objects
            .iter()
            .find(|o| {
                o.kind == kind
                    && o.name == name
                    && (kind == K::Database || o.schema.as_deref() == Some("edit_scope"))
            })
            .unwrap();
        T {
            kind,
            object_oid: object.object_oid,
            column_number: object.column_number,
        }
    };
    let table = target(K::Table, "items");
    let routine = T {
        kind: K::Routine,
        object_oid: cluster
            .sql("SELECT 'edit_scope.f(integer)'::regprocedure::oid")
            .parse()
            .unwrap(),
        column_number: None,
    };
    let role = |name: &str| G::Role { name: name.into() };
    let action = |target: T, grantee: G, privilege: P, change: C| Action::ChangeObjectPrivilege {
        target,
        grantee,
        privilege,
        change,
    };
    for (target, privilege) in [
        (target(K::Database, "postgres"), P::Create),
        (target(K::Schema, "edit_scope"), P::Create),
        (table.clone(), P::Select),
        (table.clone(), P::Maintain),
        (target(K::Column, "a"), P::Update),
        (target(K::Sequence, "counter"), P::Usage),
        (routine.clone(), P::Execute),
        (target(K::Routine, "p"), P::Execute),
    ] {
        let grant = action(
            target.clone(),
            role("edit_subject"),
            privilege,
            C::Grant {
                grant_option: false,
            },
        );
        let preview = client
            .preview_postgres_object(session, root, grant.clone())
            .await
            .unwrap();
        assert!(preview.sql.starts_with("GRANT ") && !preview.sql.starts_with("DO "));
        assert!(preview.warning.contains("Native database"));
        if target.kind == K::Database {
            let mut unconfirmed = confirmed(preview.clone());
            unconfirmed.production_confirmed = false;
            assert!(client
                .apply_postgres_object(session, root, unconfirmed)
                .await
                .is_err());
        }
        client
            .apply_postgres_object(session, root, confirmed(preview))
            .await
            .unwrap();
        assert!(
            client
                .preview_postgres_object(session, root, grant)
                .await
                .is_err(),
            "an existing native edge must be an explicit no-op"
        );
        apply(
            client,
            session,
            root,
            action(
                target.clone(),
                role("edit_subject"),
                privilege,
                C::Grant { grant_option: true },
            ),
        )
        .await;
        apply(
            client,
            session,
            root,
            action(
                target.clone(),
                role("edit_subject"),
                privilege,
                C::Revoke {
                    grant_option_only: true,
                },
            ),
        )
        .await;
        apply(
            client,
            session,
            root,
            action(
                target,
                role("edit_subject"),
                privilege,
                C::Revoke {
                    grant_option_only: false,
                },
            ),
        )
        .await;
    }
    assert_eq!(
        cluster.sql(
            "SELECT has_function_privilege('edit_subject','edit_scope.f(text)','EXECUTE')::text"
        ),
        "false",
        "routine overloads must stay separate"
    );
    apply(
        client,
        session,
        root,
        action(
            table.clone(),
            role("PUBLIC"),
            P::Select,
            C::Grant { grant_option: true },
        ),
    )
    .await;
    assert_eq!(
        cluster.sql("SELECT has_table_privilege('edit_subject','edit_scope.items','SELECT')::text"),
        "false",
        "a literal role named PUBLIC must not become the all-role grantee"
    );
    assert_eq!(cluster.sql("SELECT count(*) FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) e JOIN pg_roles r ON r.oid=e.grantee WHERE c.oid='edit_scope.items'::regclass AND r.rolname='PUBLIC' AND e.privilege_type='SELECT' AND e.is_grantable"),"1");
    apply(
        client,
        session,
        root,
        action(
            table.clone(),
            role("PUBLIC"),
            P::Select,
            C::Revoke {
                grant_option_only: false,
            },
        ),
    )
    .await;
    let grant_subject = || {
        action(
            table.clone(),
            role("edit_subject"),
            P::Select,
            C::Grant {
                grant_option: false,
            },
        )
    };
    assert!(
        client
            .preview_postgres_object(session, plain, grant_subject())
            .await
            .is_err(),
        "native authority is required"
    );
    assert!(client
        .preview_postgres_object(
            session,
            root,
            action(
                table.clone(),
                G::Public {},
                P::Select,
                C::Grant { grant_option: true }
            )
        )
        .await
        .is_err());
    apply(
        client,
        session,
        root,
        action(
            table.clone(),
            G::Public {},
            P::Select,
            C::Grant {
                grant_option: false,
            },
        ),
    )
    .await;
    assert_eq!(
        cluster.sql("SELECT has_table_privilege('edit_subject','edit_scope.items','SELECT')::text"),
        "true"
    );
    apply(
        client,
        session,
        root,
        action(
            table.clone(),
            G::Public {},
            P::Select,
            C::Revoke {
                grant_option_only: false,
            },
        ),
    )
    .await;
    apply(
        client,
        session,
        root,
        action(
            target(K::Column, "a"),
            role("edit_subject"),
            P::Select,
            C::Grant { grant_option: true },
        ),
    )
    .await;
    assert_eq!(
        cluster.sql("SELECT has_table_privilege('edit_subject','edit_scope.items','SELECT')::text"),
        "false"
    );
    apply(
        client,
        session,
        root,
        action(
            table.clone(),
            role("edit_subject"),
            P::Select,
            C::Revoke {
                grant_option_only: false,
            },
        ),
    )
    .await;
    assert_eq!(
        cluster.sql(
            "SELECT has_column_privilege('edit_subject','edit_scope.items','a','SELECT')::text"
        ),
        "false",
        "table revoke removes corresponding column grants"
    );
    client
        .execute(session, root, "SET standard_conforming_strings=off")
        .await
        .unwrap();
    let quoted = r#"edit $1 $5 ' \ $native$"#;
    apply(
        client,
        session,
        root,
        action(
            table.clone(),
            role(quoted),
            P::Select,
            C::Grant {
                grant_option: false,
            },
        ),
    )
    .await;
    assert_eq!(cluster.sql(r#"SELECT has_table_privilege(E'edit $1 $5 '' \\ $native$','edit_scope.items','SELECT')::text"#),"true");
    apply(
        client,
        session,
        root,
        action(
            table.clone(),
            role(quoted),
            P::Select,
            C::Revoke {
                grant_option_only: false,
            },
        ),
    )
    .await;
    client
        .execute(session, root, "SET standard_conforming_strings=on")
        .await
        .unwrap();
    cluster.sql("GRANT SELECT ON edit_scope.items TO edit_actor WITH GRANT OPTION");
    client
        .execute(session, plain, "SET ROLE edit_actor")
        .await
        .unwrap();
    let delegated = client
        .preview_postgres_object(session, plain, grant_subject())
        .await
        .unwrap();
    assert!(delegated.warning.contains("edit_actor"));
    assert!(
        client
            .apply_postgres_object(session, root, confirmed(delegated.clone()))
            .await
            .is_err(),
        "review cannot move to another native connection/grantor"
    );
    client
        .apply_postgres_object(session, plain, confirmed(delegated))
        .await
        .unwrap();
    assert_eq!(cluster.sql("SELECT g.rolname FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) e JOIN pg_roles g ON g.oid=e.grantor JOIN pg_roles r ON r.oid=e.grantee WHERE c.oid='edit_scope.items'::regclass AND r.rolname='edit_subject' AND e.privilege_type='SELECT'"),"edit_actor");
    assert!(
        client
            .preview_postgres_object(
                session,
                root,
                action(
                    table.clone(),
                    role("edit_subject"),
                    P::Select,
                    C::Revoke {
                        grant_option_only: false
                    }
                )
            )
            .await
            .is_err(),
        "owner cannot misrepresent another grantor's edge as its own"
    );
    apply(
        client,
        session,
        plain,
        action(
            table.clone(),
            role("edit_subject"),
            P::Select,
            C::Revoke {
                grant_option_only: false,
            },
        ),
    )
    .await;
    client.execute(session, plain, "RESET ROLE").await.unwrap();
    apply(
        client,
        session,
        root,
        action(
            table.clone(),
            role("edit_subject"),
            P::Select,
            C::Grant { grant_option: true },
        ),
    )
    .await;
    cluster.sql("SET ROLE edit_subject; GRANT SELECT ON edit_scope.items TO edit_peer; RESET ROLE");
    let revoke = client
        .preview_postgres_object(
            session,
            root,
            action(
                table.clone(),
                role("edit_subject"),
                P::Select,
                C::Revoke {
                    grant_option_only: true,
                },
            ),
        )
        .await
        .unwrap();
    assert!(
        client
            .apply_postgres_object(session, root, confirmed(revoke))
            .await
            .is_err(),
        "RESTRICT must preserve dependent grants"
    );
    assert_eq!(cluster.sql("SELECT has_table_privilege('edit_subject','edit_scope.items','SELECT WITH GRANT OPTION')::text"),"true");
    assert_eq!(
        cluster.sql("SELECT has_table_privilege('edit_peer','edit_scope.items','SELECT')::text"),
        "true"
    );
    cluster.sql("REVOKE SELECT ON edit_scope.items FROM edit_subject CASCADE; GRANT SELECT ON edit_scope.items TO edit_other WITH GRANT OPTION; GRANT edit_other TO edit_actor WITH INHERIT TRUE, SET TRUE");
    client
        .execute(session, plain, "SET ROLE edit_actor")
        .await
        .unwrap();
    let error = client
        .preview_postgres_object(session, plain, grant_subject())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("ambiguous"),
        "multiple inherited grantors require explicit native role selection"
    );
    client
        .execute(session, plain, "SET ROLE edit_other")
        .await
        .unwrap();
    apply(client, session, plain, grant_subject()).await;
    apply(
        client,
        session,
        plain,
        action(
            table.clone(),
            role("edit_subject"),
            P::Select,
            C::Revoke {
                grant_option_only: false,
            },
        ),
    )
    .await;
    assert_eq!(
        client
            .execute(session, plain, "SELECT current_user::text")
            .await
            .unwrap()
            .rows[0]
            .values[0],
        Value::Text("edit_other".into()),
        "editor must preserve SET ROLE context"
    );
    client.execute(session, plain, "RESET ROLE").await.unwrap();
    let stale = client
        .preview_postgres_object(session, root, grant_subject())
        .await
        .unwrap();
    cluster.sql("GRANT INSERT ON edit_scope.items TO edit_peer");
    assert!(
        client
            .apply_postgres_object(session, root, confirmed(stale))
            .await
            .is_err(),
        "unrelated ACL edits invalidate review"
    );
    cluster.sql("REVOKE INSERT ON edit_scope.items FROM edit_peer");
    let stale = client
        .preview_postgres_object(session, root, grant_subject())
        .await
        .unwrap();
    cluster.sql("ALTER ROLE edit_subject RENAME TO edit_subject_old; CREATE ROLE edit_subject");
    assert!(
        client
            .apply_postgres_object(session, root, confirmed(stale))
            .await
            .is_err(),
        "same-name role replacement invalidates native identity"
    );
    assert_eq!(
        cluster.sql("SELECT has_table_privilege('edit_subject','edit_scope.items','SELECT')::text"),
        "false"
    );
    cluster.sql("DROP ROLE edit_subject; ALTER ROLE edit_subject_old RENAME TO edit_subject");
    let stale = client
        .preview_postgres_object(session, root, grant_subject())
        .await
        .unwrap();
    client
        .execute(session, root, "SET ROLE edit_other")
        .await
        .unwrap();
    assert!(
        client
            .apply_postgres_object(session, root, confirmed(stale))
            .await
            .is_err(),
        "actor changes invalidate review even with native authority"
    );
    client.execute(session, root, "RESET ROLE").await.unwrap();
    cluster.sql("CREATE TABLE edit_scope.replaced(a integer)");
    let replaced = T {
        kind: K::Table,
        object_oid: cluster
            .sql("SELECT 'edit_scope.replaced'::regclass::oid")
            .parse()
            .unwrap(),
        column_number: None,
    };
    let stale = client
        .preview_postgres_object(
            session,
            root,
            action(
                replaced,
                role("edit_subject"),
                P::Select,
                C::Grant {
                    grant_option: false,
                },
            ),
        )
        .await
        .unwrap();
    cluster.sql("DROP TABLE edit_scope.replaced; CREATE TABLE edit_scope.replaced(a integer)");
    assert!(client
        .apply_postgres_object(session, root, confirmed(stale))
        .await
        .is_err());
    assert_eq!(
        cluster
            .sql("SELECT has_table_privilege('edit_subject','edit_scope.replaced','SELECT')::text"),
        "false"
    );
    cluster.sql(r#"
CREATE FUNCTION edit_scope.guard_events() RETURNS event_trigger LANGUAGE plpgsql AS $fixture$
BEGIN
 IF current_setting('sift.object_privilege_test',true)='extra' AND TG_EVENT='ddl_command_end' THEN
  PERFORM set_config('sift.object_privilege_test','spent',true);
  GRANT UPDATE ON edit_scope.items TO edit_peer;
 ELSIF current_setting('sift.object_privilege_test',true)='replace' AND TG_EVENT='ddl_command_start' THEN
  PERFORM set_config('sift.object_privilege_test','spent',true);
  ALTER TABLE edit_scope.items RENAME TO items_prior;
  CREATE TABLE edit_scope.items(a integer,b integer);
 END IF;
END $fixture$;
CREATE EVENT TRIGGER edit_guard_start ON ddl_command_start WHEN TAG IN ('GRANT') EXECUTE FUNCTION edit_scope.guard_events();
CREATE EVENT TRIGGER edit_guard_end ON ddl_command_end WHEN TAG IN ('GRANT') EXECUTE FUNCTION edit_scope.guard_events();
"#);
    for mode in ["extra", "replace"] {
        let preview = client
            .preview_postgres_object(session, root, grant_subject())
            .await
            .unwrap();
        client
            .execute(
                session,
                root,
                format!("SET sift.object_privilege_test='{mode}'"),
            )
            .await
            .unwrap();
        assert!(
            client
                .apply_postgres_object(session, root, confirmed(preview))
                .await
                .is_err(),
            "native postcondition drift must roll back the entire statement"
        );
        client
            .execute(session, root, "SET sift.object_privilege_test='idle'")
            .await
            .unwrap();
        assert_eq!(
            cluster.sql(
                "SELECT has_table_privilege('edit_subject','edit_scope.items','SELECT')::text"
            ),
            "false"
        );
        assert_eq!(
            cluster
                .sql("SELECT has_table_privilege('edit_peer','edit_scope.items','UPDATE')::text"),
            "false"
        );
        assert_eq!(
            cluster.sql("SELECT 'edit_scope.items'::regclass::oid"),
            table.object_oid.to_string()
        );
        assert_eq!(
            cluster.sql("SELECT to_regclass('edit_scope.items_prior') IS NULL"),
            "t"
        );
    }
    cluster.sql("DROP EVENT TRIGGER edit_guard_start; DROP EVENT TRIGGER edit_guard_end; DROP FUNCTION edit_scope.guard_events()");
    assert!(sessions.list_operations().iter().any(|entry| matches!(
        entry.operation,
        sift_protocol::Operation::ApplyPostgresObject {
            action: Action::ChangeObjectPrivilege { .. },
            ..
        }
    ) && entry.status
        == sift_protocol::OperationStatus::Succeeded));
    assert!(sessions.list_operations().iter().any(|entry| matches!(
        entry.operation,
        sift_protocol::Operation::ApplyPostgresObject {
            action: Action::ChangeObjectPrivilege { .. },
            ..
        }
    ) && entry.status
        == sift_protocol::OperationStatus::Failed));
}

async fn default_privileges(
    client: &Client,
    session: SessionId,
    root: ConnectionId,
    plain: ConnectionId,
    cluster: &Cluster,
) {
    use sift_protocol::{
        PostgresDefaultPrivilegeKind as K, PostgresObjectPrivilege as P,
        PostgresPrivilegeChange as C, PostgresPrivilegeGrantee as G,
    };
    cluster.sql(r#"CREATE ROLE "defaults owner"; CREATE ROLE "defaults recipient"; GRANT CREATE ON DATABASE postgres TO "defaults owner"; CREATE SCHEMA "defaults scope" AUTHORIZATION "defaults owner"; SET ROLE "defaults owner"; CREATE TABLE "defaults scope".before_grant(x int); RESET ROLE"#);
    let action = |scope: Option<&str>, kind, privilege, change| Action::ChangeDefaultPrivilege {
        creator: "defaults owner".into(),
        schema: scope.map(str::to_owned),
        object_kind: kind,
        grantee: G::Role {
            name: "defaults recipient".into(),
        },
        privilege,
        change,
    };
    let grant = C::Grant {
        grant_option: false,
    };
    let revoke = C::Revoke {
        grant_option_only: false,
    };
    let select = action(Some("defaults scope"), K::Tables, P::Select, grant);
    assert!(client
        .preview_postgres_object(session, plain, select.clone())
        .await
        .is_err());
    let preview = client
        .preview_postgres_object(session, root, select.clone())
        .await
        .unwrap();
    assert!(preview.warning.contains("Existing objects are unchanged"));
    assert!(preview
        .impact
        .as_ref()
        .is_some_and(|impact| !impact.truncated && !impact.objects.is_empty()));
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
    assert!(client
        .preview_postgres_object(session, root, select)
        .await
        .is_err());
    apply(
        client,
        session,
        root,
        action(None, K::Tables, P::Update, grant),
    )
    .await;
    assert!(client
        .preview_postgres_object(
            session,
            root,
            action(Some("defaults scope"), K::Tables, P::Update, revoke)
        )
        .await
        .is_err());
    apply(
        client,
        session,
        root,
        action(Some("defaults scope"), K::Tables, P::Update, grant),
    )
    .await;
    apply(
        client,
        session,
        root,
        action(Some("defaults scope"), K::Tables, P::Update, revoke),
    )
    .await;
    apply(
        client,
        session,
        root,
        action(
            Some("defaults scope"),
            K::Tables,
            P::Select,
            C::Grant { grant_option: true },
        ),
    )
    .await;
    apply(
        client,
        session,
        root,
        action(
            Some("defaults scope"),
            K::Tables,
            P::Select,
            C::Revoke {
                grant_option_only: true,
            },
        ),
    )
    .await;
    for (kind, privilege, scope) in [
        (K::Sequences, P::Usage, Some("defaults scope")),
        (K::Routines, P::Execute, None),
        (K::Types, P::Usage, Some("defaults scope")),
        (K::Schemas, P::Create, None),
    ] {
        apply(client, session, root, action(scope, kind, privilege, grant)).await;
    }
    cluster.sql(r#"SET ROLE "defaults owner"; CREATE TABLE "defaults scope".after_grant(x int); CREATE SEQUENCE "defaults scope".seq; CREATE FUNCTION "defaults scope".fn() RETURNS int LANGUAGE sql AS 'SELECT 1'; CREATE TYPE "defaults scope".typ AS ENUM ('a'); CREATE SCHEMA "defaults created"; RESET ROLE"#);
    assert_eq!(cluster.sql(r#"SELECT has_table_privilege('defaults recipient','"defaults scope".before_grant','SELECT'),has_table_privilege('defaults recipient','"defaults scope".after_grant','SELECT'),has_table_privilege('defaults recipient','"defaults scope".after_grant','UPDATE'),has_table_privilege('defaults recipient','"defaults scope".after_grant','SELECT WITH GRANT OPTION'),has_sequence_privilege('defaults recipient','"defaults scope".seq','USAGE'),has_function_privilege('defaults recipient','"defaults scope".fn()','EXECUTE'),has_type_privilege('defaults recipient','"defaults scope".typ','USAGE'),has_schema_privilege('defaults recipient','defaults created','CREATE')"#),"f|t|t|f|t|t|t|t");
    assert_eq!(cluster.sql("SELECT count(*) FROM pg_proc p CROSS JOIN LATERAL aclexplode(p.proacl) a WHERE p.proname='fn' AND a.grantee=(SELECT oid FROM pg_roles WHERE rolname='defaults recipient') AND a.privilege_type='EXECUTE'"),"1");
    assert_eq!(cluster.sql("SELECT count(*) FROM pg_type t CROSS JOIN LATERAL aclexplode(t.typacl) a WHERE t.typname='typ' AND a.grantee=(SELECT oid FROM pg_roles WHERE rolname='defaults recipient') AND a.privilege_type='USAGE'"),"1");
    let stale = client
        .preview_postgres_object(session, root, action(None, K::Tables, P::Delete, grant))
        .await
        .unwrap();
    cluster.sql(r#"ALTER DEFAULT PRIVILEGES FOR ROLE "defaults owner" GRANT TRIGGER ON TABLES TO "defaults recipient""#);
    assert!(client
        .apply_postgres_object(session, root, confirmed(stale))
        .await
        .is_err());
    cluster.sql(r#"GRANT "defaults owner" TO members_plain WITH INHERIT TRUE"#);
    let stale = client
        .preview_postgres_object(session, plain, action(None, K::Tables, P::Insert, grant))
        .await
        .unwrap();
    cluster.sql(r#"REVOKE "defaults owner" FROM members_plain"#);
    assert!(client
        .apply_postgres_object(session, plain, confirmed(stale))
        .await
        .is_err());
    let stale = client
        .preview_postgres_object(session, root, action(None, K::Tables, P::Insert, grant))
        .await
        .unwrap();
    client
        .execute(session, root, "SET ROLE members_plain")
        .await
        .unwrap();
    assert!(client
        .apply_postgres_object(session, root, confirmed(stale))
        .await
        .is_err());
    client.execute(session, root, "RESET ROLE").await.unwrap();
    let quoted = r#"defaults $1 ' \ " scope"#;
    let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    cluster.sql(&format!("CREATE SCHEMA {}", quote(quoted)));
    client
        .execute(session, root, "SET standard_conforming_strings=off")
        .await
        .unwrap();
    apply(
        client,
        session,
        root,
        action(Some(quoted), K::Tables, P::Select, grant),
    )
    .await;
    client
        .execute(session, root, "SET standard_conforming_strings=on")
        .await
        .unwrap();
    let stale = client
        .preview_postgres_object(
            session,
            root,
            action(Some(quoted), K::Tables, P::Insert, grant),
        )
        .await
        .unwrap();
    cluster.sql(&format!(
        "DROP SCHEMA {}; CREATE SCHEMA {}",
        quote(quoted),
        quote(quoted)
    ));
    assert!(client
        .apply_postgres_object(session, root, confirmed(stale))
        .await
        .is_err());
    cluster.sql("CREATE FUNCTION public.current_database() RETURNS name LANGUAGE sql AS $$SELECT 'shadow'::name$$; CREATE FUNCTION public.left(text,integer) RETURNS text LANGUAGE sql AS $$SELECT 'shadow'::text$$; CREATE FUNCTION public.shadow_eq(jsonb,jsonb) RETURNS bool LANGUAGE plpgsql AS $$BEGIN RAISE EXCEPTION 'Untrusted equality operator'; END$$; CREATE OPERATOR public.= (LEFTARG=jsonb, RIGHTARG=jsonb, FUNCTION=public.shadow_eq)");
    client
        .execute(session, root, "SET search_path=public,pg_catalog")
        .await
        .unwrap();
    apply(
        client,
        session,
        root,
        action(None, K::Tables, P::Insert, grant),
    )
    .await;
    client
        .execute(session, root, "RESET search_path")
        .await
        .unwrap();
    cluster.sql("DROP OPERATOR public.= (jsonb,jsonb); DROP FUNCTION public.shadow_eq(jsonb,jsonb); DROP FUNCTION public.current_database(); DROP FUNCTION public.left(text,integer)");
    for (name, prior) in [
        ("defaults recipient", "defaults prior recipient"),
        ("defaults owner", "defaults prior owner"),
    ] {
        cluster.sql("CREATE ROLE defaults_swap");
        let stale = client
            .preview_postgres_object(session, root, action(None, K::Tables, P::Delete, grant))
            .await
            .unwrap();
        cluster.sql(&format!(
            "ALTER ROLE {} RENAME TO {}; ALTER ROLE defaults_swap RENAME TO {}",
            quote(name),
            quote(prior),
            quote(name)
        ));
        assert!(client
            .apply_postgres_object(session, root, confirmed(stale))
            .await
            .is_err());
        cluster.sql(&format!("ALTER ROLE {} RENAME TO defaults_swap; ALTER ROLE {} RENAME TO {}; DROP ROLE defaults_swap",quote(name),quote(prior),quote(name)));
    }
    let public = Action::ChangeDefaultPrivilege {
        creator: "defaults owner".into(),
        schema: Some("defaults scope".into()),
        object_kind: K::Routines,
        grantee: G::Public {},
        privilege: P::Execute,
        change: grant,
    };
    apply(client, session, root, public.clone()).await;
    let mut named = public.clone();
    if let Action::ChangeDefaultPrivilege { grantee, .. } = &mut named {
        *grantee = G::Role {
            name: "PUBLIC".into(),
        };
    }
    apply(client, session, root, named).await;
    let mut public = public;
    if let Action::ChangeDefaultPrivilege { change, .. } = &mut public {
        *change = revoke;
    }
    let preview = client
        .preview_postgres_object(session, root, public)
        .await
        .unwrap();
    assert!(preview
        .impact
        .as_ref()
        .unwrap()
        .objects
        .iter()
        .any(|entry| entry.contains("PUBLIC (all roles)")));
    client
        .apply_postgres_object(session, root, confirmed(preview))
        .await
        .unwrap();
}
