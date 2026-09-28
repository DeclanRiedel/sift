use super::*;
use gpui::{TestAppContext, VisualTestContext};

#[gpui::test]
fn url_dialog_shows_separate_ssh_user_in_tunnel_mode(cx: &mut TestAppContext) {
    let window = super::super::tests::shell(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.simulate_resize(gpui::size(px(1200.), px(800.)));
    let workspace = window.root(&mut cx).unwrap();
    workspace.update(&mut cx, |shell, cx| {
        shell.modal = Some(Modal::ConnectionUrl);
        shell.tailnet.mode = Some(TailnetMode::Tunnel);
        shell.connection_url_input.update(cx, |input, cx| {
            input.set_text("postgresql://owner:secret@db.example.ts.net:5432/app", cx)
        });
        cx.notify();
    });
    cx.run_until_parked();
    let url = cx.debug_bounds("connection-url-input").expect("URL field");
    let ssh_user = cx.debug_bounds("tailnet-ssh-user").expect("SSH user field");
    assert!(
        url.right() < ssh_user.left(),
        "URL and SSH settings should have separate columns"
    );
    assert!(cx.debug_bounds("tailnet-ssh-port").is_some());
}

#[gpui::test]
fn tailnet_settings_save_without_password_in_configuration(cx: &mut TestAppContext) {
    let window = super::super::tests::shell(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let workspace = window.root(&mut cx).unwrap();
    let (sender, mut receiver) = ExecutorSender::channel(8);
    workspace.update(&mut cx, |shell, cx| {
        shell.executor_sender = Some(sender);
        shell.selected_database_tenant = Some(1);
        shell.connection_url_input.update(cx, |input, cx| {
            input.set_text(
                "postgresql://fixture:fixture@100.83.175.73:5432/fixture",
                cx,
            )
        });
        shell.tailnet.mode = Some(TailnetMode::Automatic);
        shell
            .tailnet
            .user
            .update(cx, |input, cx| input.set_text("fixture", cx));
        shell.submit_connection_url(cx);
    });
    let Ok(ExecutorCommand::CreateConnectionProfile {
        configuration,
        credentials,
        ..
    }) = receiver.try_recv()
    else {
        panic!("expected save");
    };
    assert_eq!(configuration["sift_network"]["mode"], "automatic");
    assert_eq!(configuration["sift_network"]["ssh_user"], "fixture");
    assert!(configuration.get("password").is_none());
    assert_eq!(credentials.unwrap()["password"], "fixture");
}

#[gpui::test]
fn serve_confirmation_cannot_follow_target_change(cx: &mut TestAppContext) {
    let window = super::super::tests::shell(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let workspace = window.root(&mut cx).unwrap();
    let (sender, mut receiver) = ExecutorSender::channel(8);
    workspace.update(&mut cx, |shell, cx| {
        shell.executor_sender = Some(sender);
        shell.tailnet.mode = Some(TailnetMode::Direct);
        shell.connection_url_input.update(cx, |input, cx| {
            input.set_text("postgresql://fixture@100.83.175.73:5432/fixture", cx)
        });
        let connection = shell.tailnet_request(cx).unwrap();
        shell.tailnet.preview = Some((
            TailnetServeRequest {
                connection,
                action: TailnetServeAction::Preview,
                acknowledge_exposure: false,
                expected_revision: None,
                expected_instance_id: None,
            },
            TailnetServeReport {
                instance_id: "fixture".into(),
                revision: "fixture".into(),
                owned: false,
                configured: false,
                message: "preview".into(),
            },
        ));
        shell.connection_url_input.update(cx, |input, cx| {
            input.set_text("postgresql://fixture@100.83.175.74:5432/fixture", cx)
        });
        shell.request_tailnet_serve(TailnetServeAction::Apply, cx);
        assert!(shell.tailnet.preview.is_none());
        assert!(shell
            .tailnet
            .message
            .as_deref()
            .unwrap()
            .contains("changed"));
    });
    assert!(receiver.try_recv().is_err());
}
