mod support;

use serde_json::Value;
use std::collections::HashSet;

#[tokio::test]
async fn openapi_exposes_routes_and_resolvable_schemas() {
    let mut daemon = support::TestDaemon::start("openapi").await;
    let response = daemon
        .client()
        .get(daemon.url("/openapi.json"))
        .send()
        .await
        .expect("OpenAPI request should complete");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let document: Value = response.json().await.expect("OpenAPI should be JSON");
    let paths = document["paths"]
        .as_object()
        .expect("paths should be an object");
    assert_eq!(
        paths["/v1/projects"]["post"]["responses"]["201"]["content"]["application/json"]["schema"]
            ["$ref"],
        "#/components/schemas/ProjectSummary"
    );
    assert!(
        paths["/v1/projects/{project_id}/threads/{thread_id}/runs"]["post"]["responses"]["202"]
            .is_object()
    );
    // 忙碌时重复提交返回 409 run_busy，被拒绝的输入不保存。
    assert!(
        paths["/v1/projects/{project_id}/threads/{thread_id}/runs"]["post"]["responses"]["409"]
            .is_object()
    );
    assert!(paths["/v1/projects/{project_id}/threads/{thread_id}/rename"]["post"]["responses"]["204"]["content"].is_null());
    assert!(paths["/v1/projects/{project_id}/threads/{thread_id}/status"]["get"]["responses"]["200"]["content"].is_object());
    assert!(
        paths["/v1/projects/{project_id}/threads/{thread_id}/events"]["get"]["responses"]["101"]
            .is_object()
    );
    assert!(paths["/v1/projects/{project_id}/threads/{thread_id}/attachments"]["post"]["requestBody"]["content"]["multipart/form-data"].is_object());
    assert!(paths["/v1/projects/{project_id}/threads/{thread_id}/attachments/{attachment_id}"]["get"]["responses"]["200"]["content"].is_object());
    assert!(
        paths
            .get("/v1/projects/{project_id}/threads/{thread_id}/open")
            .is_none()
    );
    assert!(
        paths
            .get("/v1/projects/{project_id}/threads/{thread_id}/runs/{run_id}/messages")
            .is_none()
    );

    let mut operations = HashSet::new();
    for methods in paths.values() {
        for (method, operation) in methods.as_object().expect("path item should be object") {
            if !["get", "post", "patch", "delete", "put"].contains(&method.as_str()) {
                continue;
            }
            let id = operation["operationId"]
                .as_str()
                .expect("operation ID should exist");
            assert!(operations.insert(id), "duplicate operation ID: {id}");
            assert!(
                operation["responses"]["default"]["content"]["application/json"].is_object(),
                "{id} should document protocol errors"
            );
        }
    }
    assert_eq!(operations.len(), 43);

    fn check_refs(document: &Value, value: &Value) {
        match value {
            Value::Object(entries) => {
                if let Some(reference) = entries.get("$ref").and_then(Value::as_str) {
                    assert!(
                        reference.starts_with("#/"),
                        "non-local reference: {reference}"
                    );
                    assert!(
                        document.pointer(&reference[1..]).is_some(),
                        "unresolved schema: {reference}"
                    );
                }
                for child in entries.values() {
                    check_refs(document, child);
                }
            }
            Value::Array(items) => {
                for child in items {
                    check_refs(document, child);
                }
            }
            _ => {}
        }
    }
    check_refs(&document, &document);
    daemon.shutdown().await;
}

#[tokio::test]
async fn extractor_rejections_use_protocol_error() {
    let mut daemon = support::TestDaemon::start("extractor-errors").await;
    let malformed = daemon
        .client()
        .post(daemon.url("/projects"))
        .header("content-type", "application/json")
        .body("{")
        .send()
        .await
        .expect("malformed JSON request should complete");
    assert_eq!(malformed.status(), reqwest::StatusCode::BAD_REQUEST);
    let error: omini_protocol::ProtocolError = malformed.json().await.expect("error should decode");
    assert_eq!(error.code, "invalid_json");

    let query = daemon
        .client()
        .get(daemon.url("/projects/missing/threads/missing/runs?include_archived=maybe"))
        .send()
        .await
        .expect("invalid query request should complete");
    assert_eq!(query.status(), reqwest::StatusCode::BAD_REQUEST);
    let error: omini_protocol::ProtocolError = query.json().await.expect("error should decode");
    assert_eq!(error.code, "invalid_query");

    let multipart = daemon
        .client()
        .post(daemon.url("/projects/missing/threads/missing/attachments"))
        .header("x-omini-client-id", "client")
        .send()
        .await
        .expect("invalid multipart request should complete");
    assert_eq!(multipart.status(), reqwest::StatusCode::BAD_REQUEST);
    let error: omini_protocol::ProtocolError = multipart.json().await.expect("error should decode");
    assert_eq!(error.code, "invalid_attachment_upload");

    let websocket = daemon
        .client()
        .get(daemon.url("/projects/missing/threads/missing/events"))
        .header("x-omini-client-id", "client")
        .send()
        .await
        .expect("invalid WebSocket request should complete");
    assert_eq!(websocket.status(), reqwest::StatusCode::BAD_REQUEST);
    let error: omini_protocol::ProtocolError = websocket.json().await.expect("error should decode");
    assert_eq!(error.code, "invalid_websocket_upgrade");

    for path in [
        "/projects/missing/threads/missing/open",
        "/projects/missing/threads/missing/runs/missing/messages",
    ] {
        let response = daemon
            .client()
            .post(daemon.url(path))
            .send()
            .await
            .expect("removed route request should complete");
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        let error: omini_protocol::ProtocolError = response
            .json()
            .await
            .expect("removed route error should decode");
        assert_eq!(error.code, "route_not_found");
    }

    let response = daemon
        .client()
        .post(daemon.url("/health"))
        .send()
        .await
        .expect("unsupported method request should complete");
    assert_eq!(response.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);
    let error: omini_protocol::ProtocolError = response.json().await.expect("error should decode");
    assert_eq!(error.code, "method_not_allowed");
    daemon.shutdown().await;
}
