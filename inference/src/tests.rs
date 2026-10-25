use crate::{
    client::{Client, Endpoint},
    policy::{Policy, Protocol, Route},
    protocols,
    server::{Gateway, serve},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Response, StatusCode, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, duplex},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};

mod providers;

struct Recorded {
    path: String,
    headers: HeaderMap,
    body: Value,
}
struct Origin {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    task: JoinHandle<()>,
}

impl Origin {
    async fn new(status: StatusCode, response_body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let service =
                        service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                            let recorded = recorded.clone();
                            async move {
                                let (parts, body) = request.into_parts();
                                let body = body.collect().await.unwrap().to_bytes();
                                recorded.lock().unwrap().push(Recorded {
                                    path: parts.uri.to_string(),
                                    headers: parts.headers,
                                    body: serde_json::from_slice(&body).unwrap(),
                                });
                                let mut response = Response::new(Full::new(Bytes::from_static(
                                    response_body.as_bytes(),
                                )));
                                *response.status_mut() = status;
                                response
                                    .headers_mut()
                                    .insert("content-type", "application/json".parse().unwrap());
                                if status == StatusCode::TOO_MANY_REQUESTS {
                                    response
                                        .headers_mut()
                                        .insert("retry-after", "17".parse().unwrap());
                                }
                                Ok::<_, std::convert::Infallible>(response)
                            }
                        });
                    let _ = http1::Builder::new()
                        .keep_alive(false)
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn completions() -> Value {
    json!({"model":"allowed","messages":[{"role":"user","content":"hello"}]})
}
fn messages() -> Value {
    json!({"model":"allowed","max_tokens":1024,"messages":[{"role":"user","content":"hello"}]})
}

fn gateway(completion_origin: &str, messages_origin: &str, other_origin: &str) -> Arc<Gateway> {
    let models = Arc::new(HashSet::from(["allowed".to_owned()]));
    let mut routes = HashMap::new();
    for (path, url, protocol, key, models) in [
        (
            "/opencode-go/v1/chat/completions",
            format!("{completion_origin}/native/chat"),
            Protocol::Completions,
            "go-secret",
            models.clone(),
        ),
        (
            "/opencode-go/v1/messages",
            format!("{messages_origin}/native/messages"),
            Protocol::Messages,
            "go-secret",
            models,
        ),
        (
            "/openrouter/v1/chat/completions",
            format!("{other_origin}/native/chat"),
            Protocol::Completions,
            "router-secret",
            Arc::new(HashSet::from(["other".to_owned()])),
        ),
    ] {
        let endpoint = Arc::new(Endpoint::for_test(
            &url,
            key,
            vec!["x-opencode-session".parse().unwrap()],
        ));
        for path in [path.to_owned(), path.replacen("/v1/", "/", 1)] {
            routes.insert(
                path,
                Route {
                    endpoint: endpoint.clone(),
                    models: Some(models.clone()),
                    protocol,
                },
            );
        }
    }
    Arc::new(Gateway {
        policy: Policy::new(routes),
        client: Client::new(Duration::from_secs(5)).unwrap(),
        max_body: 1024 * 1024,
        max_response_body: 1024 * 1024,
    })
}

