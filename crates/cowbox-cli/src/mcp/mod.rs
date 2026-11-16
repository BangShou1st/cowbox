mod adapter;
mod process;
mod protocol;

use crate::cli::McpAdapterArgs;
use crate::cli::McpProxyArgs;
use crate::config::{self, Config};
use crate::error::{CliError, CliResult};
use crate::workspace;
use protocol::{
    AdapterFailure, AdapterMode, AdapterOutput, AdapterRequest, INTERNAL_ERROR, INVALID_PARAMS,
    INVALID_REQUEST, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, METHOD_NOT_FOUND, PARSE_ERROR,
    SERVER_NOT_INITIALIZED, control_tools, error_response, is_control_tool,
    negotiate_protocol_version, response,
};
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const SANDBOX_START_GRACE: Duration = Duration::from_secs(15);
const CLIENT_QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const CHILD_STDOUT_LIMIT: usize = 8 * 1024 * 1024;
const CHILD_STDERR_LIMIT: usize = 2 * 1024 * 1024;

enum InputEvent {
    Line(Vec<u8>),
    Oversized,
    Closed,
}

struct Proxy {
    workspace: PathBuf,
    executable: PathBuf,
    upstream: Vec<String>,
    network: cowbox_core::NetworkMode,
    session: Option<String>,
    call_timeout: Option<Duration>,
    discovery_timeout: Option<Duration>,
    max_response_bytes: usize,
    allow_read: Vec<PathBuf>,
    disconnect: Arc<AtomicBool>,
    initialized: bool,
    tools: Option<Vec<Value>>,
}

#[derive(Debug, Clone)]
struct SessionSnapshot {
    id: String,
    head: u64,
    status: String,
}

struct SandboxExecution {
    process: process::ProcessOutput,
    envelope: Option<Value>,
    adapter: Option<AdapterOutput>,
}

pub fn adapter_main(args: &McpAdapterArgs) -> CliResult<i32> {
    adapter::main(args)
}

pub fn proxy_main(args: &McpProxyArgs) -> CliResult<i32> {
    let mut proxy = Proxy::new(args)?;
    proxy.run()
}

impl Proxy {
    fn new(args: &McpProxyArgs) -> CliResult<Self> {
        if args.max_response_bytes == 0 || args.max_response_bytes > MAX_RESPONSE_BYTES {
            return Err(CliError::new(
                "MCP_RESPONSE_LIMIT_INVALID",
                format!("--max-response-bytes must be between 1 and {MAX_RESPONSE_BYTES}"),
            )
            .exit(2));
        }
        if args.upstream.is_empty() {
            return Err(CliError::new(
                "MCP_UPSTREAM_INVALID",
                "mcp-proxy requires an upstream command after `--`",
            )
            .exit(2));
        }
        let upstream = args
            .upstream
            .iter()
            .map(|value| {
                value.clone().into_string().map_err(|_| {
                    CliError::new(
                        "MCP_UPSTREAM_INVALID",
                        "upstream command arguments must be valid UTF-8",
                    )
                    .exit(2)
                })
            })
            .collect::<CliResult<Vec<_>>>()?;
        let start = match &args.workspace {
            Some(path) => path.clone(),
            None => std::env::current_dir()?,
        };
        let workspace = workspace::find_workspace_root(&start)?;
        if !workspace.is_dir() {
            return Err(CliError::new(
                "MCP_WORKSPACE_INVALID",
                format!("workspace {} is not a directory", workspace.display()),
            )
            .exit(2));
        }
        let config = Config::load(&workspace)?;
        let network = config.network_policy(args.network.map(Into::into));
        let call_timeout = config::parse_timeout(&args.timeout)?;
        let discovery_timeout = config::parse_timeout(&args.discovery_timeout)?;
        let executable = std::env::current_exe()?.canonicalize()?;
        let mut allow_read = vec![executable.clone()];
        allow_read.extend(
            args.allow_read
                .iter()
                .map(|path| workspace::resolve_path(&workspace, path)),
        );
        eprintln!(
            "cowbox mcp-proxy: workspace={} network={} ({}) timeout={}",
            workspace.display(),
            network.mode.as_str(),
            network.source.as_str(),
            args.timeout
        );
        Ok(Self {
            workspace,
            executable,
            upstream,
            network: network.mode,
            session: args.session.clone(),
            call_timeout,
            discovery_timeout,
            max_response_bytes: args.max_response_bytes,
            allow_read,
            disconnect: Arc::new(AtomicBool::new(false)),
            initialized: false,
            tools: None,
        })
    }

