//! Read-only hints from the selected server's provider registry.

use super::*;

const MAX_CAPABILITIES: usize = 40;
const MAX_LIMIT_CHARS: usize = 256;

pub(super) fn descriptor_for_instance<'a>(
    selected_instance_id: Option<&str>,
    requested_instance_id: &str,
    provider_id: &str,
    providers: &'a [sift_protocol::ProviderDescriptor],
) -> Option<&'a sift_protocol::ProviderDescriptor> {
    if selected_instance_id.unwrap_or("local") != requested_instance_id {
        return None;
    }
    providers
        .iter()
        .find(|descriptor| descriptor.provider.provider_id.as_str() == provider_id)
}

pub(super) fn provider_quality_label(
    quality: Option<sift_protocol::ProviderQuality>,
) -> &'static str {
    match quality {
        Some(sift_protocol::ProviderQuality::Compatible) => "Compatible",
        Some(sift_protocol::ProviderQuality::QueryCapable) => "Query capable",
        Some(sift_protocol::ProviderQuality::Transactional) => "Transactional",
        Some(sift_protocol::ProviderQuality::IdeCapable) => "IDE capable",
        Some(sift_protocol::ProviderQuality::SiftCertified) => "Sift certified",
        None => "Unrated",
    }
}

impl WorkspaceShell {
    pub(super) fn open_provider_details(
        &mut self,
        provider_id: &sift_protocol::ProviderId,
        cx: &mut Context<Self>,
    ) {
        self.connection_row_menu = None;
        self.modal = Some(Modal::ProviderDetails {
            instance_id: self
                .selected_instance_id
                .as_deref()
                .unwrap_or("local")
                .to_owned(),
            provider_id: provider_id.as_str().to_owned(),
        });
        cx.notify();
    }

    pub(super) fn render_provider_details(
        &self,
        instance_id: &str,
        provider_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        let provider = descriptor_for_instance(
            self.selected_instance_id.as_deref(),
            instance_id,
            provider_id,
            &self.lifecycle.providers,
        );
        let Some(provider) = provider else {
            return div().flex().flex_col().gap_2()
                .child(div().text_lg().child("Provider details"))
                .child(div().text_sm().text_color(colors.muted_text).child("Provider information is unavailable for the selected server. Reconnect to refresh it."))
                .into_any_element();
        };
        let display_name = provider.display_name.chars().take(128).collect::<String>();
        let version = provider
            .provider
            .provider_version
            .chars()
            .take(64)
            .collect::<String>();
        let capabilities = provider
            .capabilities
            .iter()
            .take(MAX_CAPABILITIES)
            .enumerate()
            .map(|(index, capability)| {
                let limits = if capability.limits.is_empty() {
                    String::new()
                } else {
                    format!(
                        " · limits {}",
                        serde_json::to_string(&capability.limits)
                            .unwrap_or_default()
                            .chars()
                            .take(MAX_LIMIT_CHARS)
                            .collect::<String>()
                    )
                };
                div()
                    .id(("provider-capability", index))
                    .text_xs()
                    .font_family("monospace")
                    .child(format!(
                        "{}{}",
                        capability.id.chars().take(128).collect::<String>(),
                        limits
                    ))
            });
        div().h(px(500.)).flex().flex_col().gap_3()
            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(display_name))
            .child(div().text_sm().text_color(colors.muted_text).child(format!("{} · dialect {} · provider version {}", provider.provider.provider_id, provider.provider.dialect_id, version)))
            .child(div().text_sm().child(format!("{} · {} · configuration schema v{}", if provider.available { "Available on this server" } else { "Unavailable on this server" }, provider_quality_label(provider.quality), provider.configuration_schema_version)))
            .child(div().text_xs().text_color(colors.muted_text).child("Capabilities are advertised by the server; each action still checks authorization and current connection state."))
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Capabilities ({})", provider.capabilities.len())))
            .child(div().id("provider-capability-list").debug_selector(|| "provider-capability-list".into()).flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap_1().children(capabilities))
            .children((provider.capabilities.len() > MAX_CAPABILITIES).then(|| div().text_xs().text_color(colors.warning).child(format!("Showing first {MAX_CAPABILITIES} capabilities"))))
            .child(div().text_xs().text_color(colors.muted_text).child("Vim: Esc close"))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_lookup_never_reuses_another_servers_hints() {
        let provider = sift_protocol::ProviderDescriptor {
            provider: sift_protocol::ProviderRef {
                provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
                dialect_id: sift_protocol::DialectId::new("sift/postgresql").unwrap(),
                provider_version: "1.0".into(),
            },
            display_name: "PostgreSQL".into(),
            configuration_schema: serde_json::json!({}),
            credential_schema: serde_json::json!({}),
            configuration_schema_version: 1,
            capabilities: vec![],
            quality: Some(sift_protocol::ProviderQuality::SiftCertified),
            available: true,
        };
        let providers = vec![provider];
        assert!(
            descriptor_for_instance(Some("server-a"), "server-a", "sift/postgres", &providers)
                .is_some()
        );
        assert!(
            descriptor_for_instance(Some("server-b"), "server-a", "sift/postgres", &providers)
                .is_none()
        );
        assert!(
            descriptor_for_instance(Some("server-a"), "server-a", "sift/sqlite", &providers)
                .is_none()
        );
    }
}
