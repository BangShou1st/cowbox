use super::protocol::{
    AdapterOutput, AdapterRequest, MAX_LIST_PAGES, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
    MAX_TOOLS, MAX_UPSTREAM_STDERR, METHOD_NOT_FOUND, UPSTREAM_PROTOCOL_VERSION, error_response,
};
use crate::cli::McpAdapterArgs;
use crate::error::CliResult;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

struct AdapterFailure {
    output: AdapterOutput,
    exit_code: i32,
}

struct UpstreamSession {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<Result<Vec<u8>, String>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    deadline: Option<Instant>,
    next_id: u64,
}

impl UpstreamSession {
    fn start(
        workspace: &Path,
        command: &[String],
        timeout_ms: u64,
        max_line: usize,
    ) -> Result<Self, Box<AdapterFailure>> {
        let Some(program) = command.first() else {
            return Err(adapter_failure(
                "MCP_UPSTREAM_INVALID",
                "upstream command cannot be empty",
                2,
            ));
        };
        let merged_root = std::env::current_dir().map_err(|error| {
            adapter_failure(
                "MCP_ADAPTER_CWD_FAILED",
                format!("cannot determine the sandbox workspace: {error}"),
                1,
            )
        })?;
        let program = map_workspace_path(workspace, &merged_root, program);
        let arguments = command[1..]
            .iter()
            .map(|value| map_workspace_path(workspace, &merged_root, value))
            .collect::<Vec<_>>();
        let mut child = Command::new(&program)
            .args(&arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                adapter_failure(
                    "MCP_UPSTREAM_START_FAILED",
                    format!("cannot start upstream MCP server {program}: {error}"),
                    127,
                )
            })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| {
            adapter_failure("MCP_UPSTREAM_START_FAILED", "stdout is unavailable", 127)
        })?;
        let stderr_pipe = child.stderr.take().ok_or_else(|| {
            adapter_failure("MCP_UPSTREAM_START_FAILED", "stderr is unavailable", 127)
        })?;

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut oversized = false;
            loop {
                let mut line = Vec::new();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) => {
                        let _ = sender.send(Err("upstream closed stdout".to_string()));
                        break;
                    }
                    Ok(_) => {
                        if line.len() > max_line {
                            if !oversized {
                                let _ = sender.send(Err(format!(
                                    "upstream response exceeded {max_line} bytes"
                                )));
                                oversized = true;
                            }
                            continue;
                        }
                        if sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(format!("cannot read upstream stdout: {error}")));
                        break;
                    }
                }
            }
        });

        let stderr = Arc::new(Mutex::new(Vec::new()));
        let stderr_copy = Arc::clone(&stderr);
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr_pipe);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let text = line.trim_end();
                        if !text.is_empty() {
                            eprintln!("[upstream] {text}");
                            if let Ok(mut captured) = stderr_copy.lock() {
                                let remaining = MAX_UPSTREAM_STDERR.saturating_sub(captured.len());
                                captured.extend_from_slice(
                                    &text.as_bytes()[..text.len().min(remaining)],
                                );
                                if remaining < text.len() {
                                    captured.extend_from_slice(b"\n[truncated]\n");
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            lines: receiver,
            stderr,
            deadline: (timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(timeout_ms)),
            next_id: 1,
        })
    }

    fn send(&mut self, value: &Value) -> Result<(), Box<AdapterFailure>> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            adapter_failure("MCP_UPSTREAM_CLOSED", "upstream stdin is already closed", 1)
        })?;
        let mut encoded = serde_json::to_vec(value).map_err(|error| {
            adapter_failure(
                "MCP_UPSTREAM_PROTOCOL",
                format!("cannot serialize upstream request: {error}"),
                1,
            )
        })?;
        encoded.push(b'\n');
        stdin.write_all(&encoded).map_err(|error| {
            adapter_failure(
                "MCP_UPSTREAM_CLOSED",
                format!("cannot write to upstream stdin: {error}"),
                1,
            )
        })?;
        stdin.flush().map_err(|error| {
            adapter_failure(
                "MCP_UPSTREAM_CLOSED",
                format!("cannot flush upstream stdin: {error}"),
                1,
            )
        })
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, Box<AdapterFailure>> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send(&request)?;
        self.read_response(id)
    }

    fn notify(&mut self, method: &str) -> Result<(), Box<AdapterFailure>> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
        }))
    }

    fn read_response(&mut self, expected_id: u64) -> Result<Value, Box<AdapterFailure>> {
        loop {
            let remaining = match self.deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(self.timeout_failure());
                    }
                    deadline.duration_since(now)
                }
                None => Duration::from_secs(24 * 60 * 60),
            };
            let line = match self.lines.recv_timeout(remaining) {
                Ok(Ok(line)) => line,
                Ok(Err(message)) => {
                    if message.contains("exceeded") {
                        return Err(self.failure_with_stderr("MCP_RESPONSE_TOO_LARGE", message, 1));
                    }
                    return Err(self.failure_with_stderr("MCP_UPSTREAM_PROTOCOL", message, 1));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(self.timeout_failure()),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.failure_with_stderr(
                        "MCP_UPSTREAM_PROTOCOL",
                        "upstream stdout reader stopped unexpectedly",
                        1,
                    ));
                }
            };
            let trimmed = line.strip_suffix(b"\n").unwrap_or(&line);
            let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_slice(trimmed) {
                Ok(value) => value,
                Err(error) => {
                    eprintln!(
                        "[upstream] ignoring non-JSON stdout: {} ({error})",
                        String::from_utf8_lossy(trimmed)
                    );
                    continue;
                }
            };
            if value.get("method").and_then(Value::as_str).is_some() {
                if let Some(id) = value.get("id").cloned() {
                    let _ = self.send(&error_response(
                        id,
                        METHOD_NOT_FOUND,
                        "the cowbox MCP adapter does not implement upstream client requests",
                        None,
                    ));
                }
                continue;
            }
            if value.get("id").and_then(Value::as_u64) == Some(expected_id) {
                return Ok(value);
            }
            eprintln!(
                "[upstream] ignoring response for unexpected id: {}",
                value.get("id").cloned().unwrap_or(Value::Null)
            );
        }
    }

    fn timeout_failure(&self) -> Box<AdapterFailure> {
        self.failure_with_stderr(
            "MCP_UPSTREAM_TIMEOUT",
            "upstream MCP server did not answer before the tool-call timeout",
            124,
        )
    }

    fn failure_with_stderr(
        &self,
        code: &str,
        message: impl Into<String>,
        exit_code: i32,
    ) -> Box<AdapterFailure> {
        let mut failure = adapter_failure(code, message, exit_code);
        let stderr = self
            .stderr
            .lock()
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
            .filter(|text| !text.is_empty());
        if let Some(output) = failure.output.error.as_mut() {
            output.upstream_stderr = stderr;
        }
        failure
    }

    fn finish(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for UpstreamSession {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn main(args: &McpAdapterArgs) -> CliResult<i32> {
    let mut input = Vec::new();
    std::io::stdin()
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut input)?;
    if input.len() > MAX_REQUEST_BYTES {
        return emit(
            AdapterOutput::failure(
                "MCP_REQUEST_TOO_LARGE",
                format!("adapter request exceeded {MAX_REQUEST_BYTES} bytes"),
            ),
            1,
            args.max_response_bytes,
        );
    }
    let request: AdapterRequest = match serde_json::from_slice(&input) {
        Ok(request) => request,
        Err(error) => {
            return emit(
                AdapterOutput::failure(
                    "MCP_ADAPTER_REQUEST_INVALID",
                    format!("cannot parse adapter request: {error}"),
                ),
                1,
                args.max_response_bytes,
            );
        }
    };
    if args.max_response_bytes == 0 || args.max_response_bytes > MAX_RESPONSE_BYTES {
        return emit(
            AdapterOutput::failure(
                "MCP_RESPONSE_LIMIT_INVALID",
                format!("adapter response limit must be between 1 and {MAX_RESPONSE_BYTES} bytes"),
            ),
            2,
            MAX_RESPONSE_BYTES,
        );
    }
    if request.max_response_bytes == 0 || request.max_response_bytes > MAX_RESPONSE_BYTES {
        return emit(
            AdapterOutput::failure(
                "MCP_RESPONSE_LIMIT_INVALID",
                format!("request response limit must be between 1 and {MAX_RESPONSE_BYTES} bytes"),
            ),
            2,
            args.max_response_bytes,
        );
    }
    if request.upstream.is_empty() {
        return emit(
            AdapterOutput::failure("MCP_UPSTREAM_INVALID", "upstream command cannot be empty"),
            2,
            args.max_response_bytes.min(request.max_response_bytes),
        );
    }

    let max_line = args.max_response_bytes.min(request.max_response_bytes);
    let (output, exit_code) = match drive(&request, max_line) {
        Ok(output) => (output, 0),
        Err(failure) => (failure.output, failure.exit_code),
    };
    emit(output, exit_code, max_line)
}

fn drive(request: &AdapterRequest, max_line: usize) -> Result<AdapterOutput, Box<AdapterFailure>> {
    let mut upstream = UpstreamSession::start(
        &request.workspace,
        &request.upstream,
        request.timeout_ms,
        max_line,
    )?;
    let initialize = upstream.request(
        "initialize",
        json!({
            "protocolVersion": UPSTREAM_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "cowbox-mcp-proxy",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }),
    )?;
    if let Some(error) = initialize.get("error") {
        return Err(failure_from_rpc(
            "MCP_UPSTREAM_INITIALIZE_FAILED",
            "upstream rejected initialize",
            error,
            &upstream,
        ));
    }
    let initialize_result = initialize.get("result").cloned().ok_or_else(|| {
        upstream.failure_with_stderr(
            "MCP_UPSTREAM_PROTOCOL",
            "initialize response is missing result",
            1,
        )
    })?;
    upstream.notify("notifications/initialized")?;

    let output = match request.mode {
        super::protocol::AdapterMode::List => {
            let (tools, server_info) = discover(&mut upstream, initialize_result)?;
            AdapterOutput::tools(
                tools,
                initialize
                    .pointer("/result/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(UPSTREAM_PROTOCOL_VERSION),
                server_info,
            )
        }
        super::protocol::AdapterMode::Call => {
            let Some(tool) = request.tool.as_deref() else {
                return Err(upstream.failure_with_stderr(
                    "MCP_TOOL_MISSING",
                    "tools/call adapter request is missing a tool name",
                    1,
                ));
            };
            if !request.arguments.is_object() {
                return Err(upstream.failure_with_stderr(
                    "MCP_ARGUMENTS_INVALID",
                    "tools/call arguments must be a JSON object",
                    1,
                ));
            }
            let rpc = upstream.request(
                "tools/call",
                json!({
                    "name": tool,
                    "arguments": request.arguments,
                }),
            )?;
            AdapterOutput::call(rpc)
        }
    };
    upstream.finish();
    Ok(output)
}

fn discover(
    upstream: &mut UpstreamSession,
    initialize_result: Value,
) -> Result<(Vec<Value>, Value), Box<AdapterFailure>> {
    let server_info = initialize_result
        .get("serverInfo")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = BTreeSet::new();
    for page in 0..MAX_LIST_PAGES {
        let params = cursor
            .as_ref()
            .map(|cursor| json!({ "cursor": cursor }))
            .unwrap_or_else(|| json!({}));
        let response = upstream.request("tools/list", params)?;
        if let Some(error) = response.get("error") {
            return Err(failure_from_rpc(
                "MCP_UPSTREAM_LIST_FAILED",
                "upstream rejected tools/list",
                error,
                upstream,
            ));
        }
        let page_tools = response
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                upstream.failure_with_stderr(
                    "MCP_UPSTREAM_PROTOCOL",
                    "tools/list response is missing result.tools",
                    1,
                )
            })?;
        tools.extend(page_tools.iter().cloned());
        if tools.len() > MAX_TOOLS {
            return Err(upstream.failure_with_stderr(
                "MCP_TOOL_LIMIT",
                format!("upstream advertised more than {MAX_TOOLS} tools"),
                1,
            ));
        }
        let next = response
            .pointer("/result/nextCursor")
            .and_then(Value::as_str)
            .filter(|cursor| !cursor.is_empty());
        let Some(next) = next else {
            return Ok((tools, server_info));
        };
        if !seen_cursors.insert(next.to_string()) {
            return Err(upstream.failure_with_stderr(
                "MCP_UPSTREAM_PROTOCOL",
                format!("tools/list repeated cursor {next:?}"),
                1,
            ));
        }
        cursor = Some(next.to_string());
        if page + 1 == MAX_LIST_PAGES {
            return Err(upstream.failure_with_stderr(
                "MCP_UPSTREAM_PROTOCOL",
                format!("tools/list exceeded {MAX_LIST_PAGES} pages"),
                1,
            ));
        }
    }
    unreachable!()
}