    fn run(&mut self) -> CliResult<i32> {
        let (sender, receiver) = mpsc::channel();
        let disconnect = Arc::clone(&self.disconnect);
        thread::spawn(move || read_client_input(sender, disconnect));

        loop {
            let event = match receiver.recv() {
                Ok(event) => event,
                Err(_) => return Ok(0),
            };
            match event {
                InputEvent::Closed => return Ok(0),
                InputEvent::Oversized => {
                    if !self.emit(error_response(
                        Value::Null,
                        INVALID_REQUEST,
                        format!("JSON-RPC request exceeded {MAX_REQUEST_BYTES} bytes"),
                        None,
                    ))? {
                        return Ok(0);
                    }
                }
                InputEvent::Line(line) => {
                    if self.disconnect.load(Ordering::Relaxed) {
                        return Ok(0);
                    }
                    let Some(response) = self.handle_line(&line) else {
                        continue;
                    };
                    if !self.emit(response)? || self.disconnect.load(Ordering::Relaxed) {
                        return Ok(0);
                    }
                }
            }
        }
    }

    fn handle_line(&mut self, line: &[u8]) -> Option<Value> {
        let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
        let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
        if trimmed.is_empty() {
            return None;
        }
        let value: Value = match serde_json::from_slice(trimmed) {
            Ok(value) => value,
            Err(error) => {
                return Some(error_response(
                    Value::Null,
                    PARSE_ERROR,
                    format!("invalid JSON: {error}"),
                    None,
                ));
            }
        };
        if value.is_array() {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "JSON-RPC batches are not supported by this MCP version",
                None,
            ));
        }
        let Some(object) = value.as_object() else {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "JSON-RPC message must be an object",
                None,
            ));
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_response(
                object.get("id").cloned().unwrap_or(Value::Null),
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
                None,
            ));
        }
        let id = object.get("id").cloned();
        let method = object.get("method").and_then(Value::as_str);
        let Some(method) = method else {
            if id.is_some() && (object.contains_key("result") || object.contains_key("error")) {
                eprintln!("cowbox mcp-proxy: ignoring client response");
            }
            return None;
        };
        let Some(id) = id else {
            self.handle_notification(method, object.get("params"));
            return None;
        };
        Some(self.handle_request(id, method, object.get("params")))
    }

    fn handle_notification(&mut self, method: &str, _params: Option<&Value>) {
        match method {
            "notifications/initialized" => {}
            "notifications/cancelled" => {
                eprintln!("cowbox mcp-proxy: client cancelled a completed or queued request");
            }
            _ => eprintln!("cowbox mcp-proxy: ignoring notification {method}"),
        }
    }

    fn handle_request(&mut self, id: Value, method: &str, params: Option<&Value>) -> Value {
        if params.is_some_and(|params| !params.is_object()) {
            return error_response(id, INVALID_PARAMS, "params must be an object", None);
        }
        if method == "initialize" {
            return self.initialize(id, params);
        }
        if method == "ping" {
            return response(id, json!({}));
        }
        if !self.initialized {
            return error_response(
                id,
                SERVER_NOT_INITIALIZED,
                "initialize must be called before this request",
                None,
            );
        }
        match method {
            "tools/list" => self.tools_list(id),
            "tools/call" => self.tools_call(id, params),
            _ => error_response(
                id,
                METHOD_NOT_FOUND,
                format!("method {method} is not supported"),
                None,
            ),
        }
    }

    fn initialize(&mut self, id: Value, params: Option<&Value>) -> Value {
        let requested = params
            .and_then(|params| params.get("protocolVersion"))
            .and_then(Value::as_str);
        let negotiated = negotiate_protocol_version(requested);
        self.initialized = true;
        response(
            id,
            json!({
                "protocolVersion": negotiated,
                "capabilities": {
                    "tools": { "listChanged": false }
                },
                "serverInfo": {
                    "name": "cowbox",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "Use upstream tools for workspace edits. Each tools/call creates a cowbox snapshot step. Inspect with cowbox_diff and use cowbox_rollback instead of stacking fixes. cowbox_commit only previews changes; the user must run `cowbox commit --step <n>` in the same workspace terminal and approve it."
            }),
        )
    }

    fn tools_list(&mut self, id: Value) -> Value {
        match self.ensure_tools() {
            Ok(tools) => response(id, json!({ "tools": tools })),
            Err(error) => error_response(
                id,
                INTERNAL_ERROR,
                error.message,
                Some(json!({
                    "code": error.code,
                    "hint": error.hint,
                })),
            ),
        }
    }

    fn tools_call(&mut self, id: Value, params: Option<&Value>) -> Value {
        let Some(params) = params else {
            return error_response(id, INVALID_PARAMS, "tools/call requires params", None);
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error_response(id, INVALID_PARAMS, "tools/call name must be a string", None);
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return error_response(
                id,
                INVALID_PARAMS,
                "tools/call arguments must be an object",
                None,
            );
        }
        if is_control_tool(name) {
            if let Err(error) = validate_control_arguments(name, &arguments) {
                return error_response(id, INVALID_PARAMS, error, None);
            }
            return response(id, self.call_control(name, &arguments));
        }
        match self.ensure_tools() {
            Ok(tools) => {
                if !tools.iter().any(|tool| tool["name"] == name) {
                    return error_response(
                        id,
                        INVALID_PARAMS,
                        format!("unknown tool {name:?}"),
                        None,
                    );
                }
            }
            Err(error) => {
                return error_response(
                    id,
                    INTERNAL_ERROR,
                    error.message,
                    Some(json!({ "code": error.code, "hint": error.hint })),
                );
            }
        }
        response(id, self.call_upstream(name, &arguments))
    }

    fn ensure_tools(&mut self) -> Result<Vec<Value>, InvocationError> {
        if self.tools.is_none() {
            let request = AdapterRequest {
                mode: AdapterMode::List,
                workspace: self.workspace.clone(),
                upstream: self.upstream.clone(),
                tool: None,
                arguments: json!({}),
                timeout_ms: duration_ms(self.discovery_timeout),
                max_response_bytes: self.max_response_bytes,
            };
            let execution = self
                .execute_sandbox(&request, self.discovery_timeout, true)
                .map_err(InvocationError::from_cli)?;
            if execution.process.disconnected {
                return Err(InvocationError::new(
                    "CLIENT_DISCONNECTED",
                    "MCP client disconnected during tool discovery",
                ));
            }
            if execution.process.timed_out {
                return Err(InvocationError::new(
                    "MCP_DISCOVERY_TIMEOUT",
                    "sandboxed MCP tool discovery timed out",
                ));
            }
            let adapter = execution.adapter.clone().ok_or_else(|| {
                invocation_from_execution(
                    &execution,
                    "MCP_DISCOVERY_FAILED",
                    "tool discovery did not produce an adapter response",
                )
            })?;
            if !adapter.ok {
                let failure = adapter.error.unwrap_or_else(|| AdapterFailure {
                    code: "MCP_DISCOVERY_FAILED".to_string(),
                    message: "tool discovery failed".to_string(),
                    hint: None,
                    upstream_stderr: None,
                });
                return Err(InvocationError {
                    code: failure.code,
                    message: failure.message,
                    hint: failure.hint.or(failure.upstream_stderr),
                });
            }
            let tools = adapter.tools.unwrap_or_default();
            validate_upstream_tools(&tools)?;
            self.tools = Some(tools);
        }
        let mut tools = self.tools.clone().unwrap_or_default();
        tools.extend(control_tools());
        Ok(tools)
    }

    fn call_upstream(&mut self, name: &str, arguments: &Value) -> Value {
        let before = match self.inspect_binding() {
            Ok(snapshot) => snapshot,
            Err(error) => return self.tool_error_without_step(&error),
        };
        let head_before = before.as_ref().map(|session| session.head).unwrap_or(0);
        let request = AdapterRequest {
            mode: AdapterMode::Call,
            workspace: self.workspace.clone(),
            upstream: self.upstream.clone(),
            tool: Some(name.to_string()),
            arguments: arguments.clone(),
            timeout_ms: duration_ms(self.call_timeout),
            max_response_bytes: self.max_response_bytes,
        };
        let execution = match self.execute_sandbox(&request, self.call_timeout, false) {
            Ok(execution) => execution,
            Err(error) => {
                return tool_error(
                    "MCP_SANDBOX_FAILED",
                    error.message,
                    error.hint,
                    base_metadata(&self.workspace, self.session.as_deref(), head_before, None),
                );
            }
        };
        if execution.process.disconnected {
            return tool_error(
                "CLIENT_DISCONNECTED",
                "MCP client disconnected",
                None,
                base_metadata(&self.workspace, self.session.as_deref(), head_before, None),
            );
        }
        let envelope = execution.envelope.as_ref();
        if envelope
            .and_then(|value| value.get("ok"))
            .and_then(Value::as_bool)
            == Some(false)
        {
            let code = envelope
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str)
                .unwrap_or("MCP_SANDBOX_FAILED");
            let message = envelope
                .and_then(|value| value.pointer("/error/message"))
                .and_then(Value::as_str)
                .unwrap_or("cowbox could not create a sandbox step");
            let hint = envelope
                .and_then(|value| value.pointer("/error/hint"))
                .and_then(Value::as_str);
            return tool_error(
                code,
                message,
                hint.map(ToString::to_string),
                base_metadata(&self.workspace, self.session.as_deref(), head_before, None),
            );
        }
        let session = envelope.and_then(session_from_envelope);
        let step = envelope
            .and_then(|value| value.pointer("/data/step"))
            .and_then(Value::as_u64);
        let exit_code = envelope
            .and_then(|value| value.pointer("/data/exit_code"))
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        let timed_out = envelope
            .and_then(|value| value.pointer("/data/timed_out"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(session) = &session {
            if self.session.is_none() {
                self.session = Some(session.id.clone());
            } else if self.session.as_deref() != Some(session.id.as_str()) {
                return tool_error(
                    "MCP_SESSION_CHANGED",
                    format!(
                        "cowbox returned session {}, but the proxy is bound to {}",
                        session.id,
                        self.session.as_deref().unwrap_or("none")
                    ),
                    Some("restart mcp-proxy or finish the other active session".to_string()),
                    base_metadata(&self.workspace, self.session.as_deref(), head_before, step),
                );
            }
        }
        let head_after = session.as_ref().map(|session| session.head);
        let metadata = snapshot_metadata(
            &self.workspace,
            session.as_ref(),
            head_before,
            step,
            exit_code,
            timed_out,
        );
        if timed_out {
            return tool_error(
                "MCP_STEP_TIMEOUT",
                "the cowbox step timed out; its snapshot is recorded and can be inspected or rolled back",
                None,
                metadata,
            );
        }
        let Some(adapter) = execution.adapter else {
            return tool_error(
                "MCP_ADAPTER_RESPONSE_INVALID",
                "the sandboxed adapter did not return a valid MCP response",
                None,
                metadata,
            );
        };
        if !adapter.ok {
            let failure = adapter.error.unwrap_or_else(|| AdapterFailure {
                code: "MCP_UPSTREAM_FAILED".to_string(),
                message: "upstream MCP call failed".to_string(),
                hint: None,
                upstream_stderr: None,
            });
            let hint = append_stderr(failure.hint, failure.upstream_stderr);
            return tool_error(failure.code, failure.message, hint, metadata);
        }
        let Some(rpc) = adapter.response else {
            return tool_error(
                "MCP_UPSTREAM_PROTOCOL",
                "upstream tools/call response is missing",
                None,
                metadata,
            );
        };
        if let Some(error) = rpc.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("upstream tools/call failed");
            return tool_error(
                "MCP_UPSTREAM_RPC_ERROR",
                format!("upstream MCP error {code}: {message}"),
                error.get("data").map(ToString::to_string),
                metadata,
            );
        }
        let Some(result) = rpc.get("result") else {
            return tool_error(
                "MCP_UPSTREAM_PROTOCOL",
                "upstream tools/call response has neither result nor error",
                None,
                metadata,
            );
        };
        let mut result_object = result.as_object().cloned().unwrap_or_else(|| {
            let mut object = Map::new();
            object.insert(
                "content".to_string(),
                json!([{ "type": "text", "text": result.to_string() }]),
            );
            object
        });
        attach_cowbox_metadata(&mut result_object, metadata);
        result_object.insert(
            "content".to_string(),
            append_snapshot_content(
                result_object.get("content").cloned(),
                head_before,
                step,
                head_after,
            ),
        );
        Value::Object(result_object)
    }

    fn call_control(&mut self, name: &str, arguments: &Value) -> Value {
        let before = match self.inspect_binding() {
            Ok(Some(session)) => session,
            Ok(None) => {
                return tool_error(
                    "NO_ACTIVE_SESSION",
                    "no active cowbox session for this workspace",
                    Some("call an upstream workspace tool first".to_string()),
                    base_metadata(&self.workspace, self.session.as_deref(), 0, None),
                );
            }
            Err(error) => return self.tool_error_without_step(&error),
        };
        let step = arguments.get("step").and_then(Value::as_u64);
        let mut args: Vec<OsString> = vec![OsString::from(name.trim_start_matches("cowbox_"))];
        match name {
            "cowbox_rollback" => {
                args.push(OsString::from(step.unwrap_or(0).to_string()));
                args.push(OsString::from("--json"));
            }
            "cowbox_diff" => {
                args.push(OsString::from("--json"));
                if let Some(step) = step {
                    args.push(OsString::from("--step"));
                    args.push(OsString::from(step.to_string()));
                }
            }
            "cowbox_commit" => {
                args.push(OsString::from("--dry-run"));
                args.push(OsString::from("--json"));
                if let Some(step) = step {
                    args.push(OsString::from("--step"));
                    args.push(OsString::from(step.to_string()));
                }
            }
            _ => {}
        }
        let cli = match self.run_cli(&args) {
            Ok(cli) => cli,
            Err(error) => {
                return tool_error(
                    "MCP_CONTROL_FAILED",
                    error.message,
                    error.hint,
                    snapshot_metadata(
                        &self.workspace,
                        Some(&before),
                        before.head,
                        None,
                        None,
                        false,
                    ),
                );
            }
        };
        let Some(envelope) = cli.envelope else {
            return tool_error(
                "MCP_CONTROL_FAILED",
                "cowbox control command returned invalid JSON",
                None,
                snapshot_metadata(
                    &self.workspace,
                    Some(&before),
                    before.head,
                    None,
                    cli.process.exit_code,
                    cli.process.timed_out,
                ),
            );
        };
        if envelope["ok"].as_bool() != Some(true) {
            let code = envelope
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("MCP_CONTROL_FAILED");
            let message = envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("cowbox control command failed");
            let hint = envelope
                .pointer("/error/hint")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            return tool_error(
                code,
                message,
                hint,
                snapshot_metadata(
                    &self.workspace,
                    Some(&before),
                    before.head,
                    None,
                    cli.process.exit_code,
                    cli.process.timed_out,
                ),
            );
        }
        let session = session_from_envelope(&envelope).unwrap_or_else(|| before.clone());
        let head_after = session.head;
        let target = step.unwrap_or(head_after);
        let mut data = envelope.get("data").cloned().unwrap_or_else(|| json!({}));
        if name == "cowbox_commit" {
            if let Some(object) = data.as_object_mut() {
                object.insert("committed".to_string(), Value::Bool(false));
                object.insert("target_step".to_string(), json!(target));
                object.insert(
                    "terminal_command".to_string(),
                    json!(format!("cowbox commit --step {target}")),
                );
            }
        }
        let metadata = snapshot_metadata(
            &self.workspace,
            Some(&session),
            before.head,
            None,
            cli.process.exit_code,
            false,
        );
        let mut result = Map::new();
        result.insert(
            "content".to_string(),
            json!([{
                "type": "text",
                "text": format!(
                    "[cowbox] {}: session={} head_before={} head_after={}\n{}",
                    name,
                    session.id,
                    before.head,
                    head_after,
                    serde_json::to_string_pretty(&data).unwrap_or_else(|_| data.to_string())
                )
            }]),
        );
        result.insert("structuredContent".to_string(), data);
        attach_cowbox_metadata(&mut result, metadata);
        Value::Object(result)
    }

    fn inspect_binding(&mut self) -> Result<Option<SessionSnapshot>, InvocationError> {
        let active = self.inspect_active()?;
        if let Some(bound) = &self.session {
            match &active {
                Some(active) if &active.id == bound => Ok(Some(active.clone())),
                Some(active) => Err(InvocationError::new(
                    "MCP_SESSION_CHANGED",
                    format!(
                        "workspace active session is {}, but mcp-proxy is bound to {bound}",
                        active.id
                    ),
                )),
                None => Err(InvocationError::new(
                    "MCP_SESSION_ENDED",
                    format!(
                        "bound cowbox session {bound} is no longer active; start a new mcp-proxy after commit or discard"
                    ),
                )),
            }
        } else {
            if let Some(active) = &active {
                self.session = Some(active.id.clone());
            }
            Ok(active)
        }
    }

    fn inspect_active(&self) -> Result<Option<SessionSnapshot>, InvocationError> {
        let cli = self
            .run_cli(&[OsString::from("log"), OsString::from("--json")])
            .map_err(InvocationError::from_cli)?;
        let envelope = cli.envelope.ok_or_else(|| {
            InvocationError::new(
                "MCP_STATE_INVALID",
                "`cowbox log --json` did not return JSON",
            )
        })?;
        if envelope["ok"].as_bool() == Some(true) {
            return Ok(session_from_envelope(&envelope));
        }
        if envelope.pointer("/error/code").and_then(Value::as_str) == Some("NO_ACTIVE_SESSION") {
            return Ok(None);
        }
        let code = envelope
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("MCP_STATE_INVALID");
        let message = envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("cannot inspect the active cowbox session");
        Err(InvocationError::new(code, message))
    }

    fn execute_sandbox(
        &self,
        request: &AdapterRequest,
        timeout: Option<Duration>,
        discovery: bool,
    ) -> CliResult<SandboxExecution> {
        let encoded = serde_json::to_vec(request)?;
        if encoded.len() > MAX_REQUEST_BYTES {
            return Err(CliError::new(
                "MCP_REQUEST_TOO_LARGE",
                format!("sandbox adapter request exceeded {MAX_REQUEST_BYTES} bytes"),
            ));
        }
        let temp = if discovery {
            Some(
                tempfile::Builder::new()
                    .prefix("cowbox-mcp-discovery-")
                    .tempdir_in(std::env::temp_dir())?,
            )
        } else {
            None
        };
        let mut env = Vec::new();
        if let Some(temp) = &temp {
            env.push((
                OsString::from("COWBOX_HOME"),
                temp.path().join("state").into_os_string(),
            ));
        }
        let sandbox_timeout = timeout.map(|timeout| timeout + SANDBOX_START_GRACE);
        let process_timeout = sandbox_timeout.map(|timeout| timeout + Duration::from_secs(5));
        let mut args = vec![
            OsString::from("run"),
            OsString::from("--json"),
            OsString::from("--timeout"),
            OsString::from(format_timeout(sandbox_timeout)),
        ];
        if self.network.isolates_network() {
            args.push(OsString::from("--network"));
            args.push(OsString::from("off"));
        }
        for path in &self.allow_read {
            args.push(OsString::from("--allow-read"));
            args.push(path.as_os_str().to_os_string());
        }
        if !discovery {
            if let Some(session) = &self.session {
                args.push(OsString::from("--session"));
                args.push(OsString::from(session));
            }
        }
        args.push(OsString::from("--"));
        args.push(self.executable.as_os_str().to_os_string());
        args.push(OsString::from("__mcp-adapter"));
        args.push(OsString::from("--max-response-bytes"));
        args.push(OsString::from(self.max_response_bytes.to_string()));

        let result = process::run(process::ProcessSpec {
            program: self.executable.as_os_str().to_os_string(),
            args,
            cwd: self.workspace.clone(),
            env,
            stdin: Some(encoded),
            timeout: process_timeout,
            disconnect: Arc::clone(&self.disconnect),
            max_stdout: CHILD_STDOUT_LIMIT,
            max_stderr: CHILD_STDERR_LIMIT,
        });
        let cleanup = temp.as_ref().map(|temp| remove_tree_writable(temp.path()));
        let process = result?;
        if let Some(Err(error)) = cleanup {
            eprintln!("cowbox mcp-proxy: warning: discovery cleanup failed: {error}");
        }
        let envelope: Option<Value> = if process.stdout_truncated {
            None
        } else {
            serde_json::from_slice(&process.stdout).ok()
        };
        log_child_stderr("sandbox", &process, envelope.is_none());
        let adapter = parse_adapter_output(
            envelope
                .as_ref()
                .and_then(|value| value.pointer("/data/stdout"))
                .and_then(Value::as_str),
            envelope
                .as_ref()
                .and_then(|value| value.pointer("/data/stdout_truncated"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            self.max_response_bytes,
        );
        Ok(SandboxExecution {
            process,
            envelope,
            adapter,
        })
    }

    fn run_cli(&self, args: &[OsString]) -> CliResult<CliExecution> {
        let process = process::run(process::ProcessSpec {
            program: self.executable.as_os_str().to_os_string(),
            args: args.to_vec(),
            cwd: self.workspace.clone(),
            env: Vec::new(),
            stdin: None,
            timeout: Some(CLIENT_QUERY_TIMEOUT),
            disconnect: Arc::clone(&self.disconnect),
            max_stdout: CHILD_STDOUT_LIMIT,
            max_stderr: CHILD_STDERR_LIMIT,
        })?;
        let envelope = if process.stdout_truncated {
            None
        } else {
            serde_json::from_slice(&process.stdout).ok()
        };
        log_child_stderr("control", &process, envelope.is_none());
        Ok(CliExecution { process, envelope })
    }

    fn tool_error_without_step(&self, error: &InvocationError) -> Value {
        tool_error(
            &error.code,
            &error.message,
            error.hint.clone(),
            base_metadata(&self.workspace, self.session.as_deref(), 0, None),
        )
    }

    fn emit(&self, value: Value) -> CliResult<bool> {
        let mut encoded = serde_json::to_vec(&value)?;
        if encoded.len() > self.max_response_bytes {
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            encoded = serde_json::to_vec(&error_response(
                id,
                INTERNAL_ERROR,
                format!(
                    "MCP response is {} bytes; configured limit is {}",
                    encoded.len(),
                    self.max_response_bytes
                ),
                Some(json!({ "code": "MCP_RESPONSE_TOO_LARGE" })),
            ))?;
        }
        encoded.push(b'\n');
        let mut stdout = std::io::stdout().lock();
        match stdout.write_all(&encoded).and_then(|()| stdout.flush()) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

struct CliExecution {
    process: process::ProcessOutput,
    envelope: Option<Value>,
}

#[derive(Debug)]
struct InvocationError {
    code: String,
    message: String,
    hint: Option<String>,
}

impl InvocationError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: None,
        }
    }

    fn from_cli(error: CliError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            hint: error.hint,
        }
    }
}

