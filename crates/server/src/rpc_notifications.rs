//! Versioned subscriptions with bounded credit and a two-part stop barrier.
use super::*;
use crate::registry::ProviderNotificationStream;
use sift_driver_api::PgNotification;
use sift_extension_protocol::{
    ConnectionDisposition, DriverListenAck, DriverListenRequest, DriverNotificationFrame,
    DriverUnlistenAck, DriverUnlistenRequest,
};
use sift_plugin_host::{ReceivedStreamFrame, RpcStream};

const STOP_TIMEOUT: Duration = Duration::from_secs(5);
struct Subscription {
    process: Arc<SupervisedProcess>,
    provider: ProviderId,
    connection: WireId,
    subscription: WireId,
    channels: Vec<String>,
    max_event_bytes: usize,
}
impl Subscription {
    fn invalid(&self) -> DriverError {
        DriverError::new(Code::ConnectionInvalidated,
            "External notification state is invalid or uncertain; generation stopped; reconnect required")
            .with_provider(self.provider.clone())
    }
    async fn fence(&self) -> DriverError {
        let _ = self
            .process
            .shutdown("invalid or uncertain notification subscription")
            .await;
        self.invalid()
    }
    fn event(&self, frame: &ReceivedStreamFrame) -> Result<Option<PgNotification>, DriverError> {
        if frame.encoded_bytes > self.max_event_bytes {
            return Err(self.invalid());
        }
        match serde_json::from_value::<DriverNotificationFrame>(frame.frame.payload.clone()) {
            Ok(DriverNotificationFrame::Notification {
                connection,
                subscription,
                channel,
                payload,
            }) if connection == self.connection
                && subscription == self.subscription
                && self.channels.contains(&channel) =>
            {
                Ok(Some(PgNotification { channel, payload }))
            }
            Ok(DriverNotificationFrame::Stopped {
                connection,
                subscription,
                disposition,
            }) if connection == self.connection
                && subscription == self.subscription
                && disposition == ConnectionDisposition::Reusable =>
            {
                Ok(None)
            }
            _ => Err(self.invalid()),
        }
    }
    async fn credit(
        &self,
        stream: &RpcStream,
        frame: &ReceivedStreamFrame,
    ) -> Result<(), DriverError> {
        match tokio::time::timeout(STOP_TIMEOUT, stream.accept(frame)).await {
            Ok(Ok(())) => Ok(()),
            _ => Err(self.invalid()),
        }
    }
    async fn stop(
        &self,
        stream: &mut RpcStream,
        mut request: Request,
        mut stopped: bool,
    ) -> Result<(), DriverError> {
        request.deadline_unix_ms = deadline_unix_ms(STOP_TIMEOUT);
        let cleanup = async {
            let response = self.process.request(request);
            tokio::pin!(response);
            let mut acknowledged = false;
            while !acknowledged || !stopped {
                tokio::select! {
                    result = &mut response, if !acknowledged => {
                        let response = result.map_err(|_| self.invalid())?;
                        let ResponseResult::Ok { payload } = response.result else { return Err(self.invalid()) };
                        let ack: DriverUnlistenAck = serde_json::from_value(payload).map_err(|_| self.invalid())?;
                        if ack.connection != self.connection || ack.subscription != self.subscription
                            || ack.disposition != ConnectionDisposition::Reusable { return Err(self.invalid()); }
                        acknowledged = true;
                    }
                    frame = stream.next(), if !stopped => {
                        let frame = frame.ok_or_else(|| self.invalid())?;
                        if self.event(&frame)?.is_some() { self.credit(stream, &frame).await?; }
                        else { stopped = true; }
                    }
                }
            }
            if stream.try_next().is_some() {
                return Err(self.invalid());
            }
            Ok(())
        };
        match tokio::time::timeout(STOP_TIMEOUT, cleanup).await {
            Ok(Ok(())) => Ok(()),
            _ => Err(self.fence().await),
        }
    }
    async fn pump(
        self,
        mut stream: RpcStream,
        stop: Request,
        sender: mpsc::Sender<Result<PgNotification, DriverError>>,
        done: tokio::sync::oneshot::Sender<Result<(), DriverError>>,
    ) {
        let mut stopped = false;
        let mut unexpected_end = false;
        let mut uncertain = false;
        loop {
            tokio::select! {
                biased;
                _ = sender.closed() => break,
                frame = stream.next() => {
                    let Some(frame) = frame else { uncertain = true; break };
                    match self.event(&frame) {
                        Ok(Some(event)) => {
                            if self.credit(&stream, &frame).await.is_err() { uncertain = true; break; }
                            if sender.send(Ok(event)).await.is_err() { break; }
                        }
                        Ok(None) => { stopped = true; unexpected_end = true; break; }
                        Err(_) => { uncertain = true; break; }
                    }
                }
            }
        }
        let result = if uncertain {
            Err(self.fence().await)
        } else {
            self.stop(&mut stream, stop, stopped).await
        };
        let result = if unexpected_end && result.is_ok() {
            Err(DriverError::new(
                Code::ConnectionFailed,
                "External notification subscription ended",
            ))
        } else {
            result
        };
        if let Err(error) = &result {
            let _ = sender.try_send(Err(error.clone()));
        }
        drop(stream);
        let _ = done.send(result);
    }
}

