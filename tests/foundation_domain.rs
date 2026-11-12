use std::time::Duration;

use copilot_controller::domain::{
    Action, ClockError, ClockSample, ClockSource, Generations, MonotonicStamp,
    NativeProcessIdentity, OpaqueId, RefusalReason, RuntimeEpoch, SessionId, Slot, Status,
    TargetIdentity,
};

const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
const OTHER_ID: &str = "11111111-2222-4333-8444-555555555555";

fn epoch() -> RuntimeEpoch {
    RuntimeEpoch::parse_str(ID).unwrap()
}

fn stamp(ticks: u64, frequency: u64) -> MonotonicStamp {
    MonotonicStamp {
        epoch: epoch(),
        ticks,
        frequency,
    }
}

#[test]
fn slots_validate_construction_and_json_identically() {
    for number in 1..=6 {
        let slot = Slot::new(number).unwrap();
        assert_eq!(slot.number(), number);
        assert_eq!(serde_json::to_string(&slot).unwrap(), number.to_string());
        assert_eq!(
            serde_json::from_str::<Slot>(&number.to_string()).unwrap(),
            slot
        );
    }
    assert!(Slot::new(0).is_err());
    assert!(Slot::new(7).is_err());
    assert!(Slot::try_from(-1_i64).is_err());
    assert!(Slot::try_from(u64::MAX).is_err());
    for json in [
        "0",
        "7",
        "-1",
        "1.0",
        "1e0",
        "\"1\"",
        "null",
        "true",
        "18446744073709551616",
    ] {
        assert!(serde_json::from_str::<Slot>(json).is_err(), "{json}");
    }
}

#[test]
fn uuid_text_is_canonical_and_epoch_is_a_distinct_fresh_identity() {
    let session = SessionId::parse_str(&ID.to_uppercase()).unwrap();
    assert_eq!(session.to_string(), ID);
    assert_eq!(
        serde_json::to_string(&session).unwrap(),
        format!("\"{ID}\"")
    );
    assert_eq!(
        serde_json::from_str::<SessionId>(&format!("\"{}\"", ID.to_uppercase())).unwrap(),
        session
    );
    for bad in [
        "{01234567-89ab-cdef-0123-456789abcdef}",
        "0123456789abcdef0123456789abcdef",
        "urn:uuid:01234567-89ab-cdef-0123-456789abcdef",
        "01234567_89ab-cdef-0123-456789abcdef",
        "01234567-89ab-cdef-0123-456789abcdeg",
        "01234567-89ab-cdef-0123-456789abcde",
    ] {
        assert!(SessionId::parse_str(bad).is_err(), "{bad}");
        assert!(serde_json::from_str::<RuntimeEpoch>(&format!("\"{bad}\"")).is_err());
    }
    assert!(serde_json::from_str::<SessionId>("null").is_err());

    let first = RuntimeEpoch::fresh();
    let second = RuntimeEpoch::fresh();
    assert!(!first.is_nil());
    assert_ne!(first, second);
    assert_eq!(first.as_uuid().get_version_num(), 4);
    assert!(
        RuntimeEpoch::parse_str("00000000-0000-0000-0000-000000000000")
            .unwrap()
            .is_nil()
    );
}

