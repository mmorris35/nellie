//! SSE transport for MCP clients.
//!
//! Provides Server-Sent Events transport for MCP protocol,
//! allowing Claude Code and other MCP clients to connect.

use axum::{
    extract::State,
    http::StatusCode,
    response::sse::{Event, Sse},
    routing::{get, post},
    Json, Router,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

use super::mcp::{get_tools, McpState, ToolRequest};

type SessionId = String;
type Sessions = Arc<RwLock<HashMap<SessionId, mpsc::Sender<SseMessage>>>>;

/// MCP JSON-RPC request
#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// MCP JSON-RPC response
#[derive(Debug, Clone, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

/// Message to send via SSE
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum SseMessage {
    Response(JsonRpcResponse),
    #[allow(dead_code)]
    Notification(serde_json::Value),
}

/// SSE state
#[derive(Clone)]
pub struct SseState {
    sessions: Sessions,
    mcp_state: Arc<McpState>,
}

impl SseState {
    pub fn new(mcp_state: Arc<McpState>) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            mcp_state,
        }
    }
}

/// Query params for POST endpoint
#[derive(Debug, Deserialize)]
pub struct PostQuery {
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// Create SSE router
pub fn create_sse_router(mcp_state: Arc<McpState>) -> Router {
    let sse_state = SseState::new(mcp_state);

    Router::new()
        .route("/sse", get(sse_handler))
        .route("/message", post(message_handler))
        .with_state(sse_state)
}

/// SSE connection handler
async fn sse_handler(
    State(state): State<SseState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let session_id = generate_session_id();
    tracing::info!(%session_id, "New SSE connection");

    let (tx, rx) = mpsc::channel::<SseMessage>(64);

    // Store session
    state.sessions.write().await.insert(session_id.clone(), tx);

    // Create SSE stream
    let session_for_cleanup = session_id.clone();
    let sessions_for_cleanup = state.sessions.clone();

    let stream = ReceiverStream::new(rx).map(move |msg| {
        let data = serde_json::to_string(&msg).unwrap_or_default();
        Ok(Event::default().event("message").data(data))
    });

    // Prepend endpoint event
    let endpoint_event = futures::stream::once(async move {
        Ok(Event::default()
            .event("endpoint")
            .data(format!("/message?sessionId={session_id}")))
    });

    let combined = endpoint_event.chain(stream);

    // Cleanup on disconnect (via keep-alive timeout)
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        sessions_for_cleanup
            .write()
            .await
            .remove(&session_for_cleanup);
    });

    Sse::new(combined).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// Message handler for client requests
async fn message_handler(
    State(state): State<SseState>,
    axum::extract::Query(query): axum::extract::Query<PostQuery>,
    Json(request): Json<JsonRpcRequest>,
) -> Result<StatusCode, StatusCode> {
    let session_id = query.session_id;
    tracing::debug!(%session_id, method = %request.method, "Received MCP request");

    // Get session sender
    let tx = {
        let sessions = state.sessions.read().await;
        sessions.get(&session_id).cloned()
    };

    let tx = tx.ok_or(StatusCode::NOT_FOUND)?;

    // Handle the request
    let response = handle_mcp_request(&state.mcp_state, request).await;

    // Send response via SSE
    tx.send(SseMessage::Response(response))
        .await
        .map_err(|_| StatusCode::GONE)?;

    Ok(StatusCode::ACCEPTED)
}

/// Handle MCP JSON-RPC request
async fn handle_mcp_request(mcp_state: &McpState, request: JsonRpcRequest) -> JsonRpcResponse {
    let id = request.id.clone();

    let result = match request.method.as_str() {
        "initialize" => Ok(handle_initialize()),
        "initialized" => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(serde_json::json!({})),
                error: None,
            }
        }
        "tools/list" => Ok(handle_list_tools()),
        "tools/call" => handle_call_tool(mcp_state, &request.params).await,
        "ping" => Ok(serde_json::json!({})),
        _ => Err(JsonRpcError {
            code: -32601,
            message: format!("Method not found: {}", request.method),
        }),
    };

    match result {
        Ok(result) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        },
        Err(error) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        },
    }
}

fn handle_initialize() -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "nellie",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn handle_list_tools() -> serde_json::Value {
    let tools = get_tools();
    serde_json::json!({ "tools": tools })
}

async fn handle_call_tool(
    mcp_state: &McpState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let name = params["name"].as_str().ok_or_else(|| JsonRpcError {
        code: -32602,
        message: "Missing tool name".to_string(),
    })?;

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // Use existing tool dispatch
    let request = ToolRequest {
        name: name.to_string(),
        arguments,
    };

    let response = super::mcp::invoke_tool_direct(mcp_state, request).await;

    match response.error {
        Some(err) => Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("Error: {}", err)
            }],
            "isError": true
        })),
        None => Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&response.content).unwrap_or_default()
            }],
            "_meta": {
                "anthropic/maxResultSizeChars": 500_000
            }
        })),
    }
}

fn generate_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{:016x}{:016x}", timestamp, rand::random::<u64>())
}
