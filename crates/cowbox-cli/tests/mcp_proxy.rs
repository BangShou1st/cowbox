#![cfg(target_os = "linux")]

use assert_cmd::Command;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    workspace: std::path::PathBuf,
    state: std::path::PathBuf,
    server: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("fixture");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(workspace.join(".git")).expect("git marker");
        std::fs::write(workspace.join("base.txt"), "base\n").expect("base");
        let server = workspace.join("mcp_upstream.py");
        std::fs::write(&server, include_str!("fixtures/mcp_upstream.py")).expect("fixture server");
        Self {
            _root: root,
            workspace,
            state,
            server,
        }
    }

    fn client(&self, extra: &[&str], server_args: &[&str]) -> Client {
        let binary = assert_cmd::cargo::cargo_bin("cowbox");
        let mut command = std::process::Command::new(binary);
        command
            .current_dir(&self.workspace)
            .env("COWBOX_HOME", &self.state)
            .arg("mcp-proxy")
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--network")
            .arg("off")
            .args(extra)
            .arg("--")
            .arg("python3")
            .arg(&self.server)
            .args(server_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Client::start(command)
    }
}

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<Vec<u8>>,
    stderr: Arc<Mutex<String>>,
    next_id: i64,
}

impl Client {
    fn start(mut command: std::process::Command) -> Self {
        let mut child = command.spawn().expect("start mcp-proxy");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if !line.trim().is_empty() && sender.send(line.into_bytes()).is_err() {
                    break;
                }
            }
        });
        let captured = Arc::new(Mutex::new(String::new()));
        let stderr_copy = Arc::clone(&captured);
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                stderr_copy
                    .lock()
                    .expect("stderr lock")
                    .push_str(&format!("{line}\n"));
            }
        });
        Self {
            child,
            stdin,
            lines: receiver,
            stderr: captured,
            next_id: 1,
        }
    }

    fn initialize(&mut self) {
        let value = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "cowbox-test", "version": "1" }
            }),
        );
        assert_eq!(
            value["result"]["protocolVersion"].as_str(),
            Some("2025-06-18")
        );
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
        let ping = self.request("ping", Value::Null);
        assert_eq!(ping["result"], json!({}));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.request_with_id(json!(id), method, params)
    }

    fn request_with_id(&mut self, id: Value, method: &str, params: Value) -> Value {
        let mut message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
        });
        if !params.is_null() {
            message["params"] = params;
        }
        self.send(message);
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(180))
            .unwrap_or_else(|_| panic!("timed out waiting for {method}; stderr={}", self.stderr()));
        let value: Value = serde_json::from_slice(&line).expect("JSON-RPC response");
        assert_eq!(value["jsonrpc"].as_str(), Some("2.0"));
        assert_eq!(value["id"], id);
        value
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let value = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        assert!(value.get("error").is_none(), "tool protocol error: {value}");
        value["result"].clone()
    }

    fn send(&mut self, value: Value) {
        let stdin = self.stdin.as_mut().expect("stdin");
        serde_json::to_writer(&mut *stdin, &value).expect("write request");
        stdin.write_all(b"\n").expect("newline");
        stdin.flush().expect("flush");
    }

    fn close_input(&mut self) {
        drop(self.stdin.take());
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("wait") {
                return status.code();
            }
            thread::sleep(Duration::from_millis(20));
        }
        None
    }

    fn stderr(&self) -> String {
        self.stderr.lock().expect("stderr lock").clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn require_sandbox() -> bool {
    if std::env::var("COWBOX_REQUIRE_SANDBOX").as_deref() != Ok("1") {
        eprintln!("skipping MCP sandbox test; set COWBOX_REQUIRE_SANDBOX=1");
        return false;
    }
    let fixture = Fixture::new();
    let output = Command::cargo_bin("cowbox")
        .expect("binary")
        .current_dir(&fixture.workspace)
        .env("COWBOX_HOME", &fixture.state)
        .args(["doctor", "--json"])
        .output()
        .expect("doctor");
    assert!(
        output.status.success(),
        "sandbox required but doctor failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    drop(fixture);
    true
}

fn content_text(result: &Value) -> String {
    result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn complete_snapshot_rollback_and_preview_workflow() {
    if !require_sandbox() {
        return;
    }
    let fixture = Fixture::new();
    let mut client = fixture.client(&[], &[]);
    client.initialize();
    let tools = client.request_with_id(json!("tools-1"), "tools/list", json!({}));
    let names = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    for expected in [
        "write_file",
        "cowbox_diff",
        "cowbox_rollback",
        "cowbox_commit",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {tools}"
        );
    }

    let first = client.call_tool(
        "write_file",
        json!({ "path": "wrong.txt", "content": "WRONG\n" }),
    );
    let first_meta = &first["_meta"]["cowbox"];
    assert_eq!(first_meta["head_before"].as_u64(), Some(0));
    assert_eq!(first_meta["step"].as_u64(), Some(1));
    assert_eq!(first_meta["head_after"].as_u64(), Some(1));

    let read = client.call_tool("read_file", json!({ "path": "wrong.txt" }));
    assert!(content_text(&read).contains("WRONG"));
    assert_eq!(read["_meta"]["cowbox"]["head_before"].as_u64(), Some(1));
    assert_eq!(read["_meta"]["cowbox"]["step"].as_u64(), Some(2));
    assert!(!fixture.workspace.join("wrong.txt").exists());

    let rollback = client.call_tool("cowbox_rollback", json!({ "step": 0 }));
    assert_eq!(rollback["structuredContent"]["head"].as_u64(), Some(0));
    let rewritten = client.call_tool(
        "write_file",
        json!({ "path": "wrong.txt", "content": "RIGHT\n" }),
    );
    assert_eq!(
        rewritten["_meta"]["cowbox"]["head_before"].as_u64(),
        Some(0)
    );
    assert_eq!(rewritten["_meta"]["cowbox"]["step"].as_u64(), Some(3));
    let diff = client.call_tool("cowbox_diff", json!({}));
    let changes = diff["structuredContent"]["changes"]
        .as_array()
        .expect("changes");
    assert!(changes.iter().any(|change| {
        change["rel_path"].as_str() == Some("wrong.txt") && change["kind"].as_str() == Some("added")
    }));
    let preview = client.call_tool("cowbox_commit", json!({ "step": 3 }));
    assert_eq!(preview["structuredContent"]["committed"], false);
    assert_eq!(
        preview["structuredContent"]["target_step"].as_u64(),
        Some(3)
    );
    assert_eq!(
        preview["structuredContent"]["terminal_command"].as_str(),
        Some("cowbox commit --step 3")
    );
    assert!(!fixture.workspace.join("wrong.txt").exists());

    let binary = assert_cmd::cargo::cargo_bin("cowbox");
    let commit = std::process::Command::new(binary)
        .current_dir(&fixture.workspace)
        .env("COWBOX_HOME", &fixture.state)
        .args(["commit", "--step", "3", "--yes", "--json"])
        .output()
        .expect("commit");
    assert!(
        commit.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("wrong.txt")).expect("host file"),
        "RIGHT\n"
    );
}

#[test]
fn pagination_errors_limits_and_network_off_are_reported() {
    if !require_sandbox() {
        return;
    }
    let fixture = Fixture::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let port = listener.local_addr().expect("address").port();
    let mut client = fixture.client(&["--max-response-bytes", "4096"], &["--page-size", "2"]);
    client.initialize();
    let tools = client.request("tools/list", json!({}));
    assert!(tools["result"]["tools"].as_array().expect("tools").len() >= 10);

    let failure = client.call_tool("fail_tool", json!({}));
    assert_eq!(failure["isError"], true);
    assert_eq!(failure["_meta"]["cowbox"]["step"].as_u64(), Some(1));
    let rpc = client.call_tool("rpc_error", json!({}));
    assert_eq!(rpc["isError"], true);
    assert!(content_text(&rpc).contains("requested RPC failure"));
    assert_eq!(rpc["_meta"]["cowbox"]["step"].as_u64(), Some(2));
    let exited = client.call_tool("exit_tool", json!({}));
    assert_eq!(exited["isError"], true);
    assert!(content_text(&exited).contains("upstream"));
    assert_eq!(exited["_meta"]["cowbox"]["step"].as_u64(), Some(3));
    let large = client.call_tool("large_tool", json!({ "bytes": 20000 }));
    assert_eq!(large["isError"], true);
    assert!(content_text(&large).contains("TOO_LARGE"));
    assert_eq!(large["_meta"]["cowbox"]["step"].as_u64(), Some(4));
    let network = client.call_tool("connect_tcp", json!({ "host": "127.0.0.1", "port": port }));
    assert_eq!(
        network["isError"], true,
        "network isolation failed: {network}"
    );
    assert!(content_text(&network).contains("connection failed"));
    drop(listener);
}

#[test]
fn timeout_keeps_a_rollbackable_step_and_disconnect_stops_the_proxy() {
    if !require_sandbox() {
        return;
    }
    let fixture = Fixture::new();
    let mut client = fixture.client(&["--timeout", "3s"], &[]);
    client.initialize();
    client.request("tools/list", json!({}));
    let timeout = client.call_tool("hang_tool", json!({}));
    assert_eq!(timeout["isError"], true);
    assert_eq!(timeout["_meta"]["cowbox"]["step"].as_u64(), Some(1));
    assert!(timeout["_meta"]["cowbox"]["rollback_command"].is_string());

    let id = client.next_id;
    client.send(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": "hang_tool", "arguments": {} }
    }));
    client.close_input();
    let code = client.wait_for_exit(Duration::from_secs(15));
    assert_eq!(code, Some(0), "disconnect did not stop the proxy");
}

#[test]
fn reserved_tool_conflicts_fail_discovery() {
    if !require_sandbox() {
        return;
    }
    let fixture = Fixture::new();
    let mut client = fixture.client(&[], &["--conflict"]);
    client.initialize();
    let tools = client.request("tools/list", json!({}));
    assert_eq!(tools["error"]["data"]["code"], "MCP_TOOL_NAME_CONFLICT");
}
