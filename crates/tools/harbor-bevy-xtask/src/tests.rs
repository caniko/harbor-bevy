use super::*;
use serde_json::json;
use std::{fs, process::Command};

#[test]
fn project_config_loads_from_root() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("harbor-bevy.toml"),
        r#"
app_name = "Sample"
binary_name = "sample-game"
platforms = ["linux", "android"]
denylist_patterns = ["internal[-_]name"]
[paths]
license_classes = "policy/classes.toml"
[bevy]
mode = "veritasium"
package_list = "engine.txt"
[android_flavors.demo]
gradle_module = ":demo"
[steam]
app_id = 22
[steam.depots]
linux = 23
[test_suites.smoke]
args = ["test", "--locked"]
[gameplay]
config = "peer.toml"
[cluster]
config = "cluster.toml"
"#,
    )
    .unwrap();
    let cfg = ProjectConfig::load(root.path()).unwrap();
    assert_eq!(cfg.app_name, "Sample");
    assert_eq!(cfg.binary_name, "sample-game");
    assert_eq!(cfg.bevy.mode, BevyMode::Veritasium);
    assert_eq!(
        cfg.paths.license_classes.to_str(),
        Some("policy/classes.toml")
    );
    assert_eq!(cfg.steam.unwrap().depots["linux"], 23);
    assert_eq!(cfg.platforms, ["linux", "android"]);
    assert!(cfg.android_flavors.contains_key("demo"));
    assert!(cfg.test_suites.contains_key("smoke"));
    assert_eq!(cfg.gameplay["config"].as_str(), Some("peer.toml"));
    assert_eq!(cfg.cluster["config"].as_str(), Some("cluster.toml"));
}

