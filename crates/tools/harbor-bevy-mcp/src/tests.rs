use super::*;
use rmcp::ServerHandler;
use std::fs;

#[test]
fn artifact_reads_are_filtered_bounded_and_rooted() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path().join("artifacts");
    fs::create_dir(&root).unwrap();
    fs::write(repo.path().join("private"), "credential").unwrap();
    fs::write(
        root.join("run.log"),
        "start\nINFO alive\nERROR wait\nerror again\nend",
    )
    .unwrap();
    fs::write(root.join("summary.json"), r#"{"status":"ok"}"#).unwrap();
    let tools = DevTools::new(&root).unwrap();
    let log = |path: &str, start, end| FileRange {
        path: path.into(),
        start,
        end,
    };
    let result = tools
        .read_ranges(&[log("run.log", 2, 4)], Some("(?i)error"))
        .unwrap();
    assert_eq!(
        result[0]
            .lines
            .iter()
            .map(|line| line.number)
            .collect::<Vec<_>>(),
        [3, 4]
    );
    assert!(tools.read_ranges(&[log("../private", 1, 2)], None).is_err());
    assert!(
        tools
            .read_ranges(&[log("run.log", 1, u64::MAX)], None)
            .is_err()
    );
    assert!(
        tools
            .inspect_report(Path::new("summary.json"), None)
            .unwrap()
            .report
            .is_some()
    );
    let expected = "00".repeat(32);
    let report = tools
        .inspect_report(Path::new("summary.json"), Some(&expected))
        .unwrap();
    assert_eq!(report.hash_matches, Some(false));
    fs::write(root.join("plain.log"), "abc").unwrap();
    assert_eq!(
        tools
            .inspect_report(Path::new("plain.log"), None)
            .unwrap()
            .sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        tools
            .inspect_report(Path::new("summary.json"), Some(&report.sha256))
            .unwrap()
            .hash_matches,
        Some(true)
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(repo.path().join("private"), root.join("link")).unwrap();
        assert!(tools.read_ranges(&[log("link", 1, 2)], None).is_err());
    }
}

#[cfg(unix)]
#[test]
fn xtask_forwards_literal_arguments() {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("sample task");
    fs::write(&program, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let runner = XtaskRunner::new(root.path(), &program).unwrap();
    let output = runner.run(["a b", "$(exit 9)"]).unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"a b\n$(exit 9)\n");
}

#[test]
fn default_layer_composes_playtest_and_inspection() {
    struct Game;
    impl Playtest for Game {
        fn start_session(&self, _: Value) -> Result<Value> {
            Ok(serde_json::json!({"session_id": "demo"}))
        }
        fn get_state(&self, _: &str) -> Result<Value> {
            Ok(Value::Null)
        }
        fn execute_action(&self, _: &str, _: Value) -> Result<Value> {
            Ok(Value::Null)
        }
        fn stop_session(&self, _: &str) -> Result<Value> {
            Ok(Value::Null)
        }
    }
    let root = tempfile::tempdir().unwrap();
    let server = layered(DevTools::new(root.path()).unwrap(), Some(Arc::new(Game)));
    for name in [
        "read_ranges",
        "inspect_report",
        "start_session",
        "get_state",
        "execute_action",
        "stop_session",
    ] {
        assert!(server.get_tool(name).is_some(), "missing {name}");
    }
    assert!(server.get_tool("android_doctor").is_none());
}