fn request(path: &str, body: &str) -> Vec<u8> {
    format!("POST {path} HTTP/1.1\r\nHost: attacker\r\nContent-Type: application/json\r\nAuthorization: guest-key\r\nX-Api-Key: guest-key\r\nX-Opencode-Session: conversation-123\r\nAnthropic-Beta: web-search\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

async fn exchange(gateway: Arc<Gateway>, request: &[u8]) -> Vec<u8> {
    let (mut client, server) = duplex(16384);
    let task = tokio::spawn(serve(server, gateway, Duration::from_secs(5)));
    client.write_all(request).await.unwrap();
    let mut reply = Vec::new();
    timeout(Duration::from_secs(5), client.read_to_end(&mut reply))
        .await
        .unwrap()
        .unwrap();
    let _ = task.await.unwrap();
    reply
}

#[tokio::test]
async fn upstream_and_protocol_routes_keep_credentials_and_payloads_separate() {
    let chat = Origin::new(StatusCode::OK, r#"{"reply":"chat response"}"#).await;
    let msg = Origin::new(StatusCode::OK, r#"{"reply":"messages response"}"#).await;
    let other = Origin::new(StatusCode::OK, r#"{"reply":"router response"}"#).await;
    let gateway = gateway(&chat.url, &msg.url, &other.url);
    for (path, body, response) in [
        (
            "/opencode-go/v1/chat/completions",
            completions(),
            "chat response",
        ),
        ("/opencode-go/v1/messages", messages(), "messages response"),
        (
            "/openrouter/v1/chat/completions",
            json!({"model":"other","messages":[{"role":"user","content":"hi"}]}),
            "router response",
        ),
    ] {
        let reply = exchange(gateway.clone(), &request(path, &body.to_string())).await;
        assert!(reply.starts_with(b"HTTP/1.1 200"));
        assert!(String::from_utf8_lossy(&reply).contains(response));
    }
    for (origin, path, key) in [
        (&chat, "/native/chat", "Bearer go-secret"),
        (&msg, "/native/messages", "Bearer go-secret"),
        (&other, "/native/chat", "Bearer router-secret"),
    ] {
        let recorded = origin.requests.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].path, path);
        assert_eq!(recorded[0].body["stream"], false);
        assert_eq!(recorded[0].headers["authorization"], key);
        assert_eq!(
            recorded[0].headers["x-opencode-session"],
            "conversation-123"
        );
        for header in ["x-api-key", "anthropic-beta"] {
            assert!(!recorded[0].headers.contains_key(header));
        }
        assert_ne!(recorded[0].headers["host"], "attacker");
    }
}

#[tokio::test]
async fn messages_beta_query_is_consumed_without_forwarding_it() {
    let origin = Origin::new(StatusCode::OK, r#"{"ok":true}"#).await;
    let gateway = gateway(&origin.url, &origin.url, &origin.url);
    for path in [
        "/opencode-go/messages?beta=true",
        "/opencode-go/v1/messages?beta=true",
    ] {
        let reply = exchange(gateway.clone(), &request(path, &messages().to_string())).await;
        assert!(reply.starts_with(b"HTTP/1.1 200"));
    }
    let recorded = origin.requests.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    for request in recorded.iter() {
        assert_eq!(request.path, "/native/messages");
        assert_eq!(request.body["model"], "allowed");
        assert!(!request.headers.contains_key("anthropic-beta"));
    }
}

#[test]
fn queries_are_restricted_to_the_exact_messages_beta_marker() {
    let gateway = gateway(
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
    );
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    for prefix in ["", "v1/"] {
        for (suffix, body, queries) in [
            (
                "messages",
                messages(),
                vec![
                    "",
                    "beta",
                    "beta=false",
                    "beta=True",
                    "beta=%74rue",
                    "%62eta=true",
                    "beta=true&beta=true",
                    "beta=true&url=evil",
                    "url=evil&beta=true",
                    "beta=true&",
                ],
            ),
            ("chat/completions", completions(), vec!["beta=true"]),
        ] {
            for query in queries {
                let uri = format!("/opencode-go/{prefix}{suffix}?{query}")
                    .parse()
                    .unwrap();
                assert_eq!(
                    gateway
                        .policy
                        .authorize(
                            &hyper::Method::POST,
                            &uri,
                            &headers,
                            &serde_json::to_vec(&body).unwrap()
                        )
                        .err()
                        .unwrap()
                        .status,
                    StatusCode::BAD_REQUEST,
                    "accepted query on {uri}"
                );
            }
        }
    }
}

#[tokio::test]
async fn rejected_capabilities_models_and_route_overrides_make_no_upstream_calls() {
    let origin = Origin::new(StatusCode::OK, "unused").await;
    let gateway = gateway(&origin.url, &origin.url, &origin.url);
    let mut forbidden = Vec::new();
    for (key, value) in [
        ("store", json!(true)),
        ("stream", json!("yes")),
        ("stream_options", json!({"include_usage":true})),
        ("base_url", json!("https://evil")),
        ("tools", json!([{"type":"web_search"}])),
        (
            "tools",
            json!([{"type":"function","function":{"name":"x","parameters":{"$ref":"https://evil/schema"}}}]),
        ),
    ] {
        let mut body = completions();
        body[key] = value;
        forbidden.push(("/opencode-go/v1/chat/completions", body));
    }
    for options in [
        json!({"include_usage": "yes"}),
        json!({"include_usage": true, "url": "https://evil"}),
    ] {
        let mut body = completions();
        body["stream"] = json!(true);
        body["stream_options"] = options;
        forbidden.push(("/opencode-go/v1/chat/completions", body));
    }
    let mut remote_image = completions();
    remote_image["messages"][0]["content"] =
        json!([{"type":"image_url","image_url":{"url":"https://evil/image"}}]);
    forbidden.push(("/opencode-go/v1/chat/completions", remote_image));
    let mut server_tools = messages();
    server_tools["tools"] = json!([{"type":"web_search_20250305","name":"web_search"}]);
    forbidden.push(("/opencode-go/v1/messages", server_tools));
    let mut mcp = messages();
    mcp["mcp_servers"] = json!([{"url":"https://evil"}]);
    forbidden.push(("/opencode-go/v1/messages", mcp.clone()));
    forbidden.push(("/opencode-go/v1/messages?beta=true", mcp));
    let mut forbidden_model = messages();
    forbidden_model["model"] = json!("forbidden");
    forbidden.push(("/opencode-go/messages?beta=true", forbidden_model));
    forbidden.push(("/openrouter/v1/chat/completions", completions()));
    forbidden.push((
        "/opencode-go/v1/chat/completions?url=https://evil",
        completions(),
    ));
    forbidden.push(("/opencode-go/v1/chat/completions/extra", completions()));
    for (path, body) in forbidden {
        let reply = exchange(gateway.clone(), &request(path, &body.to_string())).await;
        assert!(
            reply.starts_with(b"HTTP/1.1 4"),
            "unexpected response: {}",
            String::from_utf8_lossy(&reply)
        );
        assert!(origin.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn model_validation_and_forwarding_use_the_same_duplicate_key_interpretation() {
    let origin = Origin::new(StatusCode::OK, r#"{"ok":true}"#).await;
    let gateway = gateway(&origin.url, &origin.url, &origin.url);
    let body =
        r#"{"model":"forbidden","model":"allowed","messages":[{"role":"user","content":"hi"}]}"#;
    let reply = exchange(gateway, &request("/opencode-go/v1/chat/completions", body)).await;
    assert!(reply.starts_with(b"HTTP/1.1 200"));
    assert_eq!(origin.requests.lock().unwrap()[0].body["model"], "allowed");
}

#[test]
fn client_function_round_trips_and_thinking_are_supported_in_both_protocols() {
    let chat = json!({"model":"allowed","stream":false,"tools":[{"type":"function","function":{"name":"shell","parameters":{"type":"object"}}}],"messages":[{"role":"assistant","content":null,"reasoning_content":"plan","tool_calls":[{"id":"call_1","type":"function","function":{"name":"shell","arguments":"{}"}}]},{"role":"tool","tool_call_id":"call_1","content":"done"}]});
    assert!(protocols::completions(&chat).is_ok());
    let msg = json!({"model":"allowed","max_tokens":2048,"thinking":{"type":"enabled","budget_tokens":1024},"system":[{"type":"text","text":"code","cache_control":{"type":"ephemeral"}}],"tools":[{"name":"shell","input_schema":{"type":"object"}}],"messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"plan","signature":"opaque"},{"type":"tool_use","id":"call_1","name":"shell","input":{}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"done"}]}]});
    assert!(protocols::messages(&msg).is_ok());
}

#[test]
fn endpoint_origins_cannot_encode_guest_controlled_credentials_or_redirects() {
    for url in [
        "http://localhost/v1/messages",
        "https://user:secret@example.com/v1/messages",
        "https://example.com/v1/messages?url=evil",
        "https://example.com/v1/messages#fragment",
    ] {
        assert!(Endpoint::new(url, HeaderMap::new(), Vec::new()).is_err());
    }
}

#[tokio::test]
async fn upstream_429_and_backoff_survive_without_a_gateway_retry() {
    let origin = Origin::new(
        StatusCode::TOO_MANY_REQUESTS,
        "provider error includes go-secret",
    )
    .await;
    let reply = exchange(
        gateway(&origin.url, &origin.url, &origin.url),
        &request(
            "/opencode-go/v1/chat/completions",
            &completions().to_string(),
        ),
    )
    .await;
    assert!(reply.starts_with(b"HTTP/1.1 429"));
    let reply = String::from_utf8_lossy(&reply).to_lowercase();
    assert!(reply.contains("retry-after: 17"));
    assert!(!reply.contains("go-secret"));
    assert_eq!(origin.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn oversized_request_never_reaches_an_upstream() {
    let origin = Origin::new(StatusCode::OK, "unused").await;
    let mut gateway = gateway(&origin.url, &origin.url, &origin.url);
    Arc::get_mut(&mut gateway).unwrap().max_body = 32;
    let reply = exchange(
        gateway,
        &request(
            "/opencode-go/v1/chat/completions",
            &completions().to_string(),
        ),
    )
    .await;
    assert!(reply.starts_with(b"HTTP/1.1 413"));
    assert!(origin.requests.lock().unwrap().is_empty());
}

// A raw origin makes provider body boundaries and cancellation observable.
async fn read_headers(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> String {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(headers).unwrap()
}

async fn consume_request(stream: &mut tokio::net::TcpStream, streaming: bool) {
    let headers = read_headers(stream).await;
    let accept = if streaming {
        "accept: text/event-stream"
    } else {
        "accept: application/json"
    };
    assert!(
        headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case(accept)),
        "{headers}"
    );
    let length = headers
        .to_lowercase()
        .lines()
        .find_map(|line| {
            line.strip_prefix("content-length: ")
                .and_then(|n| n.parse::<usize>().ok())
        })
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["stream"],
        streaming
    );
}

async fn read_chunk(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> Vec<u8> {
    let mut line = Vec::new();
    while !line.ends_with(b"\r\n") {
        line.push(stream.read_u8().await.unwrap());
    }
    let n = usize::from_str_radix(String::from_utf8(line).unwrap().trim(), 16).unwrap();
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes).await.unwrap();
    assert_eq!(
        stream.read_u16().await.unwrap(),
        u16::from_be_bytes(*b"\r\n")
    );
    bytes
}

#[tokio::test]
async fn whitespace_precedes_complete_json_and_never_splits_it() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (release, wait) = tokio::sync::oneshot::channel();
    let origin = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        consume_request(&mut stream, false).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n{\r\n").await.unwrap();
        wait.await.unwrap();
        stream
            .write_all(b"a\r\n\"ok\":true}\r\n0\r\n\r\n")
            .await
            .unwrap();
    });
    let (mut client, server) = duplex(16384);
    let task = tokio::spawn(serve(
        server,
        gateway(&url, &url, &url),
        Duration::from_secs(5),
    ));
    client
        .write_all(&request(
            "/opencode-go/v1/chat/completions",
            &completions().to_string(),
        ))
        .await
        .unwrap();
    timeout(Duration::from_secs(2), async {
        assert!(read_headers(&mut client).await.starts_with("HTTP/1.1 200"));
        let mut body = read_chunk(&mut client).await;
        assert_eq!(body, b"\n");
        release.send(()).unwrap();
        body.extend(read_chunk(&mut client).await);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"ok":true})
        );
        assert!(read_chunk(&mut client).await.is_empty());
    })
    .await
    .unwrap();
    task.await.unwrap().unwrap();
    origin.await.unwrap();
}

#[tokio::test]
async fn disconnect_while_waiting_for_json_closes_the_provider_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let origin = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        consume_request(&mut stream, false).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n{\r\n").await.unwrap();
        assert!(
            timeout(Duration::from_secs(2), stream.read_u8())
                .await
                .unwrap()
                .is_err()
        );
    });
    let (mut client, server) = duplex(16384);
    let task = tokio::spawn(serve(
        server,
        gateway(&url, &url, &url),
        Duration::from_secs(30),
    ));
    client
        .write_all(&request(
            "/opencode-go/v1/chat/completions",
            &completions().to_string(),
        ))
        .await
        .unwrap();
    timeout(Duration::from_secs(2), async {
        read_headers(&mut client).await;
        assert_eq!(read_chunk(&mut client).await, b"\n");
    })
    .await
    .unwrap();
    drop(client);
    let _ = timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    origin.await.unwrap();
}

#[tokio::test]
async fn event_streams_are_rejected_for_non_stream_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let origin = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        consume_request(&mut stream, false).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let reply = exchange(
        gateway(&url, &url, &url),
        &request(
            "/opencode-go/v1/chat/completions",
            &completions().to_string(),
        ),
    )
    .await;
    assert!(reply.starts_with(b"HTTP/1.1 502"));
    origin.await.unwrap();
}

#[tokio::test]
async fn event_stream_chunks_are_forwarded_as_they_arrive() {
    const FIRST: &[u8] = b"data: {\"delta\":\"one\"}\n\n";
    const SECOND: &[u8] = b"data: {\"delta\":\"two\"}\n\n";

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (release, wait) = tokio::sync::oneshot::channel();
    let origin = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        consume_request(&mut stream, true).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();
        stream
            .write_all(format!("{:x}\r\n", FIRST.len()).as_bytes())
            .await
            .unwrap();
        stream.write_all(FIRST).await.unwrap();
        stream.write_all(b"\r\n").await.unwrap();
        wait.await.unwrap();
        stream
            .write_all(format!("{:x}\r\n", SECOND.len()).as_bytes())
            .await
            .unwrap();
        stream.write_all(SECOND).await.unwrap();
        stream.write_all(b"\r\n0\r\n\r\n").await.unwrap();
    });
    let (mut client, server) = duplex(16384);
    let task = tokio::spawn(serve(
        server,
        gateway(&url, &url, &url),
        Duration::from_secs(5),
    ));
    let mut body = completions();
    body["stream"] = json!(true);
    body["stream_options"] = json!({"include_usage": true});
    client
        .write_all(&request(
            "/opencode-go/v1/chat/completions",
            &body.to_string(),
        ))
        .await
        .unwrap();
    timeout(Duration::from_secs(2), async {
        let headers = read_headers(&mut client).await;
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("content-type: text/event-stream; charset=utf-8")
        );
        assert_eq!(read_chunk(&mut client).await, FIRST);
        release.send(()).unwrap();
        assert_eq!(read_chunk(&mut client).await, SECOND);
        assert!(read_chunk(&mut client).await.is_empty());
    })
    .await
    .unwrap();
    task.await.unwrap().unwrap();
    origin.await.unwrap();
}

