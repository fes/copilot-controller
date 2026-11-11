use std::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_copilot-controller");

#[test]
fn scaffold_exposes_help_and_version_but_no_action_commands() {
    let help = Command::new(BINARY).arg("--help").output().unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("No device discovery or action delivery"));

    let version = Command::new(BINARY).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(
        String::from_utf8(version.stdout)
            .unwrap()
            .starts_with("copilot-controller ")
    );

    for command in ["run", "send", "accept", "stop", "fork", "_control"] {
        let output = Command::new(BINARY).arg(command).output().unwrap();
        assert!(!output.status.success(), "{command}");
        assert!(output.stdout.is_empty());
    }
}