fn read_client_input(sender: mpsc::Sender<InputEvent>, disconnected: Arc<AtomicBool>) {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    loop {
        match read_line_limited(&mut reader, MAX_REQUEST_BYTES) {
            Ok(Some(line)) => {
                if sender.send(InputEvent::Line(line)).is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(LineError::TooLarge) => {
                if sender.send(InputEvent::Oversized).is_err() {
                    break;
                }
            }
            Err(LineError::Io(error)) => {
                eprintln!("cowbox mcp-proxy: stdin read failed: {error}");
                break;
            }
        }
    }
    disconnected.store(true, Ordering::Relaxed);
    let _ = sender.send(InputEvent::Closed);
}

#[derive(Debug)]
enum LineError {
    TooLarge,
    Io(std::io::Error),
}

fn read_line_limited(
    reader: &mut impl BufRead,
    limit: usize,
) -> Result<Option<Vec<u8>>, LineError> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf().map_err(LineError::Io)?;
        if available.is_empty() {
            if line.is_empty() && !oversized {
                return Ok(None);
            }
            return if oversized {
                Err(LineError::TooLarge)
            } else {
                Ok(Some(line))
            };
        }
        if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
            let take = position + 1;
            if !oversized {
                line.extend_from_slice(&available[..take]);
            }
            reader.consume(take);
            return if oversized || line.len() > limit {
                Err(LineError::TooLarge)
            } else {
                Ok(Some(line))
            };
        }
        if !oversized {
            line.extend_from_slice(available);
            if line.len() > limit {
                oversized = true;
                line.clear();
            }
        }
        let consumed = available.len();
        reader.consume(consumed);
    }
}

