use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    BeginTransactionRequest, Code, ConnectionSpec, DriverError, EndTransactionRequest, Engine,
    ExecuteRequestHttp, OpenSessionRequest, SslMode, TransactionCondition, TxHandleRef, TxMode,
};
use sift_server::{DriverRegistry, SessionStore};
use std::time::Duration;

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        host: "mock.invalid".into(),
        port: None,
        database: Some("mock".into()),
        user: "mock".into(),
        password: None,
        ssl_mode: Some(SslMode::Disable),
        engine_specific: None,
    }
}

#[tokio::test]
async fn failed_statement_marks_transaction_and_blocks_commit() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_err(DriverError::new(Code::SyntaxError, "bad statement"))
        .build();
    let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let session = store.open_session(OpenSessionRequest {
        tag: None,
        tenant_id: None,
    });
    let connection = store
        .open_connection(session.id, Engine::Postgres, spec())
        .await
        .unwrap();
    let transaction = store
        .begin_transaction(
            session.id,
            BeginTransactionRequest {
                connection: connection.id,
                mode: TxMode::default(),
            },
        )
        .await
        .unwrap();
    let tx = TxHandleRef {
        tx_id: transaction.tx_id,
        connection: transaction.connection,
        mode: transaction.mode,
    };

    assert!(store
        .execute_http(
            session.id,
            ExecuteRequestHttp {
                connection: connection.id,
                sql: "broken".into(),
                params: Vec::new(),
                tx: Some(tx),
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
        )
        .await
        .is_err());
    assert_eq!(
        store.list_transactions(session.id).unwrap()[0].condition,
        TransactionCondition::Failed
    );
    let request = EndTransactionRequest {
        connection: connection.id,
        tx_id: transaction.tx_id,
    };
    assert!(store
        .commit_transaction(session.id, request.clone())
        .await
        .is_err());
    store
        .rollback_transaction(session.id, request)
        .await
        .unwrap();
}

#[tokio::test]
async fn timed_out_commit_keeps_end_claimed_while_driver_is_indeterminate() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .commit_pending()
        .build();
    let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
    store.set_request_timeout(Duration::from_millis(10));
    let session = store.open_session(OpenSessionRequest {
        tag: None,
        tenant_id: None,
    });
    let connection = store
        .open_connection(session.id, Engine::Postgres, spec())
        .await
        .unwrap();
    let transaction = store
        .begin_transaction(
            session.id,
            BeginTransactionRequest {
                connection: connection.id,
                mode: TxMode::default(),
            },
        )
        .await
        .unwrap();
    let request = EndTransactionRequest {
        connection: connection.id,
        tx_id: transaction.tx_id,
    };

    let error = store
        .commit_transaction(session.id, request.clone())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        sift_server::ApiError::Driver(DriverError {
            code: Code::QueryTimedOut,
            ..
        })
    ));
    assert_eq!(store.list_transactions(session.id).unwrap().len(), 1);
    assert!(store.commit_transaction(session.id, request).await.is_err());
}

#[tokio::test]
async fn failed_commit_releases_end_claim_and_keeps_transaction_open() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .commit_err(DriverError::new(Code::ConnectionFailed, "commit failed"))
        .build();
    let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let session = store.open_session(OpenSessionRequest {
        tag: None,
        tenant_id: None,
    });
    let connection = store
        .open_connection(session.id, Engine::Postgres, spec())
        .await
        .unwrap();
    let transaction = store
        .begin_transaction(
            session.id,
            BeginTransactionRequest {
                connection: connection.id,
                mode: TxMode::default(),
            },
        )
        .await
        .unwrap();
    let request = EndTransactionRequest {
        connection: connection.id,
        tx_id: transaction.tx_id,
    };

    assert!(store
        .commit_transaction(session.id, request.clone())
        .await
        .is_err());
    assert_eq!(store.list_transactions(session.id).unwrap().len(), 1);

    store.commit_transaction(session.id, request).await.unwrap();
    assert!(store.list_transactions(session.id).unwrap().is_empty());
}
