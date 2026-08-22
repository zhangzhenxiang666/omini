use omini_domain::config::ProviderEndpointKind;
use omini_provider_api::LlmClient;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub struct RecordedRequest {
    #[allow(dead_code)] // 共享模块会被每个独立集成测试目标单独编译。
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    #[allow(dead_code)] // 共享模块会被每个独立集成测试目标单独编译。
    pub body: Value,
}

pub struct TestResponse {
    status: u16,
    reason: &'static str,
    body: String,
    content_type: &'static str,
}

impl TestResponse {
    pub fn sse(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            body: body.into(),
            content_type: "text/event-stream",
        }
    }

    #[allow(dead_code)] // 每个集成测试 crate 都会单独编译此共享模块。
    pub fn status(status: u16, reason: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            reason,
            body: body.into(),
            content_type: "text/plain",
        }
    }
}

pub struct TestServer {
    base_url: String,
    requests: Receiver<RecordedRequest>,
    handle: Option<JoinHandle<()>>,
}

impl TestServer {
    pub fn spawn(responses: Vec<TestResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind loopback");
        let base_url = format!(
            "http://{}",
            listener
                .local_addr()
                .expect("test server should have an address")
        );
        let (request_tx, requests) = mpsc::channel();
        let handle = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener
                    .accept()
                    .expect("test server should accept request");
                let request = read_request(&mut stream);
                request_tx
                    .send(request)
                    .expect("test should receive recorded request");
                write_response(&mut stream, response);
            }
        });

        Self {
            base_url,
            requests,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn next_request(&self) -> RecordedRequest {
        self.requests
            .recv_timeout(REQUEST_TIMEOUT)
            .expect("test server should receive request")
    }

    pub fn finish(mut self) {
        self.handle
            .take()
            .expect("test server should not be finished twice")
            .join()
            .expect("test server should exit cleanly");
    }
}

pub fn client(protocol: ProviderEndpointKind, base_url: &str) -> LlmClient {
    LlmClient::with_http_client(
        protocol,
        "test-api-key".to_string(),
        url::Url::parse(base_url).unwrap(),
        test_http_client(),
    )
}

fn test_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("test HTTP client should build")
    })
}

fn read_request(stream: &mut TcpStream) -> RecordedRequest {
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .expect("test request timeout should be configured");

    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream
            .read(&mut chunk)
            .expect("test server should read request");
        assert_ne!(read, 0, "request should include headers");
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };

    let header_text =
        std::str::from_utf8(&bytes[..header_end]).expect("request headers should be UTF-8");
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().expect("request line should exist");
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .expect("request method should exist")
        .to_string();
    let path = request_parts
        .next()
        .expect("request path should exist")
        .to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let content_length = headers
        .get("content-length")
        .expect("JSON request should have content length")
        .parse::<usize>()
        .expect("content length should be numeric");

    while bytes.len() < header_end + content_length {
        let read = stream
            .read(&mut chunk)
            .expect("test server should read request body");
        assert_ne!(read, 0, "request should include complete body");
        bytes.extend_from_slice(&chunk[..read]);
    }

    RecordedRequest {
        method,
        path,
        headers,
        body: serde_json::from_slice(&bytes[header_end..header_end + content_length])
            .expect("request body should be JSON"),
    }
}

fn write_response(stream: &mut TcpStream, response: TestResponse) {
    let text = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status,
        response.reason,
        response.content_type,
        response.body.len(),
        response.body,
    );
    stream
        .write_all(text.as_bytes())
        .expect("test server should write response");
    stream.flush().expect("test server should flush response");
}
