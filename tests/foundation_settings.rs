use copilot_controller::domain::{
    Action, BindingKey, Brightness, ConfigError, Direction, DpadAngles, LaunchHost,
    MAX_CONFIG_BYTES, Settings, ValidationError,
};

#[test]
fn defaults_and_recommended_preset_are_distinct() {
    let settings = Settings::from_json(br#"{"version":1}"#).unwrap();
    assert_eq!(settings, Settings::default());
    assert_eq!(settings.brightness.get(), 0.25);
    assert!(!settings.voice_enabled);
    assert!(!settings.controls_enabled);
    assert!(!settings.auto_bind);
    assert!(!settings.title_probe);
    assert!(!settings.display_titles);
    assert!(!settings.window_switcher);
    assert_eq!(settings.launch_host, LaunchHost::Standalone);
    assert_eq!(
        BindingKey::ALL.map(|key| settings.bindings.get(key)),
        [
            Action::Autopilot,
            Action::Accept,
            Action::Stop,
            Action::Fork,
            Action::Send,
            Action::Up,
            Action::Right,
            Action::Down,
            Action::Left,
        ]
    );

    let recommended = Settings::recommended();
    assert_eq!(recommended.brightness.get(), 0.75);
    assert!(recommended.controls_enabled);
    assert!(recommended.auto_bind);
    assert!(recommended.title_probe);
    assert!(recommended.window_switcher);
    assert!(!recommended.voice_enabled);
    assert!(!recommended.display_titles);
    assert_eq!(recommended.launch_host, LaunchHost::Standalone);
    assert_eq!(recommended.bindings, settings.bindings);
}

#[test]
fn partial_bindings_preserve_defaults_and_restrict_dpad_commands() {
    let mut settings = Settings::from_json(
        br#"{"version":1,"bindings":{"ACT06":"model_picker","DPAD_UP":"none","DPAD_RIGHT":"left"},"launch_host":"integrated"}"#,
    )
    .unwrap();
    assert_eq!(settings.launch_host, LaunchHost::Integrated);
    assert_eq!(
        settings.bindings.get(BindingKey::Act06),
        Action::ModelPicker
    );
    assert_eq!(settings.bindings.get(BindingKey::Act07), Action::Accept);
    assert_eq!(settings.bindings.get(BindingKey::DpadUp), Action::None);
    assert_eq!(settings.bindings.get(BindingKey::DpadRight), Action::Left);
    let previous = settings.bindings.clone();
    assert_eq!(
        settings.bindings.set(BindingKey::DpadLeft, Action::Stop),
        Err(ValidationError::DirectionalBinding)
    );
    assert_eq!(settings.bindings, previous);
    settings
        .bindings
        .set(BindingKey::Act06, Action::None)
        .unwrap();

    let serialized = serde_json::to_vec(&settings).unwrap();
    assert_eq!(Settings::from_json(&serialized).unwrap(), settings);
}

#[test]
fn strict_settings_reject_unknown_duplicate_null_and_wrong_types() {
    for json in [
        r#"{}"#,
        r#"[1]"#,
        r#"{"version":2}"#,
        r#"{"version":1.0}"#,
        r#"{"version":"1"}"#,
        r#"{"version":null}"#,
        r#"{"version":1,"version":1}"#,
        r#"{"version":1,"other":true}"#,
        r#"{"version":1,"brightness":null}"#,
        r#"{"version":1,"voice_enabled":null}"#,
        r#"{"version":1,"controls_enabled":1}"#,
        r#"{"version":1,"auto_bind":"true"}"#,
        r#"{"version":1,"display_titles":0}"#,
        r#"{"version":1,"launch_host":null}"#,
        r#"{"version":1,"launch_host":"default_terminal"}"#,
        r#"{"version":1,"bindings":null}"#,
        r#"{"version":1,"bindings":[]}"#,
        r#"{"version":1,"bindings":{"ACT10":"none"}}"#,
        r#"{"version":1,"bindings":{"ACT06":"stop","ACT06":"send"}}"#,
        r#"{"version":1,"bindings":{"ACT06":null}}"#,
        r#"{"version":1,"bindings":{"ACT06":"execute"}}"#,
        r#"{"version":1,"bindings":{"DPAD_UP":"send"}}"#,
        r#"{"version":1,"dpad_angles":null}"#,
        r#"{"version":1,"dpad_angles":[]}"#,
        r#"{"version":1,"dpad_angles":{"center":0}}"#,
        r#"{"version":1,"dpad_angles":{"up":0,"up":0.1}}"#,
        r#"{"version":1,"dpad_angles":{"up":null}}"#,
        r#"{"version":1} trailing"#,
        r#"{"version":1}{}"#,
        r#"{"version":1"#,
    ] {
        assert!(Settings::from_json(json.as_bytes()).is_err(), "{json}");
    }
}

#[test]
fn brightness_is_finite_and_inclusive_at_both_ends() {
    for value in [0.0, -0.0, 0.25, 1.0] {
        assert_eq!(Brightness::new(value).unwrap().get(), value);
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.001, 1.001] {
        assert_eq!(Brightness::new(value), Err(ValidationError::Brightness));
    }
    for json in [
        r#"{"version":1,"brightness":-0.1}"#,
        r#"{"version":1,"brightness":1.1}"#,
        r#"{"version":1,"brightness":1e999}"#,
        r#"{"version":1,"brightness":true}"#,
        r#"{"version":1,"brightness":"0.5"}"#,
        r#"{"version":1,"brightness":NaN}"#,
    ] {
        assert!(Settings::from_json(json.as_bytes()).is_err(), "{json}");
    }
}

#[test]
fn calibration_checks_ranges_and_complete_pairwise_circular_distance() {
    let partial =
        Settings::from_json(br#"{"version":1,"dpad_angles":{"up":0.25,"right":0.25,"down":0.25}}"#)
            .unwrap();
    assert_eq!(partial.dpad_angles.get(Direction::Left), None);
    let mut angles = partial.dpad_angles.clone();
    assert_eq!(
        angles.set(Direction::Left, Some(0.75)),
        Err(ValidationError::CalibrationOverlap)
    );
    assert_eq!(angles, partial.dpad_angles);

    for json in [
        r#"{"version":1,"dpad_angles":{"up":-0.01}}"#,
        r#"{"version":1,"dpad_angles":{"up":1}}"#,
        r#"{"version":1,"dpad_angles":{"up":true}}"#,
        r#"{"version":1,"dpad_angles":{"up":"0.25"}}"#,
        r#"{"version":1,"dpad_angles":{"up":0,"right":0.14,"down":0.5,"left":0.75}}"#,
        r#"{"version":1,"dpad_angles":{"up":0.94,"right":0.09,"down":0.4,"left":0.65}}"#,
    ] {
        assert!(Settings::from_json(json.as_bytes()).is_err(), "{json}");
    }
    assert!(DpadAngles::new([Some(f64::NAN), None, None, None]).is_err());
    assert!(DpadAngles::new([Some(f64::INFINITY), None, None, None]).is_err());
    let valid = Settings::from_json(
        br#"{"version":1,"dpad_angles":{"up":0,"right":0.15,"down":0.4,"left":0.7}}"#,
    )
    .unwrap();
    assert_eq!(valid.dpad_angles.get(Direction::Right), Some(0.15));
    assert_eq!(
        serde_json::to_string(&valid.dpad_angles).unwrap(),
        r#"{"down":0.4,"left":0.7,"right":0.15,"up":0.0}"#
    );
}

#[test]
fn standalone_size_is_checked_before_json_parsing_including_whitespace() {
    let mut exactly = br#"{"version":1}"#.to_vec();
    exactly.resize(MAX_CONFIG_BYTES, b' ');
    assert!(Settings::from_json(&exactly).is_ok());
    exactly.push(b' ');
    assert!(matches!(
        Settings::from_json(&exactly),
        Err(ConfigError::TooLarge {
            actual: 65_537,
            limit: 65_536
        })
    ));
    assert!(matches!(
        Settings::from_json(&vec![b'x'; MAX_CONFIG_BYTES + 1]),
        Err(ConfigError::TooLarge { .. })
    ));
}
