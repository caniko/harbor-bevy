# telemetry-client

Send a `telemetry_protocol::Envelope<E>` or `Batch<E>` to the URLs you
configure in `EndpointRoutes`. Query with `fetch` and the full query URL.
The client sends rkyv archives as `application/octet-stream` and validates
returned archives. Ensure the server uses the same types and rkyv wire
configuration.

Use the async methods directly, or a `*_via_channel` method with an active
Tokio runtime handle to receive its result through a one-shot channel.
The client never sends automatically; the application decides when to
send and enforces its consent policy. No Bevy dependency is required.
