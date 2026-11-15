use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

fn cowbox(home: &TempDir, project: &TempDir) -> Command {
    let mut command = Command::cargo_bin("cowbox").expect("cowbox binary");
    command
        .current_dir(project.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("APPDATA", home.path())
        .env_remove("XDG_CONFIG_HOME");
    command
}

fn json_output(command: &mut Command) -> Value {
    let output = command.output().expect("run cowbox");
    assert!(
        output.status.success(),
        "cowbox failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("valid JSON")
}

#[test]
fn install_skill_uses_cowbox_subdirectory_and_list_shows_all_harnesses() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    let value = json_output(cowbox(&home, &project).args([
        "install-skill",
        "--harness",
        "codex",
        "--mode",
        "ask",
        "--json",
    ]));
    assert_eq!(value["ok"].as_bool(), Some(true));
    let skill = home.path().join(".agents/skills/cowbox");
    assert!(skill.join("SKILL.md").is_file());
    assert!(skill.join("references/windows.md").is_file());
    assert!(skill.join(".cowbox-source").is_file());
    assert!(home.path().join(".codex/skills/cowbox/SKILL.md").is_file());

    let list = json_output(cowbox(&home, &project).args(["install-skill", "--list", "--json"]));
    let harnesses = list["data"]["harnesses"].as_array().expect("harness list");
    assert_eq!(harnesses.len(), 14);
    assert!(harnesses.iter().any(|item| item["id"] == "hermes"));
    assert!(harnesses.iter().any(|item| item["id"] == "kimi"));
}

#[test]
fn install_rules_is_idempotent_and_removable() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    let first = json_output(cowbox(&home, &project).args(["install-rules", "--json"]));
    assert_eq!(first["ok"].as_bool(), Some(true));
    let agents = project.path().join("AGENTS.md");
    let claude = project.path().join("CLAUDE.md");
    let cursor = project.path().join(".cursor/rules/cowbox.mdc");
    assert!(agents.is_file());
    assert!(claude.is_file());
    assert!(cursor.is_file());
    let text = fs::read_to_string(&agents).expect("agents rules");
    assert_eq!(text.matches("<!-- cowbox:begin -->").count(), 1);

    let second = json_output(cowbox(&home, &project).args(["install-rules", "--json"]));
    assert!(
        second["data"]["files"]
            .as_array()
            .expect("files")
            .iter()
            .all(|item| item["action"] == "unchanged")
    );

    let removed =
        json_output(cowbox(&home, &project).args(["install-rules", "--remove", "--json"]));
    assert_eq!(removed["ok"].as_bool(), Some(true));
    assert!(
        !fs::read_to_string(&agents)
            .expect("agents rules")
            .contains("<!-- cowbox:begin -->")
    );
    assert!(
        !fs::read_to_string(&cursor)
            .expect("cursor rules")
            .contains("<!-- cowbox:begin -->")
    );
}

#[test]
fn policy_defaults_to_ask_and_project_can_override() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    let value = json_output(cowbox(&home, &project).args(["policy", "--json"]));
    assert_eq!(value["data"]["mode"].as_str(), Some("ask"));
    assert_eq!(value["data"]["source"].as_str(), Some("global"));

    fs::write(
        project.path().join(".cowbox.toml"),
        "version = 1\n\n[agent]\nmode = \"explicit\"\nrequire_confirmation = true\n",
    )
    .expect("project config");
    let value = json_output(cowbox(&home, &project).args(["policy", "--json"]));
    assert_eq!(value["data"]["mode"].as_str(), Some("explicit"));
    assert_eq!(value["data"]["source"].as_str(), Some("project"));
}

#[test]
fn init_agent_writes_config_and_rules() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    let value = json_output(cowbox(&home, &project).args(["init", "--agent", "--json"]));
    assert_eq!(value["ok"].as_bool(), Some(true));
    assert!(project.path().join(".cowbox.toml").is_file());
    assert!(project.path().join("AGENTS.md").is_file());
    assert!(project.path().join("CLAUDE.md").is_file());
    assert!(project.path().join(".cursor/rules/cowbox.mdc").is_file());
}

#[test]
fn setup_is_windows_only_on_linux() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    let output = cowbox(&home, &project)
        .args(["setup", "--json"])
        .output()
        .expect("run setup");
    if cfg!(windows) {
        return;
    }
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("json error");
    assert_eq!(value["error"]["code"].as_str(), Some("WINDOWS_ONLY"));
}
