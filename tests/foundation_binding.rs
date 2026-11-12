use copilot_controller::domain::{
    Assignment, Binding, ConfigError, MAX_CONFIG_BYTES, RuntimeId, SessionId, Settings, Slot,
    TerminalAnchor, WindowsSessionRoot, validate_custom_label,
};

const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
const OTHER_ID: &str = "fedcba98-7654-4321-8765-43210fedcba9";

fn binding_json(title: &str, extra: &str) -> String {
    format!(
        r#"{{"session_id":"{ID}","terminal_title":{}{extra}}}"#,
        serde_json::to_string(title).unwrap()
    )
}

fn parse(title: &str, extra: &str) -> Result<Binding, ConfigError> {
    Binding::from_json(binding_json(title, extra).as_bytes())
}

#[test]
fn binding_text_limits_use_scalars_and_reject_controls() {
    assert!(parse("漢e\u{301}🧠", "").is_ok());
    assert!(parse(&"🧠".repeat(512), "").is_ok());
    assert!(parse(&"🧠".repeat(513), "").is_err());
    assert!(parse("", "").is_err());
    assert!(parse("terminal\nname", "").is_err());

    for (field, limit) in [("process_created", 128), ("label", 512)] {
        let accepted = format!(
            r#","{field}":{}"#,
            serde_json::to_string(&"界".repeat(limit)).unwrap()
        );
        let too_long = format!(
            r#","{field}":{}"#,
            serde_json::to_string(&"界".repeat(limit + 1)).unwrap()
        );
        assert!(parse("title", &accepted).is_ok(), "{field}");
        assert!(parse("title", &too_long).is_err(), "{field}");
        assert!(parse("title", &format!(r#","{field}":"""#)).is_err());
        assert!(parse("title", &format!(r#","{field}":"a\nb""#)).is_err());
    }
}

#[test]
fn custom_labels_use_unicode_16_categories_and_scalar_count() {
    for label in [
        "",
        "日本語",
        "e\u{301}",
        "Ω ∑ 💠",
        "a\u{2028}b",
        "\u{1fae9}",
    ] {
        assert!(validate_custom_label(label).is_ok(), "{label:?}");
        let extra = format!(
            r#","custom_label":{}"#,
            serde_json::to_string(label).unwrap()
        );
        assert_eq!(parse("title", &extra).unwrap().custom_label(), Some(label));
    }
    assert!(validate_custom_label(&"é".repeat(80)).is_ok());
    assert!(validate_custom_label(&"é".repeat(81)).is_err());
    for label in ["a\nb", "a\u{200d}b", "\u{e000}", "\u{0378}", "\u{10ffff}"] {
        assert!(validate_custom_label(label).is_err(), "{label:?}");
        let extra = format!(
            r#","custom_label":{}"#,
            serde_json::to_string(label).unwrap()
        );
        assert!(parse("title", &extra).is_err());
    }
    assert!(parse("title", r#","custom_label":"\ud800""#).is_err());
}

#[test]
fn presentation_consent_and_explicit_empty_label_are_preserved() {
    let slot = Slot::new(4).unwrap();
    let mut binding = parse(
        "terminal title",
        r#","label":"derived title","custom_label":"""#,
    )
    .unwrap();
    assert_eq!(binding.custom_label(), Some(""));
    assert_eq!(binding.presentation_label(slot, false), "");
    assert_eq!(binding.presentation_label(slot, true), "");
    let serialized = serde_json::to_vec(&binding).unwrap();
    assert_eq!(
        Binding::from_json(&serialized).unwrap().custom_label(),
        Some("")
    );

    binding.set_custom_label(Some("")).unwrap();
    assert_eq!(binding.custom_label(), None);
    assert_eq!(binding.presentation_label(slot, false), "Slot 4");
    let probing = Settings::from_json(br#"{"version":1,"title_probe":true}"#).unwrap();
    assert_eq!(
        binding.presentation_label(slot, probing.display_titles),
        "Slot 4"
    );
    assert_eq!(binding.presentation_label(slot, true), "derived title");
    binding.set_label(None).unwrap();
    assert_eq!(binding.presentation_label(slot, true), "terminal title");
    binding.set_custom_label(Some("Pinned 🧠")).unwrap();
    assert_eq!(binding.presentation_label(slot, false), "Pinned 🧠");
}

#[test]
fn optional_fields_are_non_null_and_nested_schema_is_strict() {
    assert!(Binding::from_json(format!(r#"["{ID}","title"]"#).as_bytes()).is_err());
    for field in [
        "pinned",
        "custom_label",
        "pid",
        "process_created",
        "session_root",
        "label",
        "title_prefix",
        "initial_idle_confirmed",
        "assignment",
        "target",
        "fork_parent",
    ] {
        assert!(
            parse("title", &format!(r#","{field}":null"#)).is_err(),
            "{field}"
        );
    }
    for extra in [
        r#","unexpected":true"#,
        r#","pinned":false,"pinned":true"#,
        r#","pinned":1"#,
        r#","title_prefix":"false""#,
        r#","assignment":"manual""#,
        r#","fork_parent":"fedcba9876544321876543210fedcba9""#,
        r#","target":{"handle":1,"host_pid":2,"host_created":"now","runtime_id":"1,2","extra":3}"#,
        r#","target":{"handle":1,"handle":2,"host_pid":2,"host_created":"now","runtime_id":"1,2"}"#,
        r#","target":[1,2,"now","1,2"]"#,
    ] {
        assert!(parse("title", extra).is_err(), "{extra}");
    }

    let json = format!(
        r#"{{"session_id":"{}","terminal_title":"title","assignment":"explicit","fork_parent":"{OTHER_ID}"}}"#,
        ID.to_uppercase()
    );
    let binding = Binding::from_json(json.as_bytes()).unwrap();
    assert_eq!(binding.session_id().to_string(), ID);
    assert_eq!(binding.assignment(), Some(Assignment::Explicit));
    assert_eq!(binding.fork_parent().unwrap().to_string(), OTHER_ID);
    assert!(!binding.pinned());
    assert!(!binding.initial_idle_confirmed());
}

#[test]
fn pid_and_anchor_integers_require_their_exact_positive_widths() {
    for bad in ["0", "-1", "1.0", "true", r#""1""#, "4294967296"] {
        assert!(
            parse("title", &format!(r#","pid":{bad}"#)).is_err(),
            "{bad}"
        );
    }
    assert_eq!(
        parse("title", r#","pid":4294967295"#).unwrap().pid(),
        Some(u32::MAX)
    );

    fn anchor(handle: &str, pid: &str, created: &str, runtime: &str) -> String {
        format!(
            r#","target":{{"handle":{handle},"host_pid":{pid},"host_created":{},"runtime_id":{}}}"#,
            serde_json::to_string(created).unwrap(),
            serde_json::to_string(runtime).unwrap()
        )
    }
    assert!(
        parse(
            "title",
            &anchor("18446744073709551615", "4294967295", "t", "1,-2")
        )
        .is_ok()
    );
    for bad in [
        "0",
        "-1",
        "1.0",
        "true",
        r#""1""#,
        "null",
        "18446744073709551616",
    ] {
        assert!(
            parse("title", &anchor(bad, "1", "t", "1,2")).is_err(),
            "{bad}"
        );
    }
    for bad in ["0", "-1", "1.0", "true", r#""1""#, "null", "4294967296"] {
        assert!(
            parse("title", &anchor("1", bad, "t", "1,2")).is_err(),
            "{bad}"
        );
    }
    assert!(parse("title", &anchor("1", "1", "", "1,2")).is_err());
    assert!(parse("title", &anchor("1", "1", "x\n", "1,2")).is_err());
    assert!(parse("title", &anchor("1", "1", &"界".repeat(129), "1,2")).is_err());

    let runtime = RuntimeId::new("-2147483648,2147483647").unwrap();
    assert!(TerminalAnchor::new(1, 1, "created", runtime.clone()).is_ok());
    assert!(TerminalAnchor::new(0, 1, "created", runtime.clone()).is_err());
    assert!(TerminalAnchor::new(1, 0, "created", runtime).is_err());
}

#[test]
fn runtime_id_grammar_and_component_limits_are_strict() {
    for valid in ["1,2", "-2147483648,2147483647", "0,00,-0", "00001,-00002"] {
        assert!(RuntimeId::new(valid).is_ok(), "{valid}");
    }
    let thirty_two = vec!["0"; 32].join(",");
    assert!(RuntimeId::new(&thirty_two).is_ok());
    assert!(RuntimeId::new(format!("{thirty_two},0")).is_err());
    for invalid in [
        "",
        "1",
        "1,",
        ",1",
        "1, 2",
        "1,+2",
        "1,--2",
        "1,-",
        "1,２",
        "1,2147483648",
        "1,-2147483649",
        "1.0,2",
    ] {
        assert!(RuntimeId::new(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn session_roots_are_windows_lexical_paths_not_host_paths() {
    for valid in [
        r"C:\",
        r"C:\work\子",
        "c:/work/子",
        r"\\server\share",
        r"\\server\share\work\",
        "//server/share/work",
    ] {
        assert!(WindowsSessionRoot::new(valid).is_ok(), "{valid:?}");
        let extra = format!(
            r#","session_root":{}"#,
            serde_json::to_string(valid).unwrap()
        );
        assert_eq!(
            parse("title", &extra)
                .unwrap()
                .session_root()
                .unwrap()
                .as_str(),
            valid
        );
    }
    for invalid in [
        "",
        "C:",
        r"C:relative",
        r"\rooted",
        r"relative\path",
        r"\\server",
        r"\\server\",
        r"\\?\C:\work",
        r"\\.\COM1",
        r"C:\a\..\b",
        r"C:\a.\b",
        r"C:\a \b",
        r"C:\a:b",
        r"\\server\share:name",
        r#"C:\a"b"#,
        r"C:\a<b",
        r"C:\a>b",
        r"C:\a|b",
        r"C:\a?b",
        r"C:\a*b",
        "C:\\a\nb",
    ] {
        assert!(WindowsSessionRoot::new(invalid).is_err(), "{invalid:?}");
    }
    assert!(WindowsSessionRoot::new(format!("C:\\{}", "界".repeat(4093))).is_ok());
    assert!(WindowsSessionRoot::new(format!("C:\\{}", "界".repeat(4094))).is_err());
}

#[test]
fn programmatic_binding_updates_validate_without_destroying_previous_values() {
    let session = SessionId::parse_str(ID).unwrap();
    let mut binding = Binding::new(session, "terminal").unwrap();
    binding.set_pid(Some(123)).unwrap();
    assert!(binding.set_pid(Some(0)).is_err());
    assert_eq!(binding.pid(), Some(123));
    binding.set_session_root(Some(r"C:\work")).unwrap();
    assert!(binding.set_session_root(Some("relative")).is_err());
    assert_eq!(binding.session_root().unwrap().as_str(), r"C:\work");
    binding.set_custom_label(Some("good")).unwrap();
    assert!(binding.set_custom_label(Some("\u{e000}")).is_err());
    assert_eq!(binding.custom_label(), Some("good"));
    assert!(binding.set_terminal_title("").is_err());
    assert_eq!(binding.terminal_title(), "terminal");
}

#[test]
fn binding_size_limit_includes_whitespace_before_parsing() {
    let mut bytes = binding_json("title", "").into_bytes();
    bytes.resize(MAX_CONFIG_BYTES, b' ');
    assert!(Binding::from_json(&bytes).is_ok());
    bytes.push(b' ');
    assert!(matches!(
        Binding::from_json(&bytes),
        Err(ConfigError::TooLarge { .. })
    ));
}
