use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use telemetry_client::{ClientError, EndpointRoutes, TelemetryClient};
use telemetry_protocol::{Batch, ClientHeader, Envelope, EventMetadata, SessionId, Timestamp};
use url::Url;

fn envelope() -> Envelope<String> {
    Envelope {
        metadata: EventMetadata {
            timestamp: Timestamp(42),
            session: SessionId("session".into()),
            client: ClientHeader {
                name: "demo".into(),
                version: "1".into(),
            },
        },
        event: "hello".into(),
    }
}

fn serve_once(
    expected_request: &'static str,
    body: Vec<u8>,
    response_header: &'static str,
    response_body: Vec<u8>,
) -> (Url, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), expected_request);
        let mut content_length = 0;
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap();
            }
        }
        let mut request_body = vec![0; content_length];
        reader.read_exact(&mut request_body).unwrap();
        assert_eq!(request_body, body);
        write!(
            stream,
            "{response_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
            response_body.len()
        )
        .unwrap();
        stream.write_all(&response_body).unwrap();
    });
    (base, thread)
}

fn routes(base: &Url) -> EndpointRoutes {
    EndpointRoutes {
        event: base.join("collect/custom").unwrap(),
        batch: base.join("collect/multiple").unwrap(),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn single_send_and_channel_batch_use_configured_routes_and_rkyv() {
    let runtime = runtime();
    let event = envelope();
    let body = rkyv::to_bytes::<rkyv::rancor::Error>(&event).unwrap();
    let (base, server) = serve_once(
        "POST /collect/custom HTTP/1.1",
        body.to_vec(),
        "HTTP/1.1 204 No Content\r\n",
        vec![],
    );
    let client = TelemetryClient::new(routes(&base)).unwrap();
    runtime.block_on(client.send(&event)).unwrap();
    server.join().unwrap();

    let batch = Batch {
        events: vec![event],
    };
    let body = rkyv::to_bytes::<rkyv::rancor::Error>(&batch).unwrap();
    let (base, server) = serve_once(
        "POST /collect/multiple HTTP/1.1",
        body.to_vec(),
        "HTTP/1.1 204 No Content\r\n",
        vec![],
    );
    let client = TelemetryClient::new(routes(&base)).unwrap();
    let result = client
        .send_batch_via_channel(runtime.handle(), &batch)
        .unwrap();
    runtime.block_on(result).unwrap().unwrap();
    server.join().unwrap();
}

#[test]
fn channel_query_decodes_validated_envelopes_and_async_query_rejects_bad_data() {
    let runtime = runtime();
    let event = envelope();
    let body = rkyv::to_bytes::<rkyv::rancor::Error>(&event).unwrap();
    let (base, server) = serve_once(
        "GET /query?id=one HTTP/1.1",
        vec![],
        "HTTP/1.1 200 OK\r\n",
        body.to_vec(),
    );
    let client = TelemetryClient::new(routes(&base)).unwrap();
    let result =
        client.fetch_via_channel::<String>(runtime.handle(), base.join("query?id=one").unwrap());
    assert_eq!(runtime.block_on(result).unwrap().unwrap(), event);
    server.join().unwrap();

    let (base, server) = serve_once(
        "GET /query HTTP/1.1",
        vec![],
        "HTTP/1.1 200 OK\r\n",
        vec![0; 3],
    );
    assert!(matches!(
        runtime.block_on(client.fetch::<String>(base.join("query").unwrap())),
        Err(ClientError::Archive(_))
    ));
    server.join().unwrap();
}

#[test]
fn rejects_unsafe_urls_and_does_not_follow_redirects() {
    for url in [
        "http://example.com/collect",
        "https://user:secret@example.com/collect",
        "https://example.com/collect#fragment",
    ] {
        let mut routes = routes(&Url::parse("https://example.com").unwrap());
        routes.event = Url::parse(url).unwrap();
        assert!(matches!(
            TelemetryClient::new(routes),
            Err(ClientError::InvalidEndpoint)
        ));
    }

    let runtime = runtime();
    let event = envelope();
    let body = rkyv::to_bytes::<rkyv::rancor::Error>(&event).unwrap();
    let (base, server) = serve_once(
        "POST /collect/custom HTTP/1.1",
        body.to_vec(),
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/elsewhere\r\n",
        vec![],
    );
    let client = TelemetryClient::new(routes(&base)).unwrap();
    assert!(matches!(
        runtime.block_on(client.send(&event)),
        Err(ClientError::Status(status)) if status.as_u16() == 307
    ));
    server.join().unwrap();
}