#[tokio::test]
async fn event_stream_bound_stops_before_the_excess_chunk() {
    const FIRST: &[u8] = b"data: ok\n\n";
    const EXCESS: &[u8] = b"data: provider-secret\n\n";

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (release, wait) = tokio::sync::oneshot::channel();
    let origin = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        consume_request(&mut stream, true).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();
        stream
            .write_all(format!("{:x}\r\n", FIRST.len()).as_bytes())
            .await
            .unwrap();
        stream.write_all(FIRST).await.unwrap();
        stream.write_all(b"\r\n").await.unwrap();
        wait.await.unwrap();
        let _ = stream
            .write_all(format!("{:x}\r\n", EXCESS.len()).as_bytes())
            .await;
        let _ = stream.write_all(EXCESS).await;
        let _ = stream.write_all(b"\r\n0\r\n\r\n").await;
    });
    let mut gateway = gateway(&url, &url, &url);
    Arc::get_mut(&mut gateway).unwrap().max_response_body = FIRST.len();
    let (mut client, server) = duplex(16384);
    let task = tokio::spawn(serve(server, gateway, Duration::from_secs(5)));
    let mut body = completions();
    body["stream"] = json!(true);
    client
        .write_all(&request(
            "/opencode-go/v1/chat/completions",
            &body.to_string(),
        ))
        .await
        .unwrap();
    timeout(Duration::from_secs(2), async {
        assert!(read_headers(&mut client).await.starts_with("HTTP/1.1 200"));
        assert_eq!(read_chunk(&mut client).await, FIRST);
        release.send(()).unwrap();
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).await.unwrap();
        assert!(!String::from_utf8_lossy(&rest).contains("provider-secret"));
        assert!(!rest.ends_with(b"0\r\n\r\n"));
    })
    .await
    .unwrap();
    let _ = task.await.unwrap();
    origin.await.unwrap();
}

