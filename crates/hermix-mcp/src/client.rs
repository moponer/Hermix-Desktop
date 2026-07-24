//! MCP Client (Requirement 14.1-14.3)
//!
//! Connects to external MCP servers, discovers their tools, and
//! dispatches tool calls through the MCP protocol. When a server
//! sends `notifications/tools/list_changed`, the client automatically
//! rediscovers tools and updates the registry.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

use hermix_core::{JsonSchema, ToolError, ToolHandler, ToolSchema};
use hermix_tools::ToolRegistry;

use crate::auth::McpAuthProvider;
use crate::transport::{HttpSseTransport, McpTransport, StdioTransport};
use crate::McpError;

// ---------------------------------------------------------------------------
// ResourceInfo
// ---------------------------------------------------------------------------

/// Information about a resource exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceInfo {
    /// URI identifying this resource (e.g. "file:///path/to/data").
    pub uri: String,
    /// Human-readable name of the resource.
    pub name: String,
    /// Optional description of what this resource contains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME type of the resource content (e.g. "text/plain", "application/json").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Sampling types
// ---------------------------------------------------------------------------

/// Configuration for MCP sampling (server-initiated LLM requests).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingConfig {
    pub max_rpm: u32,
    pub max_tokens_cap: u32,
    pub timeout_secs: u64,
    pub allowed_models: Vec<String>,
    pub max_tool_rounds: u32,
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            max_rpm: 10,
            max_tokens_cap: 4096,
            timeout_secs: 60,
            allowed_models: vec![],
            max_tool_rounds: 3,
        }
    }
}

/// Callback type for LLM invocations triggered by MCP sampling.
pub type LlmCallback = Box<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, McpError>> + Send>> + Send + Sync,
>;

// ---------------------------------------------------------------------------
// Prompt types
// ---------------------------------------------------------------------------

/// Information about a prompt exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<PromptArgument>,
}

/// A single argument descriptor for an MCP prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

/// Result of getting a prompt from an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub messages: Vec<PromptMessage>,
}

/// A single message in a prompt result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PromptsListResponse {
    pub prompts: Vec<PromptInfo>,
}

// ---------------------------------------------------------------------------
// Status / probe types
// ---------------------------------------------------------------------------

/// Status of a single MCP server connection.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerStatus {
    pub name: String,
    pub connected: bool,
    pub tool_count: usize,
    pub resource_count: usize,
    pub transport_type: String,
    pub uptime_secs: Option<u64>,
}

/// Result from probing an MCP server.
#[derive(Debug, Clone, Serialize)]
pub struct McpProbeResult {
    pub reachable: bool,
    pub latency_ms: u64,
    pub tools: Vec<String>,
    pub resources: Vec<String>,
    pub server_info: Option<Value>,
}

// ---------------------------------------------------------------------------
// McpServerConfig
// ---------------------------------------------------------------------------

/// Configuration for connecting to an MCP server.
/// /// Supports two connection modes:
/// - **stdio**: Launch a local process and communicate via stdin/stdout (JSON-RPC)
/// - **HTTP**: Connect to a remote MCP server via HTTP/SSE
#[derive(Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Command to execute for stdio-based servers (e.g. "npx", "python").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Arguments for the stdio command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables to set for the child process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// URL for remote (HTTP/SSE) MCP servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Optional authentication provider for remote servers.
    #[serde(skip)]
    pub auth_provider: Option<Arc<dyn McpAuthProvider>>,
}

impl std::fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerConfig")
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env", &self.env)
            .field("url", &self.url)
            .field(
                "auth_provider",
                &self.auth_provider.as_ref().map(|_| "<McpAuthProvider>"),
            )
            .finish()
    }
}

impl PartialEq for McpServerConfig {
    fn eq(&self, other: &Self) -> bool {
        self.command == other.command
            && self.args == other.args
            && self.env == other.env
            && self.url == other.url
    }
}

impl McpServerConfig {
    /// Create a stdio-based config (local process).
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: Some(command.into()),
            args,
            env: HashMap::new(),
            url: None,
            auth_provider: None,
        }
    }

    /// Create an HTTP-based config (remote server).
    pub fn http(url: impl Into<String>) -> Self {
        Self {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: Some(url.into()),
            auth_provider: None,
        }
    }

    /// Add environment variables to the config.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set an authentication provider for remote servers.
    pub fn with_auth(mut self, provider: Arc<dyn McpAuthProvider>) -> Self {
        self.auth_provider = Some(provider);
        self
    }

    /// Returns true if this config is for a stdio (local process) connection.
    pub fn is_stdio(&self) -> bool {
        self.command.is_some()
    }

    /// Returns true if this config is for an HTTP (remote) connection.
    pub fn is_http(&self) -> bool {
        self.url.is_some()
    }
}