#[test]
fn opaque_and_native_targets_remain_distinct_and_strict() {
    assert_eq!(
        OpaqueId::new(" é / punctuation ").unwrap().as_str(),
        " é / punctuation "
    );
    assert!(OpaqueId::new("é".repeat(128)).is_ok());
    assert!(OpaqueId::new(format!("{}x", "é".repeat(128))).is_err());
    assert!(OpaqueId::new("").is_err());
    assert!(OpaqueId::new("a\nb").is_err());
    assert!(serde_json::from_str::<OpaqueId>("null").is_err());

    let native = TargetIdentity::WindowsNative {
        process: NativeProcessIdentity {
            pid: 7,
            creation_time_100ns: 42,
            session_id: 3,
        },
        window: 12,
    };
    let integrated = TargetIdentity::IntegratedHost {
        host_epoch: epoch(),
        target: OpaqueId::new("12").unwrap(),
        attachment: OpaqueId::new("pane").unwrap(),
    };
    assert_ne!(native, integrated);
    for target in [native, integrated] {
        let json = serde_json::to_string(&target).unwrap();
        assert_eq!(
            serde_json::from_str::<TargetIdentity>(&json).unwrap(),
            target
        );
    }

    let zero_native = r#"{"backend":"windows_native","process":{"pid":0,"creation_time_100ns":0,"session_id":0},"window":0}"#;
    assert!(serde_json::from_str::<TargetIdentity>(zero_native).is_ok());
    let nil_integrated = r#"{"backend":"integrated_host","host_epoch":"00000000-0000-0000-0000-000000000000","target":"x","attachment":"y"}"#;
    assert!(serde_json::from_str::<TargetIdentity>(nil_integrated).is_ok());
    let unknown = r#"{"backend":"windows_native","process":{"pid":1,"creation_time_100ns":2,"session_id":3,"extra":4},"window":5}"#;
    let duplicate = r#"{"backend":"integrated_host","host_epoch":"01234567-89ab-cdef-0123-456789abcdef","target":"x","target":"y","attachment":"z"}"#;
    let duplicate_backend = r#"{"backend":"integrated_host","backend":"integrated_host","host_epoch":"01234567-89ab-cdef-0123-456789abcdef","target":"x","attachment":"z"}"#;
    let extra_field = r#"{"backend":"integrated_host","host_epoch":"01234567-89ab-cdef-0123-456789abcdef","target":"x","attachment":"z","window":3}"#;
    assert!(serde_json::from_str::<TargetIdentity>(unknown).is_err());
    assert!(serde_json::from_str::<TargetIdentity>(duplicate).is_err());
    assert!(serde_json::from_str::<TargetIdentity>(duplicate_backend).is_err());
    assert!(serde_json::from_str::<TargetIdentity>(extra_field).is_err());
    assert!(serde_json::from_str::<NativeProcessIdentity>("[1,2,3]").is_err());
    let array_process = r#"{"backend":"windows_native","process":[1,2,3],"window":5}"#;
    assert!(serde_json::from_str::<TargetIdentity>(array_process).is_err());
    assert!(
        serde_json::from_str::<TargetIdentity>(
            r#"["windows_native",{"pid":1,"creation_time_100ns":2,"session_id":3},5]"#
        )
        .is_err()
    );
}

#[test]
fn all_generation_dimensions_participate_in_equality() {
    let baseline = Generations {
        connection: 1,
        profile: 2,
        selection: 3,
        binding: 4,
        presentation: 5,
    };
    for index in 0..5 {
        let mut changed = baseline;
        match index {
            0 => changed.connection += 1,
            1 => changed.profile += 1,
            2 => changed.selection += 1,
            3 => changed.binding += 1,
            _ => changed.presentation += 1,
        }
        assert_ne!(baseline, changed);
    }
    assert!(serde_json::from_str::<Generations>("[1,2,3,4,5]").is_err());
}

#[test]
fn monotonic_elapsed_uses_widened_arithmetic_and_exact_deadlines() {
    assert_eq!(
        stamp(25, 10).elapsed_since(stamp(10, 10)).unwrap(),
        Duration::new(1, 500_000_000)
    );
    assert_eq!(
        stamp(u64::MAX, 1).elapsed_since(stamp(0, 1)).unwrap(),
        Duration::from_secs(u64::MAX)
    );
    assert_eq!(
        stamp(u64::MAX - 1, u64::MAX)
            .elapsed_since(stamp(0, u64::MAX))
            .unwrap(),
        Duration::new(0, 999_999_999)
    );

    let frequency = 1_000_000_001;
    let start = stamp(0, frequency);
    let exactly = stamp(3 * frequency, frequency);
    let fractional_tick_late = stamp(3 * frequency + 1, frequency);
    assert!(
        exactly
            .elapsed_within(start, Duration::from_secs(3))
            .unwrap()
    );
    assert_eq!(
        fractional_tick_late.elapsed_since(start).unwrap(),
        Duration::from_secs(3)
    );
    assert!(
        !fractional_tick_late
            .elapsed_within(start, Duration::from_secs(3))
            .unwrap()
    );
    assert!(
        !stamp(1, u64::MAX)
            .elapsed_within(stamp(0, u64::MAX), Duration::ZERO)
            .unwrap()
    );
    assert!(
        stamp(u64::MAX, 1)
            .elapsed_within(stamp(0, 1), Duration::MAX)
            .unwrap()
    );
    assert!(
        stamp(u64::MAX, u64::MAX)
            .elapsed_within(stamp(0, u64::MAX), Duration::MAX)
            .unwrap()
    );
}