#[tokio::test]
async fn json_responses_are_rejected_for_stream_requests() {
    let origin = Origin::new(StatusCode::OK, r#"{"secret":true}"#).await;
    let mut body = completions();
    body["stream"] = json!(true);
    body["stream_options"] = json!({"include_usage": true});
    let reply = exchange(
        gateway(&origin.url, &origin.url, &origin.url),
        &request("/opencode-go/v1/chat/completions", &body.to_string()),
    )
    .await;
    assert!(reply.starts_with(b"HTTP/1.1 502"));
    assert!(!String::from_utf8_lossy(&reply).contains("secret"));
    assert_eq!(origin.requests.lock().unwrap()[0].body["stream"], true);
    assert_eq!(
        origin.requests.lock().unwrap()[0].body["stream_options"],
        json!({"include_usage": true})
    );
}

#[tokio::test]
async fn response_size_and_json_validation_fail_without_forwarding_provider_data() {
    for (body, limit) in [
        (r#"{"oversized":true}"#, 8),
        ("invalid JSON with go-secret", 1024),
    ] {
        let origin = Origin::new(StatusCode::OK, body).await;
        let mut gateway = gateway(&origin.url, &origin.url, &origin.url);
        Arc::get_mut(&mut gateway).unwrap().max_response_body = limit;
        let reply = exchange(
            gateway,
            &request(
                "/opencode-go/v1/chat/completions",
                &completions().to_string(),
            ),
        )
        .await;
        let text = String::from_utf8_lossy(&reply);
        assert!(!text.contains(body));
        assert!(!text.contains("go-secret"));
        assert!(reply.starts_with(b"HTTP/1.1 502") || !reply.ends_with(b"0\r\n\r\n"));
    }
}

#[tokio::test]
async fn response_bound_also_applies_without_a_content_length() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let origin = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        consume_request(&mut stream, false).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\nf\r\n{\"secret\":true}\r\n0\r\n\r\n").await.unwrap();
    });
    let mut gateway = gateway(&url, &url, &url);
    Arc::get_mut(&mut gateway).unwrap().max_response_body = 8;
    let reply = exchange(
        gateway,
        &request(
            "/opencode-go/v1/chat/completions",
            &completions().to_string(),
        ),
    )
    .await;
    assert!(!String::from_utf8_lossy(&reply).contains("secret"));
    assert!(!reply.ends_with(b"0\r\n\r\n"));
    origin.await.unwrap();
}