// ---------------------------------------------------------------------------
// MCP protocol types (deserialization helpers)
// ---------------------------------------------------------------------------

/// Result from the MCP initialize method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: Value,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

/// Server info returned during initialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// MCP tool definition from the protocol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct McpToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: JsonSchema,
}

/// Response from tools/list method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ToolsListResponse {
    pub tools: Vec<McpToolDefinition>,
}

/// Response from resources/list method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ResourcesListResponse {
    pub resources: Vec<ResourceInfo>,
}

// ---------------------------------------------------------------------------
// McpClient — single-server connection
// ---------------------------------------------------------------------------

/// A client connection to a single MCP server.
/// /// Handles the full lifecycle of communicating with one MCP server:
/// connecting, initializing, discovering tools, calling tools, reading
/// resources, and disconnecting.
pub struct McpClient {
    /// The configuration used to establish this connection.
    pub config: McpServerConfig,
    /// The transport layer for this connection.
    transport: Option<Box<dyn McpTransport>>,
    /// Cached list of tools discovered from this server.
    tools: Vec<ToolSchema>,
    /// Cached list of resources discovered from this server.
    resources: Vec<ResourceInfo>,
    /// JSON-RPC request ID counter.
    next_id: u64,
    /// Whether the connection has been initialized.
    connected: bool,
    /// Sampling configuration for server-initiated LLM requests.
    sampling_config: Option<SamplingConfig>,
    /// Timestamp when the client connected (for uptime tracking).
    connected_at: Option<Instant>,
}