fn validate_upstream_tools(tools: &[Value]) -> Result<(), InvocationError> {
    let mut seen = std::collections::BTreeSet::new();
    for tool in tools {
        let Some(object) = tool.as_object() else {
            return Err(InvocationError::new(
                "MCP_UPSTREAM_PROTOCOL",
                "upstream tools/list contained a non-object tool",
            ));
        };
        let Some(name) = object.get("name").and_then(Value::as_str) else {
            return Err(InvocationError::new(
                "MCP_UPSTREAM_PROTOCOL",
                "upstream tool is missing a string name",
            ));
        };
        if name.is_empty() {
            return Err(InvocationError::new(
                "MCP_UPSTREAM_PROTOCOL",
                "upstream tool name cannot be empty",
            ));
        }
        if is_control_tool(name) {
            return Err(InvocationError::new(
                "MCP_TOOL_NAME_CONFLICT",
                format!("upstream tool {name:?} conflicts with a reserved cowbox control tool"),
            ));
        }
        if !seen.insert(name) {
            return Err(InvocationError::new(
                "MCP_TOOL_NAME_CONFLICT",
                format!("upstream advertised duplicate tool name {name:?}"),
            ));
        }
    }
    Ok(())
}

fn validate_control_arguments(name: &str, arguments: &Value) -> Result<(), String> {
    let Some(object) = arguments.as_object() else {
        return Err("arguments must be an object".to_string());
    };
    for key in object.keys() {
        if key != "step" {
            return Err(format!("{name} does not support argument {key:?}"));
        }
    }
    if let Some(step) = object.get("step") {
        if !step.is_u64() {
            return Err("step must be a non-negative integer".to_string());
        }
    }
    if name == "cowbox_rollback" && !object.contains_key("step") {
        return Err("cowbox_rollback requires step".to_string());
    }
    Ok(())
}

