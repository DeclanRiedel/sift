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
    role_lifecycle(&client, session, root, plain, &cluster).await;
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