fn failure_from_rpc(
    code: &str,
    message: &str,
    error: &Value,
    upstream: &UpstreamSession,
) -> Box<AdapterFailure> {
    let detail = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown upstream error");
    upstream.failure_with_stderr(code, format!("{message}: {detail}"), 1)
}

fn map_workspace_path(workspace: &Path, merged_root: &Path, value: &str) -> String {
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return value.to_string();
    }
    let Ok(relative) = path.strip_prefix(workspace) else {
        return value.to_string();
    };
    merged_root.join(relative).to_string_lossy().into_owned()
}

fn adapter_failure(code: &str, message: impl Into<String>, exit_code: i32) -> Box<AdapterFailure> {
    Box::new(AdapterFailure {
        output: AdapterOutput::failure(code, message),
        exit_code,
    })
}

fn emit(output: AdapterOutput, exit_code: i32, max_line: usize) -> CliResult<i32> {
    let encoded = serde_json::to_vec(&output)?;
    if encoded.len() > max_line {
        let failure = AdapterOutput::failure(
            "MCP_RESPONSE_TOO_LARGE",
            format!(
                "adapter response is {} bytes; limit is {max_line}",
                encoded.len()
            ),
        );
        let fallback = serde_json::to_vec(&failure)?;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&fallback)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        return Ok(1);
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&encoded)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_upstream_command() {
        let failure = adapter_failure("MCP_UPSTREAM_INVALID", "empty", 2);
        assert_eq!(failure.exit_code, 2);
        assert_eq!(
            failure.output.error.expect("failure").code,
            "MCP_UPSTREAM_INVALID"
        );
    }

    #[test]
    fn maps_workspace_command_paths_into_the_merged_mount() {
        assert_eq!(
            map_workspace_path(
                Path::new("/project"),
                Path::new("/state/merged"),
                "/project/tools/server.py"
            ),
            "/state/merged/tools/server.py"
        );
        assert_eq!(
            map_workspace_path(Path::new("/project"), Path::new("/state/merged"), "python3"),
            "python3"
        );
    }
}