fn parse_adapter_output(
    stdout: Option<&str>,
    stdout_truncated: bool,
    max_response_bytes: usize,
) -> Option<AdapterOutput> {
    if stdout_truncated {
        eprintln!(
            "cowbox mcp-proxy: adapter stdout was truncated before the {max_response_bytes}-byte boundary"
        );
        return None;
    }
    let stdout = stdout?;
    if stdout.len() > max_response_bytes {
        return None;
    }
    serde_json::from_str(stdout.trim()).ok()
}

fn invocation_from_execution(
    execution: &SandboxExecution,
    fallback_code: &str,
    fallback_message: &str,
) -> InvocationError {
    if execution.process.timed_out {
        return InvocationError::new("MCP_DISCOVERY_TIMEOUT", "sandboxed adapter timed out");
    }
    if let Some(envelope) = &execution.envelope {
        if envelope["ok"].as_bool() == Some(false) {
            return InvocationError::new(
                envelope
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .unwrap_or(fallback_code),
                envelope
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or(fallback_message),
            );
        }
    }
    InvocationError::new(fallback_code, fallback_message)
}

fn session_from_envelope(envelope: &Value) -> Option<SessionSnapshot> {
    let session = envelope.get("session")?;
    Some(SessionSnapshot {
        id: session.get("id")?.as_str()?.to_string(),
        head: session.get("head")?.as_u64()?,
        status: session
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("active")
            .to_string(),
    })
}