#[test]
fn consumer_hooks_receive_literal_cli_arguments() {
    struct Hooks;
    impl ProjectHooks for Hooks {
        fn run(&self, project: &Project<'_>, command: ProjectCommand) -> Result<()> {
            assert!(project.root.is_absolute());
            assert_eq!(project.config.binary_name, "sample-game");
            let ProjectCommand::Build { args } = command else {
                panic!("expected Build");
            };
            assert_eq!(args, ["--out", "a b"]);
            Ok(())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("harbor-bevy.toml");
    fs::write(
        &config,
        "app_name = 'Sample'\nbinary_name = 'sample-game'\n",
    )
    .unwrap();
    let cli = KitCli::try_parse_from(["xtask", "build", "--out", "a b"]).unwrap();
    cli.command.run(root.path(), &Hooks).unwrap();
    assert!(matches!(
        KitCli::try_parse_from(["xtask", "doctor", "patches"])
            .unwrap()
            .command,
        KitCommand::Doctor {
            command: DoctorCommand::Patches
        }
    ));
    fs::write(config, "app_name = 'Sample'\nbinary_name = '../escape'\n").unwrap();
    assert!(ProjectConfig::load(root.path()).is_err());
}

#[test]
fn fork_needs_all_patches_including_the_hyphenated_name() {
    let root = tempfile::tempdir().unwrap();
    let names = doctor::ENGINE_LIST.lines().collect::<Vec<_>>();
    assert_eq!(names.len(), 74);
    let source = "https://example.invalid/engine";
    let rev = "0123456789012345678901234567890123456789";
    let patches = |skip: &str| {
        format!(
            "[patch.crates-io]\n{}",
            names
                .iter()
                .filter(|&&name| name != skip)
                .map(|name| format!("{name} = {{ git = '{source}', rev = '{rev}' }}\n"))
                .collect::<String>()
        )
    };
    let packages = names
        .iter()
        .map(|name| {
            json!({
                "name": name, "version": "0.20.1", "id": name,
                "source": format!("git+{source}?rev={rev}#{rev}"),
            })
        })
        .collect::<Vec<_>>();
    let metadata = json!({
        "packages": packages,
        "resolve": { "nodes": names.iter().map(|name| json!({"id": name})).collect::<Vec<_>>() }
    });
    let verify = |manifest: &str| {
        doctor::validate(
            root.path(),
            BevyMode::Veritasium,
            doctor::ENGINE_LIST,
            manifest,
            &metadata.to_string(),
        )
    };
    assert!(
        verify(&patches("bevy-settings"))
            .unwrap_err()
            .to_string()
            .contains("bevy-settings")
    );
    verify(&patches("")).unwrap();
}

#[test]
fn stock_rejects_forks_wrong_versions_and_duplicate_replication() {
    let root = tempfile::tempdir().unwrap();
    let stock = json!({
        "name": "bevy", "version": "0.20.1", "id": "bevy",
        "source": "registry+https://github.com/rust-lang/crates.io-index",
    });
    let replication = |name: &str, id: &str| json!({"name": name, "version": "0.1.0", "id": id});
    let verify = |packages: Vec<serde_json::Value>| {
        let nodes: Vec<_> = packages
            .iter()
            .map(|pkg| json!({"id": pkg["id"]}))
            .collect();
        doctor::validate(
            root.path(),
            BevyMode::Stock,
            doctor::ENGINE_LIST,
            "",
            &json!({"packages": packages, "resolve": {"nodes": nodes}}).to_string(),
        )
    };
    let mut bad = stock.clone();
    bad["version"] = json!("0.20.0");
    assert!(verify(vec![bad]).is_err());
    let mut fork = stock.clone();
    fork["source"] = json!("git+https://example.invalid/engine#deadbeef");
    assert!(verify(vec![fork]).is_err());
    for name in [
        "veritasium_wire",
        "veritasium_replicated",
        "veritasium_replicated_macros",
    ] {
        assert!(
            verify(vec![
                stock.clone(),
                replication(name, "a"),
                replication(name, "b")
            ])
            .is_err()
        );
    }
    verify(vec![stock, replication("veritasium_wire", "a")]).unwrap();
}

#[test]
fn denylist_scans_tracked_and_unignored_text() {
    let root = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q"]);
    fs::write(root.path().join(".gitignore"), "ignored.txt\n").unwrap();
    for (path, contents) in [
        ("tracked.rs", "first\nInternal-Name\n"),
        ("new.toml", "internal_name"),
        ("ignored.txt", "internal_name"),
        ("binary.dat", "\0internal_name"),
    ] {
        fs::write(root.path().join(path), contents).unwrap();
    }
    git(&["add", "tracked.rs"]);
    let hits = denylist::scan(root.path(), &["internal[-_]name".into()]).unwrap();
    assert_eq!(hits.iter().map(|hit| hit.line).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(hits[1].path.to_str(), Some("tracked.rs"));
}

#[test]
fn license_policy_checks_coverage_publish_status_and_transitive_boundaries() {
    let root = tempfile::tempdir().unwrap();
    for name in ["sdk", "bridge", "app"] {
        fs::create_dir(root.path().join(name)).unwrap();
        fs::write(root.path().join(name).join("Cargo.toml"), "").unwrap();
    }
    let classes = r#"
[[package]]
name = "sdk"
path = "sdk"
class = "mit-apache"
publish = true
[[package]]
name = "bridge"
path = "bridge"
class = "mit-apache"
publish = false
[[package]]
name = "app"
path = "app"
class = "proprietary"
publish = false
"#;
    let packages = ["sdk", "bridge", "app"].map(|name| {
        json!({
            "name": name, "id": name, "version": "0.1.0",
            "manifest_path": root.path().join(name).join("Cargo.toml"),
            "license": if name == "app" { None } else { Some("MIT OR Apache-2.0") },
            "publish": if name == "sdk" { None } else { Some(Vec::<String>::new()) },
        })
    });
    let metadata = |boundary: bool| {
        json!({
            "workspace_members": ["sdk", "bridge", "app"],
            "packages": packages,
            "resolve": { "nodes": [
                {"id": "sdk", "dependencies": ["bridge"]},
                {"id": "bridge", "dependencies": if boundary {vec!["app"]} else {vec![]}},
                {"id": "app", "dependencies": []}
            ]}
        })
    };
    let verify = |policy: &str, graph: &serde_json::Value| {
        license::validate(root.path(), policy, &graph.to_string())
    };
    verify(classes, &metadata(false)).unwrap();
    assert!(
        verify(classes, &metadata(true))
            .unwrap_err()
            .to_string()
            .contains("flows")
    );
    assert!(
        verify(
            &classes.replace("publish = true", "publish = false"),
            &metadata(false)
        )
        .is_err()
    );
    assert!(
        verify(
            "[[package]]\nname = 'sdk'\npath = 'sdk'\nclass = 'mit-apache'\npublish = true",
            &metadata(false)
        )
        .is_err()
    );
}
