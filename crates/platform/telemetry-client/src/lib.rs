//! Engine-independent HTTP transport for caller-defined telemetry payloads.
//! The caller owns the Tokio runtime and decides whether to send telemetry.

use std::time::Duration;

use reqwest::{StatusCode, header};
use rkyv::api::high::{HighDeserializer, HighSerializer, HighValidator};
use rkyv::bytecheck::CheckBytes;
use rkyv::rancor::Error as ArchiveError;
use rkyv::ser::allocator::ArenaHandle;
use rkyv::util::AlignedVec;
use rkyv::{Archive, Deserialize, Serialize};
use telemetry_protocol::{Batch, Envelope};
use tokio::{runtime::Handle, sync::oneshot};
use url::{Host, Url};

const CONTENT_TYPE: &str = "application/octet-stream";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Trusted, operator-configured submission endpoints. Use HTTPS, or HTTP with
/// a literal loopback address for local development.
#[derive(Clone, Debug)]
pub struct EndpointRoutes {
    pub event: Url,
    pub batch: Url,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("endpoint must use HTTPS or HTTP loopback, without credentials or a fragment")]
    InvalidEndpoint,
    #[error("telemetry HTTP transport failed")]
    Transport(#[source] reqwest::Error),
    #[error("telemetry endpoint returned HTTP {0}")]
    Status(StatusCode),
    #[error("telemetry response exceeds {MAX_RESPONSE_BYTES} bytes")]
    ResponseTooLarge,
    #[error("telemetry archive encoding or validation failed")]
    Archive(#[from] ArchiveError),
}

impl From<reqwest::Error> for ClientError {
    fn from(error: reqwest::Error) -> Self {
        Self::Transport(error.without_url())
    }
}

/// Result of an operation dispatched on the caller's runtime. Dropping the
/// receiver does not cancel the HTTP request.
pub type Pending<T> = oneshot::Receiver<Result<T, ClientError>>;

#[derive(Clone)]
pub struct TelemetryClient {
    client: reqwest::Client,
    routes: EndpointRoutes,
}

impl TelemetryClient {
    pub fn new(routes: EndpointRoutes) -> Result<Self, ClientError> {
        validate_endpoint(&routes.event)?;
        validate_endpoint(&routes.batch)?;
        let client = http_client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self { client, routes })
    }

    pub async fn send<E>(&self, event: &Envelope<E>) -> Result<(), ClientError>
    where
        Envelope<E>: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, ArchiveError>>,
    {
        self.post(self.routes.event.clone(), encode(event)?).await
    }

    pub async fn send_batch<E>(&self, batch: &Batch<E>) -> Result<(), ClientError>
    where
        Batch<E>: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, ArchiveError>>,
    {
        self.post(self.routes.batch.clone(), encode(batch)?).await
    }

    pub fn send_via_channel<E>(
        &self,
        runtime: &Handle,
        event: &Envelope<E>,
    ) -> Result<Pending<()>, ClientError>
    where
        Envelope<E>: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, ArchiveError>>,
    {
        Ok(self.post_via_channel(runtime, self.routes.event.clone(), encode(event)?))
    }

    pub fn send_batch_via_channel<E>(
        &self,
        runtime: &Handle,
        batch: &Batch<E>,
    ) -> Result<Pending<()>, ClientError>
    where
        Batch<E>: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, ArchiveError>>,
    {
        Ok(self.post_via_channel(runtime, self.routes.batch.clone(), encode(batch)?))
    }

    /// Fetch and validate an envelope from a trusted, caller-selected query URL.
    pub async fn fetch<E>(&self, route: Url) -> Result<Envelope<E>, ClientError>
    where
        Envelope<E>: Archive,
        <Envelope<E> as Archive>::Archived: for<'a> CheckBytes<HighValidator<'a, ArchiveError>>
            + Deserialize<Envelope<E>, HighDeserializer<ArchiveError>>,
    {
        validate_endpoint(&route)?;
        let mut response = self
            .client
            .get(route)
            .header(header::ACCEPT, CONTENT_TYPE)
            .send()
            .await?;
        check_status(response.status())?;
        if response
            .content_length()
            .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
        {
            return Err(ClientError::ResponseTooLarge);
        }
        let mut bytes = AlignedVec::<16>::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                return Err(ClientError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(rkyv::from_bytes::<Envelope<E>, ArchiveError>(&bytes)?)
    }

    pub fn fetch_via_channel<E>(&self, runtime: &Handle, route: Url) -> Pending<Envelope<E>>
    where
        Envelope<E>: Archive + Send + 'static,
        <Envelope<E> as Archive>::Archived: for<'a> CheckBytes<HighValidator<'a, ArchiveError>>
            + Deserialize<Envelope<E>, HighDeserializer<ArchiveError>>,
    {
        let client = self.clone();
        let (tx, rx) = oneshot::channel();
        runtime.spawn(async move {
            let _ = tx.send(client.fetch(route).await);
        });
        rx
    }

    async fn post(&self, route: Url, body: Vec<u8>) -> Result<(), ClientError> {
        let response = self
            .client
            .post(route)
            .header(header::CONTENT_TYPE, CONTENT_TYPE)
            .body(body)
            .send()
            .await?;
        check_status(response.status())
    }

    fn post_via_channel(&self, runtime: &Handle, route: Url, body: Vec<u8>) -> Pending<()> {
        let client = self.clone();
        let (tx, rx) = oneshot::channel();
        runtime.spawn(async move {
            let _ = tx.send(client.post(route, body).await);
        });
        rx
    }
}

fn encode(
    value: &impl for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, ArchiveError>>,
) -> Result<Vec<u8>, ArchiveError> {
    rkyv::to_bytes(value).map(|bytes| bytes.to_vec())
}

fn check_status(status: StatusCode) -> Result<(), ClientError> {
    if status.is_success() {
        Ok(())
    } else {
        Err(ClientError::Status(status))
    }
}

fn validate_endpoint(url: &Url) -> Result<(), ClientError> {
    let loopback = match url.host() {
        Some(Host::Ipv4(addr)) => addr.is_loopback(),
        Some(Host::Ipv6(addr)) => addr.is_loopback(),
        _ => false,
    };
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(ClientError::InvalidEndpoint);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_policy_accepts_https_and_only_literal_loopback_http() {
        for url in [
            "https://example.com/collect",
            "http://127.0.0.1/collect",
            "http://[::1]/collect",
        ] {
            assert!(
                validate_endpoint(&Url::parse(url).unwrap()).is_ok(),
                "{url}"
            );
        }
        for url in [
            "http://example.com/collect",
            "http://localhost/collect",
            "https://user:secret@example.com/collect",
            "https://example.com/collect#fragment",
            "file:///collect",
        ] {
            assert!(
                matches!(
                    validate_endpoint(&Url::parse(url).unwrap()),
                    Err(ClientError::InvalidEndpoint)
                ),
                "{url}"
            );
        }
    }
}
