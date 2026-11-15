use crate::error::{CliError, CliResult};
use crate::state::SessionRecord;
use serde_json::{Value, json};

pub const SCHEMA_VERSION: u32 = 1;

pub fn session_json(record: &SessionRecord) -> Value {
    json!({
        "id": record.session_id,
        "workspace": record.workspace,
        "status": record.status,
        "head": record.dag.head_step,
        "created_at_unix_ms": record.created_at_unix_ms,
        "updated_at_unix_ms": record.updated_at_unix_ms,
        "rollback_count": record.rollback_count,
    })
}

pub fn ok(command: &str, record: Option<&SessionRecord>, data: Value) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "ok": true,
        "command": command,
        "session": record.map(session_json),
        "data": data,
    })
}

pub fn error(command: &str, error: &CliError) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "ok": false,
        "command": command,
        "error": {
            "code": error.code,
            "message": error.message,
            "hint": error.hint,
            "exit_code": error.exit_code,
        }
    })
}

pub fn print(value: &Value) -> CliResult<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
