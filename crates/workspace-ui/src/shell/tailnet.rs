use super::*;
use sift_api_types::{
    TailnetMode, TailnetPeer, TailnetProbeRequest, TailnetServeAction, TailnetServeReport,
    TailnetServeRequest, TailnetSettings,
};

pub(super) struct TailnetUi {
    mode: Option<TailnetMode>,
    user: Entity<TextInput>,
    port: Entity<TextInput>,
    pub pending: bool,
    pub peers: Vec<TailnetPeer>,
    pub message: Option<String>,
    pub preview: Option<(TailnetServeRequest, TailnetServeReport)>,
    pub scanned_key: Option<sift_api_types::TailnetHostKey>,
    trusted_key: Option<sift_api_types::TailnetHostKey>,
}

impl TailnetUi {
    pub fn load(&mut self, configuration: &serde_json::Value, cx: &mut Context<WorkspaceShell>) {
        let settings = configuration
            .get("sift_network")
            .and_then(|v| serde_json::from_value::<TailnetSettings>(v.clone()).ok());
        self.mode = settings.as_ref().map(|s| s.mode);
        self.user.update(cx, |input, cx| {
            input.set_text(
                settings.as_ref().map(|s| s.ssh_user.as_str()).unwrap_or(""),
                cx,
            )
        });
        self.port.update(cx, |input, cx| {
            input.set_text(
                settings
                    .as_ref()
                    .map(|s| s.ssh_port)
                    .unwrap_or(22)
                    .to_string(),
                cx,
            )
        });
        self.trusted_key =
            settings
                .and_then(|s| s.host_key)
                .map(|key| sift_api_types::TailnetHostKey {
                    key,
                    fingerprint: "Saved verified key".into(),
                    host: configuration["host"].as_str().unwrap_or_default().into(),
                });
        self.preview = None;
        self.scanned_key = None;
        self.message = None;
    }
    pub fn new(cx: &mut Context<WorkspaceShell>) -> Self {
        Self {
            mode: None,
            user: cx.new(|cx| {
                TextInput::new("", "SSH user on database server", cx).aria_label("Tailnet SSH user")
            }),
            port: cx.new(|cx| TextInput::new("22", "SSH port", cx).aria_label("Tailnet SSH port")),
            pending: false,
            peers: Vec::new(),
            message: None,
            preview: None,
            scanned_key: None,
            trusted_key: None,
        }
    }
    pub fn settings(&self, cx: &Context<WorkspaceShell>) -> Option<TailnetSettings> {
        self.mode.map(|mode| TailnetSettings {
            mode,
            ssh_user: self.user.read(cx).text().trim().into(),
            ssh_port: self.port.read(cx).text().trim().parse().unwrap_or(0),
            host_key: self.trusted_key.as_ref().map(|key| key.key.clone()),
        })
    }
}