fn base_metadata(
    workspace: &Path,
    session: Option<&str>,
    head_before: u64,
    step: Option<u64>,
) -> Value {
    json!({
        "session": session,
        "workspace": workspace,
        "head_before": head_before,
        "step": step,
        "snapshot_created": step.is_some(),
    })
}

fn snapshot_metadata(
    workspace: &Path,
    session: Option<&SessionSnapshot>,
    head_before: u64,
    step: Option<u64>,
    exit_code: Option<i32>,
    timed_out: bool,
) -> Value {
    let head_after = session.map(|session| session.head).unwrap_or(head_before);
    let rollback_command = step
        .filter(|step| *step > head_before)
        .map(|_| format!("cowbox rollback {head_before}"));
    let commit_command = step.map(|step| format!("cowbox commit --step {step}"));
    json!({
        "session": session.map(|session| session.id.clone()),
        "workspace": workspace,
        "status": session.map(|session| session.status.clone()),
        "head_before": head_before,
        "step": step,
        "head_after": head_after,
        "snapshot_created": step.is_some(),
        "exit_code": exit_code,
        "timed_out": timed_out,
        "rollback_command": rollback_command,
        "commit_command": commit_command,
    })
}

fn attach_cowbox_metadata(object: &mut Map<String, Value>, metadata: Value) {
    let meta = object
        .entry("_meta".to_string())
        .or_insert_with(|| json!({}));
    if !meta.is_object() {
        *meta = json!({});
    }
    meta["cowbox"] = metadata;
}