impl RpcProvider {
    pub(super) async fn external_notifications(
        &self,
        handle: &ProviderConnectionHandle,
        channels: Vec<String>,
    ) -> Result<ProviderNotificationStream, DriverError> {
        if !self
            .descriptor
            .capabilities
            .iter()
            .any(|cap| cap.id == "driver.notifications@1")
            || !self
                .process
                .method_family_version("driver")
                .is_some_and(|v| v >= 4)
        {
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                "Notifications require Driver RPC v4 and driver.notifications@1",
            ));
        }
        if !sift_extension_protocol::valid_notification_channels(&channels) {
            return Err(DriverError::new(Code::InvalidParameterValue,
                "Notifications require 1..16 distinct channels of 1..63 UTF-8 bytes without controls"));
        }
        let connection = self.connection(handle)?.value;
        let subscription = WireId::from_u128(uuid::Uuid::new_v4().as_u128());
        let frame_budget = self.process.maximum_frame_bytes();
        let max_event_bytes = (64 * 1024).min(frame_budget + 4);
        if max_event_bytes < 1024 {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "Notification frame budget is too small",
            ));
        }
        let initial_credit_bytes = (2 * max_event_bytes).min(frame_budget + 4) as u64;
        let input = DriverListenRequest {
            connection,
            subscription,
            channels: channels.clone(),
            max_event_bytes: max_event_bytes as u32,
            initial_credit_bytes,
        };
        let mut request = self.request(
            "listen",
            serde_json::to_value(input).map_err(protocol_error)?,
        );
        request.stream_id = Some(subscription);
        if serde_json::to_vec(&sift_extension_protocol::Message::Request(request.clone()))
            .map_err(protocol_error)?
            .len()
            > frame_budget
        {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "Notification request exceeds frame budget",
            ));
        }
        let context = Subscription {
            process: self.process.clone(),
            provider: self.descriptor.provider.provider_id.clone(),
            connection,
            subscription,
            channels,
            max_event_bytes,
        };
        let (stream, ack) = match self
            .process
            .request_stream_with_credit(request, initial_credit_bytes)
            .await
        {
            Ok(result) => result,
            Err(_) => return Err(context.fence().await),
        };
        let ack = serde_json::from_value::<DriverListenAck>(ack);
        if !matches!(ack, Ok(ack) if ack.connection == connection && ack.subscription == subscription
            && ack.channels == context.channels && ack.disposition == ConnectionDisposition::Reusable)
        {
            return Err(context.fence().await);
        }
        let stop = self.request(
            "unlisten",
            serde_json::to_value(DriverUnlistenRequest {
                connection,
                subscription,
            })
            .map_err(protocol_error)?,
        );
        let (sender, notifications) = mpsc::channel(8);
        let (done, settled) = tokio::sync::oneshot::channel();
        tokio::spawn(context.pump(stream, stop, sender, done));
        Ok(ProviderNotificationStream {
            notifications,
            settled,
        })
    }
}