impl WorkspaceShell {
    pub(super) fn handle_tailnet_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(
            self.modal,
            Some(Modal::ConnectionUrl | Modal::DatabaseConnection)
        ) || !event.keystroke.modifiers.alt
            || self.tailnet.pending
            || self.database_connection_pending
        {
            return;
        }
        match event.keystroke.key.as_str() {
            "m" => {
                self.tailnet.mode = match self.tailnet.mode {
                    None => Some(TailnetMode::Direct),
                    Some(TailnetMode::Direct) => Some(TailnetMode::Automatic),
                    Some(TailnetMode::Automatic) => Some(TailnetMode::Tunnel),
                    Some(TailnetMode::Tunnel) => None,
                };
                self.tailnet.preview = None;
            }
            "r" => self.send_tailnet(ExecutorCommand::TailnetStatus, cx),
            "d" => self.request_tailnet_probe(cx),
            "f" => {
                if let Ok(request) = self.tailnet_request(cx) {
                    self.send_tailnet(ExecutorCommand::TailnetHostKey(request), cx);
                }
            }
            "v" => self.trust_tailnet_key(cx),
            "p" => self.request_tailnet_serve(TailnetServeAction::Preview, cx),
            "a" => self.request_tailnet_serve(TailnetServeAction::Apply, cx),
            "x" => self.request_tailnet_serve(TailnetServeAction::Remove, cx),
            "j" | "k" => {
                let peers: Vec<_> = self.tailnet.peers.iter().filter(|p| p.online).collect();
                if peers.is_empty() {
                    return;
                }
                let current = self
                    .tailnet_request(cx)
                    .ok()
                    .and_then(|r| peers.iter().position(|p| p.address == r.host));
                let index = match (current, event.keystroke.key.as_str()) {
                    (Some(i), "j") => (i + 1) % peers.len(),
                    (Some(i), _) => (i + peers.len() - 1) % peers.len(),
                    _ => 0,
                };
                let address = peers[index].address.clone();
                if self.modal == Some(Modal::DatabaseConnection) {
                    self.database_host_input
                        .update(cx, |input, cx| input.set_text(&address, cx));
                } else if let Ok(mut url) =
                    url::Url::parse(self.connection_url_input.read(cx).text())
                {
                    if url.set_host(Some(&address)).is_ok() {
                        self.connection_url_input
                            .update(cx, |input, cx| input.set_text(url.as_str(), cx));
                    }
                }
                self.tailnet.preview = None;
                self.tailnet.scanned_key = None;
                self.tailnet.trusted_key = None;
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
    fn trust_tailnet_key(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.tailnet.scanned_key.clone() else {
            return;
        };
        if self
            .tailnet_request(cx)
            .is_ok_and(|request| request.host == key.host)
        {
            self.tailnet.trusted_key = Some(key);
            self.tailnet.scanned_key = None;
            self.tailnet.preview = None;
            self.tailnet.message = Some(
                "Verified host key pinned to this connection. Changed keys will be rejected."
                    .into(),
            );
        } else {
            self.tailnet.message = Some("Target changed; scan the SSH host key again.".into());
        }
        cx.notify();
    }
    fn tailnet_request(&self, cx: &Context<Self>) -> Result<TailnetProbeRequest, String> {
        if self.modal == Some(Modal::DatabaseConnection) {
            return Ok(TailnetProbeRequest {
                host: self.database_host_input.read(cx).text().trim().into(),
                port: self
                    .database_port_input
                    .read(cx)
                    .text()
                    .trim()
                    .parse()
                    .map_err(|_| "Database port required")?,
                settings: self
                    .tailnet
                    .settings(cx)
                    .ok_or("Select a tailnet mode first")?,
            });
        }
        let parsed = parse_connection_url(self.connection_url_input.read(cx).text())?;
        Ok(TailnetProbeRequest {
            host: parsed.configuration["host"]
                .as_str()
                .ok_or("Database host required")?
                .into(),
            port: parsed.configuration["port"]
                .as_u64()
                .and_then(|v| u16::try_from(v).ok())
                .ok_or("Database port required")?,
            settings: self
                .tailnet
                .settings(cx)
                .ok_or("Select a tailnet connection mode first")?,
        })
    }

    fn send_tailnet(&mut self, command: ExecutorCommand, cx: &mut Context<Self>) {
        if self.tailnet.pending || self.database_connection_pending {
            return;
        }
        if self
            .executor_sender
            .as_ref()
            .is_some_and(|sender| sender.send(command).is_ok())
        {
            self.tailnet.pending = true;
            self.tailnet.preview = None;
            self.tailnet.message = Some("Checking from Sift backend…".into());
        } else {
            self.tailnet.message = Some("Connection manager unavailable".into());
        }
        cx.notify();
    }

    fn request_tailnet_probe(&mut self, cx: &mut Context<Self>) {
        match self.tailnet_request(cx) {
            Ok(request) => self.send_tailnet(ExecutorCommand::TailnetProbe(request), cx),
            Err(message) => {
                self.tailnet.message = Some(message);
                cx.notify();
            }
        }
    }

    fn request_tailnet_serve(&mut self, action: TailnetServeAction, cx: &mut Context<Self>) {
        let connection = match self.tailnet_request(cx) {
            Ok(request) => request,
            Err(message) => {
                self.tailnet.message = Some(message);
                cx.notify();
                return;
            }
        };
        let mutation = matches!(
            action,
            TailnetServeAction::Apply | TailnetServeAction::Remove
        );
        let revision = if mutation {
            let Some((preview, report)) = &self.tailnet.preview else {
                return;
            };
            if (action == TailnetServeAction::Apply && report.configured)
                || (action == TailnetServeAction::Remove && !report.owned)
            {
                return;
            }
            // Never apply a preview to a target changed while the request ran.
            if preview.connection.host != connection.host
                || preview.connection.port != connection.port
                || preview.connection.settings != connection.settings
            {
                self.tailnet.preview = None;
                self.tailnet.message =
                    Some("Connection changed. Preview Serve again before confirming.".into());
                cx.notify();
                return;
            }
            Some(report.revision.clone())
        } else {
            None
        };
        self.send_tailnet(
            ExecutorCommand::TailnetServe(TailnetServeRequest {
                connection,
                action,
                acknowledge_exposure: mutation,
                expected_revision: revision,
                expected_instance_id: self
                    .tailnet
                    .preview
                    .as_ref()
                    .map(|(_, report)| report.instance_id.clone()),
            }),
            cx,
        );
    }

    pub(super) fn render_tailnet_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.tailnet.pending || self.database_connection_pending;
        let mode = self.tailnet.mode;
        div().flex().flex_col().gap_2()
            .child(div().text_xs().child("Network · runs on Sift backend · instance administrator required"))
            .child(div().text_xs().whitespace_normal().child("Alt-M mode · Alt-R refresh · Alt-J/K device · Alt-D diagnose · Alt-F fingerprint · Alt-V verify · Alt-P Serve preview · Alt-A confirm enable · Alt-X confirm removal"))
            .child(div().flex().flex_wrap().gap_2().children([
                ("network-off", "Standard", None),
                ("network-direct", "Tailnet direct", Some(TailnetMode::Direct)),
                ("network-auto", "Auto + SSH fallback", Some(TailnetMode::Automatic)),
                ("network-tunnel", "SSH tunnel", Some(TailnetMode::Tunnel)),
            ].into_iter().map(|(id, label, selected)| {
                Button::new(id, label).disabled(pending)
                    .tone(if mode == selected { ButtonTone::Accent } else { ButtonTone::Neutral })
                    .on_click(cx.listener(move |shell, _, _, cx| { shell.tailnet.mode = selected; shell.tailnet.preview = None; cx.notify(); }))
            })))
            .when(mode.is_some(), |container| container
                .child(self.tailnet.user.clone())
                .child(self.tailnet.port.clone())
                .child(div().text_xs().whitespace_normal().child("SSH uses backend agent/default keys and strict known_hosts. Tunnel database target is remote 127.0.0.1; no password prompts. Tab moves between controls."))
                .child(div().flex().flex_wrap().gap_2()
                    .child(Button::new("tailnet-discover", "Refresh devices").disabled(pending).on_click(cx.listener(|shell, _, _, cx| shell.send_tailnet(ExecutorCommand::TailnetStatus, cx))))
                    .child(Button::new("tailnet-probe", "Diagnose network").disabled(pending).on_click(cx.listener(|shell, _, _, cx| shell.request_tailnet_probe(cx))))
                    .child(Button::new("tailnet-host-key", "Read SSH fingerprint").disabled(pending).on_click(cx.listener(|shell, _, _, cx| {
                        match shell.tailnet_request(cx) {
                            Ok(request) => shell.send_tailnet(ExecutorCommand::TailnetHostKey(request), cx),
                            Err(message) => { shell.tailnet.message = Some(message); cx.notify(); }
                        }
                    })))
                    .child(Button::new("tailnet-serve-preview", "Preview / inspect Serve").disabled(pending).on_click(cx.listener(|shell, _, _, cx| shell.request_tailnet_serve(TailnetServeAction::Preview, cx)))))
                .child(div().id("tailnet-peers").max_h(px(120.)).overflow_y_scroll().flex().flex_col().gap_1().children(self.tailnet.peers.iter().enumerate().map(|(index, peer)| {
                    let address = peer.address.clone();
                    Button::new(("tailnet-peer", index), format!("{} · {} · {}", peer.name, peer.address, if peer.online { "online" } else { "offline" }))
                        .disabled(pending || !peer.online)
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            if shell.modal == Some(Modal::DatabaseConnection) {
                                shell.database_host_input.update(cx, |input, cx| input.set_text(&address, cx));
                                shell.tailnet.preview = None;
                                shell.tailnet.trusted_key = None;
                                shell.tailnet.scanned_key = None;
                                cx.notify();
                                return;
                            }
                            if let Ok(mut url) = url::Url::parse(shell.connection_url_input.read(cx).text()) {
                                if url.set_host(Some(&address)).is_ok() {
                                    shell.connection_url_input.update(cx, |input, cx| input.set_text(url.as_str(), cx));
                                    shell.tailnet.preview = None;
                                    shell.tailnet.scanned_key = None;
                                    shell.tailnet.trusted_key = None;
                                }
                            } else { shell.tailnet.message = Some("Enter a database URL first; selecting a device replaces its host.".into()); }
                            cx.notify();
                        }))
                }))))
            .children(self.tailnet.message.as_ref().map(|message| div().text_xs().whitespace_normal().child(message.clone())))
            .children(self.tailnet.scanned_key.as_ref().map(|key| {
                div().flex().flex_col().gap_2()
                    .child(div().text_xs().whitespace_normal().child(format!("Verify {} independently on the server (ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub). Scanning alone does not authenticate a host.", key.fingerprint)))
                    .child(Button::new("tailnet-trust-key", "Fingerprint matches — trust this host").disabled(pending).on_click(cx.listener(|shell, _, _, cx| shell.trust_tailnet_key(cx))))
            }))
            .children(self.tailnet.preview.as_ref().map(|(_, report)| {
                div().flex().flex_col().gap_2()
                    .child(div().text_xs().whitespace_normal().child("Warning: Serve exposes this database to tailnet clients allowed by your policy. Review PostgreSQL localhost authentication first. Requires Python 3 and tailscale operator rights on the remote server. No public Funnel is enabled."))
                    .child(div().flex().gap_2()
                        .child(Button::new("tailnet-serve-apply", "I reviewed access — enable Serve").disabled(pending || report.configured).on_click(cx.listener(|shell, _, _, cx| shell.request_tailnet_serve(TailnetServeAction::Apply, cx))))
                        .child(Button::new("tailnet-serve-remove", "Confirm remove Sift-owned Serve").disabled(pending || !report.owned).on_click(cx.listener(|shell, _, _, cx| shell.request_tailnet_serve(TailnetServeAction::Remove, cx)))))
            }))
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "tailnet_tests.rs"]
mod tests;
