use super::*;

// Explicit opt-in: exercised using an absolute, already installed Node path.
// No package, device, user config, worker native code, or downloads are used.
fn decoy(script: &str) -> Result<VoiceEvent> {
    let node = PathBuf::from(std::env::var_os("VOICE_TEST_NODE").expect("set VOICE_TEST_NODE"));
    ensure!(node.is_absolute(), "VOICE_TEST_NODE must be absolute");
    let mut command = crate::command(&node);
    command.current_dir(node.parent().unwrap()).env_clear();
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
        .args(["-e", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut tree = Tree::spawn(&mut command)?;
    supervise(
        &mut tree,
        true,
        &Controls::default(),
        &async_channel::bounded(3).0,
    )
}

#[test]
#[ignore = "requires an absolute VOICE_TEST_NODE"]
fn rejects_trailing_helper_output() {
    for trailing in [r#"{"type":"ready"}\n"#, "partial", "\\n"] {
        let script = format!(
            r#"process.stdin.once('data', () => {{
            process.stdin.pause();
            process.stdout.write('{{"type":"ready"}}\n{}');
        }});"#,
            trailing
        );
        let error = decoy(&script).unwrap_err();
        assert_eq!(error.to_string(), "Trailing voice helper output");
    }
}

#[test]
#[ignore = "requires an absolute VOICE_TEST_NODE"]
fn helper_exit_terminates_grandchild_holding_pipe() {
    let event = decoy(r#"process.stdin.once('data', () => {
        process.stdin.pause();
        const child = require('node:child_process').spawn(process.execPath,
            ['-e', 'setInterval(() => console.log("unexpected survivor"), 10000)'],
            { stdio: ['ignore', 'inherit', 'ignore'], windowsHide: true });
        child.once('spawn', () => process.stdout.write('{"type":"ready"}\n', () => process.exit(0)));
    });"#).unwrap();
    // The grandchild holds stdout open and never closes it on its own;
    // Ready is returned only after pipe EOF and the helper has been reaped.
    assert_eq!(event, VoiceEvent::Ready);
}

// A real bundled package and its pinned Node, in a disposable install with
// no voice assets or user config. Probe must not start audio or write a home.
#[test]
#[ignore = "requires VOICE_TEST_BUNDLE and its pinned VOICE_TEST_NODE"]
fn pinned_bundle_probes_without_opening_audio() {
    let absolute = |key| {
        let path = PathBuf::from(std::env::var_os(key).expect("set voice fixture path"));
        assert!(path.is_absolute());
        dunce::canonicalize(path).unwrap()
    };
    let root = absolute("VOICE_TEST_BUNDLE");
    let node = absolute("VOICE_TEST_NODE");
    let package = dunce::canonicalize(root.join("versions/4.9.0/node_modules/gentle-pi")).unwrap();
    let config = root.join("config");
    assert!(!config.exists(), "use a fixture without a config home");
    let env = std::env::var_os("SystemRoot")
        .map(|root| ("SystemRoot", root))
        .into_iter()
        .collect();
    let adapter = VoiceAdapter {
        root: root.clone(),
        package,
        home: root,
        node,
        mode: "bundled",
        env,
    };
    assert_eq!(
        adapter.probe().unwrap(),
        VoiceEvent::Error {
            code: "not-installed".into()
        }
    );
    assert!(!config.exists());
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let name = format!(
        "pi-voice-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(name);
    std::fs::create_dir(&root).unwrap();
    Scratch(dunce::canonicalize(root).unwrap())
}

#[test]
fn replaced_node_is_rejected_before_spawn() {
    use std::fs;
    let scratch = scratch();
    let node = scratch
        .0
        .join(if cfg!(windows) { "node.exe" } else { "node" });
    fs::write(&node, "original").unwrap();
    let adapter = VoiceAdapter {
        root: scratch.0.clone(),
        package: scratch.0.clone(),
        home: scratch.0.clone(),
        node: node.clone(),
        mode: "bundled",
        env: vec![],
    };
    let replacement = scratch.0.join("replacement");
    fs::write(
        &replacement,
        "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"ready\"}'\n: > executed\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::remove_file(&node).unwrap();
    fs::rename(replacement, node).unwrap();
    assert_eq!(
        adapter.probe().unwrap(),
        VoiceEvent::Error {
            code: "unsupported".into()
        }
    );
    assert!(!scratch.0.join("executed").exists());
}

#[cfg(unix)]
#[test]
fn rejects_bundled_node_symlink_outside_selected_install() {
    use std::fs;
    use std::os::unix::fs::symlink;
    let scratch = scratch();
    let prefix = scratch.0.join("install");
    let launcher = prefix.join("bin/gentle-shell");
    let node = prefix.join("runtime/node-24/bin/node");
    let version = prefix.join("versions/4.9.0");
    fs::create_dir_all(version.join("node_modules/gentle-pi")).unwrap();
    fs::create_dir_all(node.parent().unwrap()).unwrap();
    fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    fs::write(
        &launcher,
        "# gentle-shell bundled launcher (generated; do not edit)\nruntime/node-24/bin/node\n",
    )
    .unwrap();
    symlink("versions/4.9.0", prefix.join("current")).unwrap();
    let outside = scratch.0.join("outside");
    fs::write(&outside, "not executed").unwrap();
    symlink(&outside, &node).unwrap();
    let spec = LaunchSpec {
        program: launcher.clone(),
        args: vec![],
        cwd: prefix,
        env: vec![],
        bundled: crate::launch::bundled_install(&launcher),
        launcher,
    };
    assert!(spec.bundled.is_some());
    assert!(VoiceAdapter::from_launch(&spec).is_none());
    fs::remove_file(&node).unwrap();
    fs::write(&node, "not executed").unwrap();
    assert!(VoiceAdapter::from_launch(&spec).is_some());
}