impl McpClient {
    /// Create a new client for the given config. Does not connect yet.
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            transport: None,
            tools: Vec::new(),
            resources: Vec::new(),
            next_id: 1,
            connected: false,
            sampling_config: None,
            connected_at: None,
        }
    }

    /// Connect to the MCP server: start transport, perform initialize
    /// handshake, and discover available tools.
    pub async fn connect(&mut self) -> Result<(), McpError> {
        if self.connected {
            return Err(McpError::ConnectionError("Already connected".to_string()));
        }

        let mut transport = self.create_transport().await?;
        transport.start().await?;
        self.transport = Some(transport);

        self.initialize().await?;
        self.discover_tools().await?;
        self.connected = true;
        self.connected_at = Some(Instant::now());

        Ok(())
    }

    /// Disconnect from the MCP server and release resources.
    pub async fn disconnect(&mut self) -> Result<(), McpError> {
        if let Some(mut transport) = self.transport.take() {
            transport.close().await?;
        }
        self.connected = false;
        self.connected_at = None;
        self.tools.clear();
        self.resources.clear();
        Ok(())
    }

    /// Returns `true` if the client is currently connected.
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Discover (or re-discover) the tools this server exposes.
    /// /// Sends a `tools/list` JSON-RPC request and parses the response into
    /// a `Vec<ToolSchema>`. The result is also cached internally.
    pub async fn list_tools(&mut self) -> Result<Vec<ToolSchema>, McpError> {
        let result = self
            .send_request("tools/list", serde_json::json!({}))
            .await?;

        let tools_response: ToolsListResponse =
            serde_json::from_value(result).map_err(|e| McpError::Serialization(e.to_string()))?;

        let tools: Vec<ToolSchema> = tools_response
            .tools
            .into_iter()
            .map(|t| ToolSchema {
                name: t.name,
                description: t.description.unwrap_or_default(),
                parameters: t.input_schema,
            })
            .collect();

        self.tools = tools.clone();
        Ok(tools)
    }

    /// Call a tool on this server by name with the given arguments.
    /// /// Sends a `tools/call` JSON-RPC request and returns the result. Text
    /// content items are joined into a single string value; other content
    /// types are returned as raw JSON.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, McpError> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });

        let result = self.send_request("tools/call", params).await?;

        if result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let message = result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|items| {
                    items.iter().find_map(|item| {
                        item.get("text")
                            .and_then(|t| t.as_str())
                            .map(str::to_string)
                    })
                })
                .unwrap_or_else(|| "tool call returned error".to_string());
            return Err(Self::classify_protocol_error(-1, &message));
        }

        if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
            let texts: Vec<String> = content
                .iter()
                .filter_map(|item| {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        item.get("text")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            if !texts.is_empty() {
                return Ok(serde_json::json!(texts.join("\n")));
            }
        }

        Ok(result)
    }

    /// List resources available on this server.
    pub async fn list_resources(&mut self) -> Result<Vec<ResourceInfo>, McpError> {
        let result = self
            .send_request("resources/list", serde_json::json!({}))
            .await?;

        let resources_response: ResourcesListResponse =
            serde_json::from_value(result).map_err(|e| McpError::Serialization(e.to_string()))?;

        self.resources = resources_response.resources.clone();
        Ok(resources_response.resources)
    }

    /// Read a single resource by URI from this server.
    pub async fn read_resource(&mut self, uri: &str) -> Result<Value, McpError> {
        let params = serde_json::json!({ "uri": uri });
        self.send_request("resources/read", params).await
    }

    /// Return the cached tool list from the last `list_tools` / `connect` call.
    pub fn cached_tools(&self) -> &[ToolSchema] {
        &self.tools
    }

    /// Return the cached resource list from the last `list_resources` call.
    pub fn cached_resources(&self) -> &[ResourceInfo] {
        &self.resources
    }

    /// Return the uptime of this connection, if connected.
    pub fn uptime(&self) -> Option<std::time::Duration> {
        self.connected_at.map(|t| t.elapsed())
    }

    /// Set the sampling configuration for server-initiated LLM requests.
    pub fn set_sampling_config(&mut self, config: SamplingConfig) {
        self.sampling_config = Some(config);
    }

    // -----------------------------------------------------------------------
    // Prompt support
    // -----------------------------------------------------------------------

    /// List prompts available on this server.
    pub async fn list_prompts(&mut self) -> Result<Vec<PromptInfo>, McpError> {
        let result = self
            .send_request("prompts/list", serde_json::json!({}))
            .await?;

        let response: PromptsListResponse =
            serde_json::from_value(result).map_err(|e| McpError::Serialization(e.to_string()))?;

        Ok(response.prompts)
    }

    /// Get a prompt by name with the given arguments.
    pub async fn get_prompt(
        &mut self,
        name: &str,
        args: HashMap<String, String>,
    ) -> Result<PromptResult, McpError> {
        let params = serde_json::json!({
            "name": name,
            "arguments": args,
        });

        let result = self.send_request("prompts/get", params).await?;
        let prompt_result: PromptResult =
            serde_json::from_value(result).map_err(|e| McpError::Serialization(e.to_string()))?;

        Ok(prompt_result)
    }

    // -----------------------------------------------------------------------
    // Sampling support (server-initiated LLM requests)
    // -----------------------------------------------------------------------

    /// Handle a sampling request from the MCP server.
    /// /// The server can ask the client to invoke an LLM on its behalf.
    /// The `llm_callback` performs the actual LLM call.
    pub async fn handle_sampling_request(
        &self,
        params: Value,
        llm_callback: &LlmCallback,
    ) -> Result<Value, McpError> {
        let config = self.sampling_config.as_ref().ok_or_else(|| {
            McpError::Config("Sampling not configured on this client".to_string())
        })?;

        let model = params
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("default");

        if !config.allowed_models.is_empty() && !config.allowed_models.iter().any(|m| m == model) {
            return Err(McpError::InvalidParams(format!(
                "Model '{}' is not in the allowed list",
                model
            )));
        }

        let max_tokens = params
            .get("maxTokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(config.max_tokens_cap as u64)
            .min(config.max_tokens_cap as u64);

        let messages = params
            .get("messages")
            .cloned()
            .unwrap_or(serde_json::json!([]));
        let openai_messages = Self::convert_mcp_messages_to_openai(&messages);

        let llm_request = serde_json::json!({
            "model": model,
            "messages": openai_messages,
            "max_tokens": max_tokens,
        });

        let timeout = std::time::Duration::from_secs(config.timeout_secs);
        let result = tokio::time::timeout(timeout, llm_callback(llm_request))
            .await
            .map_err(|_| McpError::ConnectionError("Sampling LLM callback timed out".into()))??;

        let content = result
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        let role = result
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("role"))
            .and_then(|r| r.as_str())
            .unwrap_or("assistant");

        Ok(serde_json::json!({
            "role": role,
            "content": {
                "type": "text",
                "text": content,
            },
            "model": model,
        }))
    }

    fn convert_mcp_messages_to_openai(messages: &Value) -> Value {
        let arr = match messages.as_array() {
            Some(a) => a,
            None => return serde_json::json!([]),
        };

        let converted: Vec<Value> = arr
            .iter()
            .map(|msg| {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                let content = msg
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                    .or_else(|| msg.get("content").and_then(|c| c.as_str()))
                    .unwrap_or("");
                serde_json::json!({
                    "role": role,
                    "content": content,
                })
            })
            .collect();

        Value::Array(converted)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Build the transport from the stored config.
    async fn create_transport(&self) -> Result<Box<dyn McpTransport>, McpError> {
        if self.config.is_stdio() {
            let command = self
                .config
                .command
                .as_ref()
                .ok_or_else(|| McpError::Config("stdio config missing command".to_string()))?;
            Ok(Box::new(StdioTransport::new(
                command,
                &self.config.args,
                &self.config.env,
            )))
        } else if self.config.is_http() {
            let url = self
                .config
                .url
                .as_ref()
                .ok_or_else(|| McpError::Config("http config missing url".to_string()))?;
            let auth_token = if let Some(ref provider) = self.config.auth_provider {
                Some(provider.get_token().await?)
            } else {
                None
            };
            Ok(Box::new(HttpSseTransport::new(url, auth_token)))
        } else {
            Err(McpError::Config(
                "server config must specify either command (stdio) or url (http)".to_string(),
            ))
        }
    }

    /// Send a JSON-RPC request and return the `result` field from the response.
    async fn send_request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let transport = self
            .transport
            .as_mut()
            .ok_or_else(|| McpError::ConnectionError("Not connected".to_string()))?;

        transport.send(request).await?;
        let response = transport.receive().await?;

        if let Some(error) = response.get("error") {
            let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(Self::classify_protocol_error(code, message));
        }

        response.get("result").cloned().ok_or(McpError::Protocol {
            code: -1,
            message: "Missing result in response".to_string(),
        })
    }

    fn classify_protocol_error(code: i64, message: &str) -> McpError {
        let msg_lc = message.to_ascii_lowercase();
        if code == -32601 {
            return McpError::MethodNotFound(message.to_string());
        }
        if code == -32602 {
            return McpError::InvalidParams(message.to_string());
        }
        if code == -32600 || msg_lc.contains("forbidden") || msg_lc.contains("permission denied") {
            return McpError::Forbidden(message.to_string());
        }
        if code == -32001 {
            return McpError::NotConfigured(message.to_string());
        }
        if msg_lc.contains("not configured")
            || msg_lc.contains("missing config")
            || msg_lc.contains("missing command")
            || msg_lc.contains("missing url")
        {
            return McpError::NotConfigured(message.to_string());
        }
        if msg_lc.contains("not found") || msg_lc.contains("unknown method") {
            return McpError::ResourceNotFound(message.to_string());
        }
        McpError::Protocol {
            code,
            message: message.to_string(),
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let transport = self
            .transport
            .as_mut()
            .ok_or_else(|| McpError::ConnectionError("Not connected".to_string()))?;

        transport.send(notification).await?;
        Ok(())
    }

    /// Run the MCP initialize handshake.
    async fn initialize(&mut self) -> Result<InitializeResult, McpError> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": { "listChanged": true },
                "resources": {},
            },
            "clientInfo": {
                "name": "hermes-agent",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });

        let result = self.send_request("initialize", params).await?;
        let init_result: InitializeResult =
            serde_json::from_value(result).map_err(|e| McpError::Serialization(e.to_string()))?;

        self.send_notification("notifications/initialized", serde_json::json!({}))
            .await?;

        Ok(init_result)
    }

    /// Internal alias used during connect().
    async fn discover_tools(&mut self) -> Result<(), McpError> {
        self.list_tools().await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// McpManager — manages multiple McpClient instances
// ---------------------------------------------------------------------------

/// Tool handler that delegates a single MCP tool call back into the
/// owning `McpManager`. The handler holds a `Weak` reference to the
/// manager so the cycle is broken at drop time (the manager is owned
/// by `HttpServerState`; the handlers are owned by the `ToolRegistry`
/// which the manager also references — without `Weak` we would have
/// a leak: registry → handler → manager → registry).
struct McpToolHandler {
    client_name: String,
    tool_name: String,
    /// Cached schema captured at registration time so `schema()` does
    /// not need to lock the manager. The real `inputSchema` returned by
    /// the MCP server is passed through — earlier versions discarded it
    /// and replaced it with `{"type":"object"}`, which left the LLM
    /// with no parameter hints.
    schema: ToolSchema,
    manager: std::sync::Weak<McpManager>,
}

#[async_trait::async_trait]
impl ToolHandler for McpToolHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let Some(manager) = self.manager.upgrade() else {
            return Err(ToolError::ExecutionFailed(format!(
                "MCP manager for server '{}' has been dropped",
                self.client_name
            )));
        };
        match manager
            .call_tool(&self.client_name, &self.tool_name, params)
            .await
        {
            Ok(value) => {
                // `call_tool` joins text content items into a single
                // string (see `McpClient::call_tool`). For Value
                // outputs the MCP server returned as non-text (images,
                // audio, embedded resources), serialise back to JSON so
                // the agent loop has a structured payload to consume.
                match value {
                    Value::String(s) => Ok(s),
                    other => Ok(other.to_string()),
                }
            }
            Err(e) => Err(ToolError::ExecutionFailed(format!(
                "MCP tool '{}::{}' failed: {}",
                self.client_name, self.tool_name, e
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }
}

/// /// The manager can:
/// - Connect to local (stdio) or remote (HTTP/SSE) MCP servers
/// - Discover available tools on each server
/// - Call tools on connected servers
/// - List and read resources from servers
/// - Automatically update the tool registry when servers notify of changes
pub struct McpManager {
    /// Active client connections keyed by server name, behind a
    /// `tokio::sync::Mutex` so all mutating operations can be expressed
    /// as `&self`. Earlier versions used a plain `HashMap`, which forced
    /// `&mut self` on every method — making the manager uncallable from
    /// the tool-dispatch hot path (the closures there only have `&self`).
    clients: Arc<tokio::sync::Mutex<HashMap<String, McpClient>>>,
    /// Shared tool registry for discovered tools.
    tool_registry: Arc<ToolRegistry>,
    /// Back-reference to the `Arc<McpManager>` that owns this struct.
    /// Set by [`ArcMcpManager::new`] via `Arc::new_cyclic` so we can
    /// recover an `Arc<Self>` from `&self` inside `connect()` and hand
    /// `Weak` references to the per-tool handlers (which live in the
    /// tool registry and must NOT strongly own the manager — that
    /// would create a registry → handler → manager → registry cycle).
    self_ref: std::sync::Weak<McpManager>,
}

/// Constructor helper for `McpManager`. Use this instead of
/// `McpManager::new` because the manager internally needs an
/// `Arc<McpManager>` to produce `Weak` references for its handlers.
/// /// Example:
/// ```ignore
/// let manager = ArcMcpManager::new(tool_registry.clone());
/// manager.connect("fetch", cfg).await?;
/// ```
pub struct ArcMcpManager;

impl ArcMcpManager {
    /// Create an `Arc<McpManager>` with the internal weak
    /// back-reference wired up.
    pub fn new(tool_registry: Arc<ToolRegistry>) -> Arc<McpManager> {
        Arc::new_cyclic(|weak| McpManager {
            clients: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            tool_registry,
            self_ref: weak.clone(),
        })
    }
}

impl McpManager {
    /// Construct a manager *without* the cyclic back-reference.
    /// /// Prefer [`ArcMcpManager::new`]. This constructor is kept for
    /// compatibility (some tests construct an `McpManager` directly to
    /// exercise non-`connect` paths). If you call [`McpManager::connect`]
    /// on a value built this way it will panic at runtime — the manager
    /// must be owned by an `Arc<McpManager>` for handlers to work.
    pub fn new(tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            clients: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            tool_registry,
            self_ref: std::sync::Weak::new(),
        }
    }

    /// Connect to an MCP server.
    /// /// Creates an `McpClient`, connects it, and registers the discovered
    /// tools in the shared tool registry with names prefixed by the server
    /// name (e.g. `"server_name__tool_name"`).
    /// /// **Caller contract**: the manager must be wrapped in an
    /// `Arc<McpManager>` (use [`ArcMcpManager::new`]). Otherwise the
    /// handler closures will get dead `Weak` references and every
    /// tool call will fail at runtime.
    pub async fn connect(&self, name: &str, config: McpServerConfig) -> Result<(), McpError> {
        info!("Connecting to MCP server: {}", name);

        let mut client = McpClient::new(config);
        client.connect().await?;

        let tools = client.cached_tools().to_vec();
        debug!("Discovered {} tools from server '{}'", tools.len(), name);

        // Recover an `Arc<Self>` from the internal back-reference so
        // each tool handler can hold a `Weak<McpManager>`.
        let manager_arc: Arc<McpManager> = self
            .self_ref
            .upgrade()
            .expect(
                "McpManager::connect requires the manager to be wrapped in Arc<McpManager>; \
                 use ArcMcpManager::new instead of McpManager::new",
            );

        for tool in &tools {
            let prefixed_name = format!("{}__{}", name, tool.name);
            // PASS THE REAL SCHEMA THROUGH. The `inputSchema` the MCP
            // server returned is what the LLM needs to see — earlier
            // versions replaced it with `{"type":"object"}`, which
            // meant the LLM got zero parameter information for any
            // MCP tool. We cache it on the handler so `schema()` can
            // answer without locking the manager.
            let tool_schema = ToolSchema {
                name: prefixed_name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            };
            let handler = Arc::new(McpToolHandler {
                client_name: name.to_string(),
                tool_name: tool.name.clone(),
                schema: tool_schema.clone(),
                manager: Arc::downgrade(&manager_arc),
            });
            self.tool_registry.register(
                prefixed_name.clone(),
                "mcp",
                tool_schema,
                handler,
                Arc::new(|| true),
                vec![],
                false,
                format!("MCP tool from server '{}'", name),
                "🔌",
                None,
            );
            debug!("Registered MCP tool: {}", prefixed_name);
        }

        let mut guard = self.clients.lock().await;
        guard.insert(name.to_string(), client);
        Ok(())
    }

    /// Disconnect from an MCP server and remove it from the active list.
    pub async fn disconnect(&self, name: &str) -> Result<(), McpError> {
        let removed = {
            let mut guard = self.clients.lock().await;
            guard.remove(name)
        };
        if let Some(mut client) = removed {
            info!("Disconnecting from MCP server: {}", name);
            client.disconnect().await?;
            Ok(())
        } else {
            Err(McpError::ServerNotFound(name.to_string()))
        }
    }

    /// Disconnect all servers.
    pub async fn disconnect_all(&self) -> Result<(), McpError> {
        let names: Vec<String> = self.clients.lock().await.keys().cloned().collect();
        for name in names {
            self.disconnect(&name).await?;
        }
        Ok(())
    }

    /// Check if a server is connected.
    pub async fn is_connected(&self, name: &str) -> bool {
        let guard = self.clients.lock().await;
        guard.get(name).is_some_and(|c| c.is_connected())
    }

    /// Get the list of connected server names.
    pub async fn connected_servers(&self) -> Vec<String> {
        self.clients.lock().await.keys().cloned().collect()
    }

    /// Discover (or re-discover) tools on a connected server.
    pub async fn discover_tools(&self, server_name: &str) -> Result<Vec<ToolSchema>, McpError> {
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.list_tools().await
    }

    /// Call a tool on a connected MCP server.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        args: Value,
    ) -> Result<Value, McpError> {
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.call_tool(tool_name, args).await
    }

    /// List resources available on a connected server.
    pub async fn list_resources(
        &self,
        server_name: &str,
    ) -> Result<Vec<ResourceInfo>, McpError> {
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.list_resources().await
    }

    /// Read a resource from a connected server.
    pub async fn read_resource(&self, server_name: &str, uri: &str) -> Result<Value, McpError> {
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.read_resource(uri).await
    }

    /// Handle a `tools/list_changed` notification from a server.
    /// /// Re-discovers tools from the server and returns the new schema
    /// list. The caller is responsible for diffing against the registry
    /// and re-registering entries that changed.
    pub async fn handle_tools_changed(
        &self,
        server_name: &str,
    ) -> Result<Vec<ToolSchema>, McpError> {
        info!(
            "Handling tools/list_changed notification from '{}'",
            server_name
        );
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        let tools = client.list_tools().await?;
        debug!(
            "Re-discovered {} tools from server '{}'",
            tools.len(),
            server_name
        );
        Ok(tools)
    }

    /// Acquire a short-lived lock on the client map and run `f` against
    /// a specific client. Useful for diagnostic endpoints that need to
    /// peek at a single client without exposing `&mut McpClient` to the
    /// caller (which would break the `&self` contract).
    pub async fn with_client<R>(
        &self,
        name: &str,
        f: impl FnOnce(&mut McpClient) -> R,
    ) -> Result<R, McpError> {
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(name)
            .ok_or_else(|| McpError::ServerNotFound(name.to_string()))?;
        Ok(f(client))
    }

    /// Get a reference to the tool registry.
    pub fn tool_registry(&self) -> &Arc<ToolRegistry> {
        &self.tool_registry
    }

    // -----------------------------------------------------------------------
    // Sampling
    // -----------------------------------------------------------------------

    /// Set the sampling configuration for all connected clients.
    pub async fn set_sampling_config(&self, config: SamplingConfig) {
        let mut guard = self.clients.lock().await;
        for client in guard.values_mut() {
            client.set_sampling_config(config.clone());
        }
    }

    // -----------------------------------------------------------------------
    // Prompts
    // -----------------------------------------------------------------------

    /// List prompts available on a connected server.
    pub async fn list_prompts(&self, server_name: &str) -> Result<Vec<PromptInfo>, McpError> {
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.list_prompts().await
    }

    /// Get a prompt from a connected server.
    pub async fn get_prompt(
        &self,
        server_name: &str,
        name: &str,
        args: HashMap<String, String>,
    ) -> Result<PromptResult, McpError> {
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.get_prompt(name, args).await
    }

    // -----------------------------------------------------------------------
    // Status / probe
    // -----------------------------------------------------------------------

    /// Get the status of all connected MCP servers.
    pub async fn get_status(&self) -> HashMap<String, McpServerStatus> {
        let guard = self.clients.lock().await;
        guard
            .iter()
            .map(|(name, client)| {
                let transport_type = if client.config.is_stdio() {
                    "stdio".to_string()
                } else if client.config.is_http() {
                    "http".to_string()
                } else {
                    "unknown".to_string()
                };
                let status = McpServerStatus {
                    name: name.clone(),
                    connected: client.is_connected(),
                    tool_count: client.cached_tools().len(),
                    resource_count: client.cached_resources().len(),
                    transport_type,
                    uptime_secs: client.uptime().map(|d| d.as_secs()),
                };
                (name.clone(), status)
            })
            .collect()
    }

    /// Probe a connected MCP server to check reachability and discover capabilities.
    pub async fn probe_server(&self, name: &str) -> Result<McpProbeResult, McpError> {
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(name)
            .ok_or_else(|| McpError::ServerNotFound(name.to_string()))?;

        let start = Instant::now();
        let tools_result = client.list_tools().await;
        let latency = start.elapsed();

        match tools_result {
            Ok(tools) => {
                let resources = client.list_resources().await.unwrap_or_default();

                Ok(McpProbeResult {
                    reachable: true,
                    latency_ms: latency.as_millis() as u64,
                    tools: tools.iter().map(|t| t.name.clone()).collect(),
                    resources: resources.iter().map(|r| r.uri.clone()).collect(),
                    server_info: None,
                })
            }
            Err(e) => {
                warn!("Probe failed for server '{}': {}", name, e);
                Ok(McpProbeResult {
                    reachable: false,
                    latency_ms: latency.as_millis() as u64,
                    tools: vec![],
                    resources: vec![],
                    server_info: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::McpClient;
    use crate::McpError;

    #[test]
    fn classify_protocol_error_maps_forbidden() {
        let err = McpClient::classify_protocol_error(-32600, "Forbidden: capability missing");
        assert!(matches!(err, McpError::Forbidden(_)));
    }

    #[test]
    fn classify_protocol_error_maps_not_configured() {
        let err = McpClient::classify_protocol_error(-32001, "Not configured: prompts disabled");
        assert!(matches!(err, McpError::NotConfigured(_)));
    }

    #[test]
    fn classify_protocol_error_maps_not_found() {
        let err = McpClient::classify_protocol_error(-1, "resource not found");
        assert!(matches!(err, McpError::ResourceNotFound(_)));
    }
}
