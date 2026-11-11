use std::collections::BTreeMap;

use copilot_controller::domain::{
    Assignment, Binding, BindingHealth, ConfigError, MAX_CONFIG_BYTES, SessionId, Settings, Slot,
    SlotMap, SlotMutationError,
};

fn slot(number: u8) -> Slot {
    Slot::new(number).unwrap()
}

fn session(number: u64) -> SessionId {
    SessionId::parse_str(&format!("abcdef00-0000-4000-8000-{number:012x}")).unwrap()
}

fn binding(number: u64) -> Binding {
    Binding::new(session(number), format!("Terminal {number}")).unwrap()
}

fn insert(map: &mut SlotMap, number: u8, value: Binding) {
    map.bind(slot(number), value, map.revision(), None, None)
        .unwrap();
}

fn binding_json(id: &str) -> String {
    format!(r#"{{"session_id":"{id}","terminal_title":"terminal"}}"#)
}

#[test]
fn slot_map_rejects_bad_schema_keys_duplicates_and_duplicate_sessions() {
    let id = session(1).to_string();
    let record = binding_json(&id);
    let duplicate_record = binding_json(&id.to_uppercase());
    let invalid = [
        r#"{}"#.to_owned(),
        r#"[1,0,{}]"#.to_owned(),
        r#"{"version":1}"#.to_owned(),
        r#"{"version":2,"slots":{}}"#.to_owned(),
        r#"{"version":1,"version":1,"slots":{}}"#.to_owned(),
        r#"{"version":1,"revision":null,"slots":{}}"#.to_owned(),
        r#"{"version":1,"revision":-1,"slots":{}}"#.to_owned(),
        r#"{"version":1,"revision":1.0,"slots":{}}"#.to_owned(),
        r#"{"version":1,"revision":"1","slots":{}}"#.to_owned(),
        r#"{"version":1,"revision":18446744073709551616,"slots":{}}"#.to_owned(),
        r#"{"version":1,"slots":null}"#.to_owned(),
        r#"{"version":1,"slots":[]}"#.to_owned(),
        r#"{"version":1,"slots":{},"unknown":0}"#.to_owned(),
        format!(r#"{{"version":1,"slots":{{"01":{record}}}}}"#),
        format!(r#"{{"version":1,"slots":{{"0":{record}}}}}"#),
        format!(r#"{{"version":1,"slots":{{"7":{record}}}}}"#),
        format!(r#"{{"version":1,"slots":{{"-1":{record}}}}}"#),
        format!(r#"{{"version":1,"slots":{{"1.0":{record}}}}}"#),
        format!(r#"{{"version":1,"slots":{{"1":{record},"1":{record}}}}}"#),
        format!(r#"{{"version":1,"slots":{{"1":{record},"\u0031":{record}}}}}"#),
        format!(r#"{{"version":1,"slots":{{"1":{record},"2":{duplicate_record}}}}}"#),
        format!(
            r#"{{"version":1,"slots":{{"1":{{"session_id":"{id}","terminal_title":"a","terminal_title":"b"}}}}}}"#
        ),
    ];
    for json in invalid {
        assert!(SlotMap::from_json(json.as_bytes()).is_err(), "{json}");
    }

    let empty = SlotMap::from_json(br#"{"version":1,"slots":{}}"#).unwrap();
    assert_eq!(empty, SlotMap::default());
    assert_eq!(empty.revision(), 0);
    assert_eq!(empty.iter().count(), 0);
}

#[test]
fn refresh_preserves_overrides_and_replacement_requires_explicit_safe_health() {
    let mut map = SlotMap::default();
    let mut original = binding(1);
    original.set_pinned(true);
    original.set_custom_label(Some("keep")).unwrap();
    insert(&mut map, 1, original);

    let before = map.clone();
    assert_eq!(
        map.bind(
            slot(1),
            binding(2),
            map.revision(),
            Some(session(1)),
            Some(BindingHealth::ProcessExited)
        ),
        Err(SlotMutationError::Pinned)
    );
    assert_eq!(map, before);

    let mut refresh = binding(1);
    refresh.set_terminal_title("new title").unwrap();
    refresh.set_custom_label(Some("do not replace")).unwrap();
    map.bind(slot(1), refresh, map.revision(), Some(session(1)), None)
        .unwrap();
    let refreshed = map.get(slot(1)).unwrap();
    assert_eq!(refreshed.terminal_title(), "new title");
    assert!(refreshed.pinned());
    assert_eq!(refreshed.custom_label(), Some("keep"));

    map.set_pinned(slot(1), false, map.revision()).unwrap();
    for health in [
        None,
        Some(BindingHealth::Live),
        Some(BindingHealth::Unresolved),
    ] {
        let before = map.clone();
        assert_eq!(
            map.bind(
                slot(1),
                binding(2),
                map.revision(),
                Some(session(1)),
                health
            ),
            Err(SlotMutationError::NotReusable)
        );
        assert_eq!(map, before);
    }
    let before = map.clone();
    assert_eq!(
        map.bind(
            slot(1),
            binding(2),
            map.revision(),
            Some(session(9)),
            Some(BindingHealth::PidReused)
        ),
        Err(SlotMutationError::CurrentSessionMismatch)
    );
    assert_eq!(map, before);
    map.bind(
        slot(1),
        binding(2),
        map.revision(),
        Some(session(1)),
        Some(BindingHealth::PidReused),
    )
    .unwrap();
    map.bind(
        slot(1),
        binding(3),
        map.revision(),
        Some(session(2)),
        Some(BindingHealth::ProcessExited),
    )
    .unwrap();
    assert_eq!(map.get(slot(1)).unwrap().session_id(), session(3));

    insert(&mut map, 2, binding(4));
    let before = map.clone();
    assert_eq!(
        map.bind(slot(3), binding(4), map.revision(), None, None),
        Err(SlotMutationError::DuplicateSession)
    );
    assert_eq!(map, before);
    assert_eq!(
        map.bind(slot(3), binding(5), map.revision(), Some(session(9)), None),
        Err(SlotMutationError::CurrentSessionMismatch)
    );
    assert_eq!(map, before);
}

#[test]
fn same_session_refresh_preserves_an_explicit_empty_label() {
    let id = session(1);
    let input = format!(
        r#"{{"version":1,"slots":{{"1":{{"session_id":"{id}","terminal_title":"old","pinned":true,"custom_label":""}}}}}}"#
    );
    let mut map = SlotMap::from_json(input.as_bytes()).unwrap();
    map.bind(slot(1), binding(1), 0, Some(id), None).unwrap();
    let result = map.get(slot(1)).unwrap();
    assert!(result.pinned());
    assert_eq!(result.custom_label(), Some(""));
}

#[test]
fn label_pin_and_unbind_validate_revision_and_session_atomically() {
    let mut map = SlotMap::default();
    assert_eq!(
        map.set_pinned(slot(1), true, 0),
        Err(SlotMutationError::EmptySlot)
    );
    assert_eq!(
        map.set_custom_label(slot(1), Some("x"), 0),
        Err(SlotMutationError::EmptySlot)
    );
    insert(&mut map, 1, binding(1));
    assert_eq!(map.revision(), 1);
    map.set_pinned(slot(1), true, 1).unwrap();
    map.set_custom_label(slot(1), Some("label"), 2).unwrap();
    let before = map.clone();
    assert!(matches!(
        map.set_custom_label(slot(1), Some("\u{e000}"), 3),
        Err(SlotMutationError::InvalidLabel(_))
    ));
    assert_eq!(map, before);
    assert!(matches!(
        map.set_pinned(slot(1), false, 2),
        Err(SlotMutationError::StaleRevision { .. })
    ));
    assert_eq!(
        map.unbind(slot(1), session(2), 3),
        Err(SlotMutationError::CurrentSessionMismatch)
    );
    assert_eq!(map, before);
    map.set_custom_label(slot(1), Some(""), 3).unwrap();
    assert_eq!(map.get(slot(1)).unwrap().custom_label(), None);
    let removed = map.unbind(slot(1), session(1), 4).unwrap();
    assert!(removed.pinned());
    assert_eq!(map.revision(), 5);
    assert!(map.get(slot(1)).is_none());
}

#[test]
fn automatic_assignment_is_stable_consent_gated_and_clears_user_state() {
    let mut map = SlotMap::default();
    let disabled = Settings::default();
    let enabled = Settings::recommended();
    let health = BTreeMap::new();
    let before = map.clone();
    assert_eq!(
        map.auto_bind(&disabled, binding(1), &health, 0),
        Err(SlotMutationError::AutoBindDisabled)
    );
    assert_eq!(map, before);

    let mut candidate = binding(1);
    candidate.set_pinned(true);
    candidate.set_custom_label(Some("old label")).unwrap();
    candidate.set_initial_idle_confirmed(true);
    candidate.set_assignment(Some(Assignment::Explicit));
    candidate.set_pid(Some(123)).unwrap();
    let first = map.auto_bind(&enabled, candidate, &health, 0).unwrap();
    assert_eq!(first.slot, slot(1));
    assert!(first.newly_bound);
    assert_eq!(first.revision, 1);
    let assigned = map.get(slot(1)).unwrap();
    assert!(!assigned.pinned());
    assert_eq!(assigned.custom_label(), None);
    assert!(!assigned.initial_idle_confirmed());
    assert_eq!(assigned.assignment(), Some(Assignment::Automatic));
    assert_eq!(assigned.pid(), Some(123));

    let before = map.clone();
    let same = map.auto_bind(&enabled, binding(1), &health, 1).unwrap();
    assert_eq!(same.slot, slot(1));
    assert!(!same.newly_bound);
    assert_eq!(same.revision, 1);
    assert_eq!(map, before);
    assert_eq!(
        map.auto_bind(&disabled, binding(1), &health, 1),
        Err(SlotMutationError::AutoBindDisabled)
    );
    assert!(matches!(
        map.auto_bind(&enabled, binding(1), &health, 0),
        Err(SlotMutationError::StaleRevision { .. })
    ));
    assert_eq!(map, before);
    assert_eq!(
        map.auto_bind(&enabled, binding(2), &health, 1)
            .unwrap()
            .slot,
        slot(2)
    );
}

#[test]
fn automatic_assignment_uses_first_safe_slot_and_never_evicts_others() {
    let enabled = Settings::recommended();
    let mut map = SlotMap::default();
    let mut pinned = binding(1);
    pinned.set_pinned(true);
    insert(&mut map, 1, pinned);
    insert(&mut map, 2, binding(2));
    insert(&mut map, 3, binding(3));
    let mut health = BTreeMap::from([
        (session(1), BindingHealth::ProcessExited),
        (session(2), BindingHealth::Live),
        (session(3), BindingHealth::Unresolved),
    ]);
    let outcome = map
        .auto_bind(&enabled, binding(4), &health, map.revision())
        .unwrap();
    assert_eq!(outcome.slot, slot(4));
    map.auto_bind(&enabled, binding(5), &health, map.revision())
        .unwrap();
    map.auto_bind(&enabled, binding(6), &health, map.revision())
        .unwrap();
    let before = map.clone();
    assert_eq!(
        map.auto_bind(&enabled, binding(7), &health, map.revision()),
        Err(SlotMutationError::NoCapacity)
    );
    assert_eq!(map, before);

    health.insert(session(3), BindingHealth::PidReused);
    let outcome = map
        .auto_bind(&enabled, binding(7), &health, map.revision())
        .unwrap();
    assert_eq!(outcome.slot, slot(3));
    assert_eq!(map.get(slot(1)).unwrap().session_id(), session(1));
    assert_eq!(map.get(slot(2)).unwrap().session_id(), session(2));
    assert_eq!(map.get(slot(3)).unwrap().session_id(), session(7));

    let mut first_reusable = SlotMap::default();
    insert(&mut first_reusable, 1, binding(8));
    let health = BTreeMap::from([(session(8), BindingHealth::ProcessExited)]);
    let selected = first_reusable
        .auto_bind(&enabled, binding(9), &health, first_reusable.revision())
        .unwrap();
    assert_eq!(selected.slot, slot(1));
    assert!(first_reusable.get(slot(2)).is_none());
}

#[test]
fn revision_exhaustion_and_staleness_leave_the_whole_map_unchanged() {
    let id = session(1);
    let input = format!(
        r#"{{"version":1,"revision":18446744073709551615,"slots":{{"1":{}}}}}"#,
        binding_json(&id.to_string())
    );
    let mut map = SlotMap::from_json(input.as_bytes()).unwrap();
    let before = map.clone();
    let max = u64::MAX;
    assert_eq!(
        map.bind(slot(2), binding(2), max, None, None),
        Err(SlotMutationError::RevisionExhausted)
    );
    assert_eq!(
        map.unbind(slot(1), id, max),
        Err(SlotMutationError::RevisionExhausted)
    );
    assert_eq!(
        map.set_pinned(slot(1), true, max),
        Err(SlotMutationError::RevisionExhausted)
    );
    assert_eq!(
        map.set_custom_label(slot(1), Some("new"), max),
        Err(SlotMutationError::RevisionExhausted)
    );
    assert_eq!(
        map.auto_bind(&Settings::recommended(), binding(2), &BTreeMap::new(), max),
        Err(SlotMutationError::RevisionExhausted)
    );
    assert_eq!(map, before);
    let existing = map
        .auto_bind(&Settings::recommended(), binding(1), &BTreeMap::new(), max)
        .unwrap();
    assert!(!existing.newly_bound);
    assert_eq!(map, before);
    assert!(matches!(
        map.unbind(slot(1), id, max - 1),
        Err(SlotMutationError::StaleRevision { .. })
    ));
    assert_eq!(map, before);
}

#[test]
fn slot_map_roundtrip_and_standalone_byte_bound() {
    let mut map = SlotMap::default();
    insert(&mut map, 6, binding(6));
    insert(&mut map, 2, binding(2));
    assert_eq!(
        map.iter()
            .map(|(slot, _)| slot.number())
            .collect::<Vec<_>>(),
        [2, 6]
    );
    let json = serde_json::to_vec(&map).unwrap();
    assert_eq!(SlotMap::from_json(&json).unwrap(), map);
    let mut bounded = json;
    bounded.resize(MAX_CONFIG_BYTES, b' ');
    assert!(SlotMap::from_json(&bounded).is_ok());
    bounded.push(b' ');
    assert!(matches!(
        SlotMap::from_json(&bounded),
        Err(ConfigError::TooLarge { .. })
    ));
}