fn append_snapshot_content(
    content: Option<Value>,
    head_before: u64,
    step: Option<u64>,
    head_after: Option<u64>,
) -> Value {
    let mut blocks = content
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let snapshot = match step {
        Some(step) => format!(
            "[cowbox] snapshot step {step} created; head {head_before} -> {}. Inspect with cowbox_diff or use cowbox_rollback {} to discard it. Host commit requires terminal approval.",
            head_after.unwrap_or(step),
            head_before
        ),
        None => format!("[cowbox] no snapshot step was created; head remains {head_before}."),
    };
    blocks.push(json!({ "type": "text", "text": snapshot }));
    Value::Array(blocks)
}

fn tool_error(
    code: impl Into<String>,
    message: impl Into<String>,
    hint: Option<String>,
    metadata: Value,
) -> Value {
    let code = code.into();
    let message = message.into();
    let mut object = Map::new();
    let text = if let Some(hint) = &hint {
        format!("{code}: {message}\n{hint}")
    } else {
        format!("{code}: {message}")
    };
    object.insert(
        "content".to_string(),
        json!([{ "type": "text", "text": text }]),
    );
    object.insert("isError".to_string(), Value::Bool(true));
    attach_cowbox_metadata(&mut object, metadata);
    Value::Object(object)
}