#[test]
fn incompatible_clocks_fail_closed_including_source_changes() {
    let array_stamp = format!(r#"["{ID}",1,10]"#);
    assert!(serde_json::from_str::<MonotonicStamp>(&array_stamp).is_err());
    let array_sample = format!(r#"["linux_monotonic",{array_stamp}]"#);
    assert!(serde_json::from_str::<ClockSample>(&array_sample).is_err());
    let start = stamp(10, 10);
    assert_eq!(
        MonotonicStamp {
            epoch: RuntimeEpoch::parse_str(OTHER_ID).unwrap(),
            ..start
        }
        .elapsed_since(start),
        Err(ClockError::EpochChanged)
    );
    assert_eq!(
        stamp(10, 0).elapsed_since(stamp(0, 0)),
        Err(ClockError::ZeroFrequency)
    );
    assert_eq!(
        stamp(10, 10).elapsed_since(stamp(0, 0)),
        Err(ClockError::ZeroFrequency)
    );
    assert_eq!(
        stamp(10, 11).elapsed_since(start),
        Err(ClockError::FrequencyChanged)
    );
    assert_eq!(
        stamp(9, 10).elapsed_since(start),
        Err(ClockError::TicksRegressed)
    );

    let later = ClockSample {
        source: ClockSource::LinuxMonotonic,
        stamp: start,
    };
    let earlier = ClockSample {
        source: ClockSource::MacosUptimeRaw,
        stamp: MonotonicStamp {
            epoch: RuntimeEpoch::parse_str(OTHER_ID).unwrap(),
            ..start
        },
    };
    assert_eq!(later.elapsed_since(earlier), Err(ClockError::SourceChanged));
    assert_eq!(
        later.elapsed_within(earlier, Duration::from_secs(3)),
        Err(ClockError::SourceChanged)
    );
}

#[test]
fn vocabularies_keep_unknown_unavailable_and_off_distinct() {
    for (json, value) in [
        ("\"off\"", Status::Off),
        ("\"idle\"", Status::Idle),
        ("\"working\"", Status::Working),
        ("\"complete\"", Status::Complete),
        ("\"waiting\"", Status::Waiting),
        ("\"error\"", Status::Error),
        ("\"unknown\"", Status::Unknown),
    ] {
        assert_eq!(serde_json::from_str::<Status>(json).unwrap(), value);
        assert_eq!(serde_json::to_string(&value).unwrap(), json);
    }
    for (json, value) in [
        ("\"none\"", Action::None),
        ("\"autopilot\"", Action::Autopilot),
        ("\"accept\"", Action::Accept),
        ("\"stop\"", Action::Stop),
        ("\"fork\"", Action::Fork),
        ("\"send\"", Action::Send),
        ("\"model_picker\"", Action::ModelPicker),
        ("\"up\"", Action::Up),
        ("\"right\"", Action::Right),
        ("\"down\"", Action::Down),
        ("\"left\"", Action::Left),
    ] {
        assert_eq!(serde_json::from_str::<Action>(json).unwrap(), value);
        assert_eq!(serde_json::to_string(&value).unwrap(), json);
    }
    let reasons = [
        "disabled",
        "unsupported",
        "unknown_ui",
        "stale_state",
        "clock_mismatch",
        "wrong_target",
        "changed_binding",
        "changed_generation",
        "changed_run",
        "changed_draft",
        "changed_dialog",
        "command_draft",
        "empty_draft",
        "busy",
        "broad_approval",
        "duplicate",
        "indeterminate_delivery",
        "pending_input",
        "unavailable",
        "unsafe_context",
        "expired_intent",
    ];
    for reason in reasons {
        let json = format!("\"{reason}\"");
        let value: RefusalReason = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), json);
    }
    assert_ne!(Status::Unknown, Status::Off);
    assert_ne!(RefusalReason::Unavailable, RefusalReason::Unsupported);
    assert!(serde_json::from_str::<Action>("\"execute\"").is_err());
}