fn append_stderr(hint: Option<String>, stderr: Option<String>) -> Option<String> {
    match (hint, stderr) {
        (Some(hint), Some(stderr)) => Some(format!("{hint}\nupstream stderr:\n{stderr}")),
        (Some(hint), None) => Some(hint),
        (None, Some(stderr)) => Some(format!("upstream stderr:\n{stderr}")),
        (None, None) => None,
    }
}

fn duration_ms(duration: Option<Duration>) -> u64 {
    duration
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn format_timeout(duration: Option<Duration>) -> String {
    match duration {
        Some(duration) => format!("{}s", duration.as_secs().max(1)),
        None => "0".to_string(),
    }
}

fn log_child_stderr(label: &str, process: &process::ProcessOutput, invalid_json: bool) {
    if process.stderr.is_empty() || (!invalid_json && process.exit_code == Some(0)) {
        return;
    }
    let text = String::from_utf8_lossy(&process.stderr);
    eprintln!(
        "cowbox mcp-proxy: {label} stderr{}{}",
        if process.stderr_truncated {
            " (truncated)"
        } else {
            ""
        },
        if text.trim().is_empty() {
            String::new()
        } else {
            format!(":\n{}", text.trim_end())
        }
    );
}

fn remove_tree_writable(path: &Path) -> std::io::Result<()> {
    fn chmod(path: &Path) -> std::io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                chmod(&entry?.path())?;
            }
        }
        Ok(())
    }
    if !path.exists() {
        return Ok(());
    }
    chmod(path)?;
    std::fs::remove_dir_all(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_reader_rejects_oversized_lines_without_consuming_the_next() {
        let mut input = std::io::Cursor::new(b"123456789\nok\n".to_vec());
        assert!(matches!(
            read_line_limited(&mut input, 4),
            Err(LineError::TooLarge)
        ));
        let next = read_line_limited(&mut input, 4)
            .expect("next line")
            .expect("some");
        assert_eq!(next, b"ok\n");
    }

    #[test]
    fn reserved_tool_collisions_are_rejected() {
        let error = validate_upstream_tools(&[json!({
            "name": "cowbox_diff",
            "inputSchema": { "type": "object" }
        })])
        .expect_err("conflict");
        assert_eq!(error.code, "MCP_TOOL_NAME_CONFLICT");
    }

    #[test]
    fn control_argument_validation_is_allowlisted() {
        assert!(validate_control_arguments("cowbox_diff", &json!({})).is_ok());
        assert!(validate_control_arguments("cowbox_rollback", &json!({})).is_err());
        assert!(validate_control_arguments("cowbox_diff", &json!({"command": "rm -rf"})).is_err());
    }
}
