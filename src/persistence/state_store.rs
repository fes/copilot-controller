//! Bounded, revision-checked snapshots and explicit crash recovery.

use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};

use serde::{Deserialize, Deserializer, Serialize, de};
use uuid::Uuid;

use crate::domain::{
    config::{Settings, SlotMap},
    identity::Slot,
};

use super::root::{
    Error as RootError, ErrorKind as RootErrorKind, LockGuard, Name, PrivateRoot, PublishMode,
};

pub const MAX_SNAPSHOT_BYTES: usize = 262_144;
const CURRENT: &str = "core-state.json";
const PENDING: &str = "core-state.pending.json";
const LOCK: &str = "core-state.lock";

fn current_name() -> Name {
    Name::new(CURRENT).expect("constant state basename")
}

fn pending_name() -> Name {
    Name::new(PENDING).expect("constant state basename")
}

fn replacement_name() -> Name {
    Name::new(format!("{PENDING}.replacement")).expect("constant recovery basename")
}

fn lock_name() -> Name {
    Name::new(LOCK).expect("constant lock basename")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    Root(RootError),
    NotInitialized,
    AlreadyInitialized,
    RecoveryRequired,
    CorruptSnapshot,
    SnapshotTooLarge {
        actual: usize,
        limit: usize,
    },
    InvalidSnapshot,
    StaleRevision {
        expected: Option<u64>,
        actual: Option<u64>,
    },
    RevisionExhausted,
    InvalidRecovery,
    MutationCommitIndeterminate,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root(error) => write!(f, "{error}"),
            Self::NotInitialized => f.write_str("state store is not initialized"),
            Self::AlreadyInitialized => f.write_str("state store is already initialized"),
            Self::RecoveryRequired => f.write_str("state recovery is required"),
            Self::CorruptSnapshot => f.write_str("state snapshot is corrupt or unsupported"),
            Self::SnapshotTooLarge { actual, limit } => {
                write!(f, "state snapshot is {actual} bytes; limit is {limit}")
            }
            Self::InvalidSnapshot => f.write_str("state snapshot failed validation"),
            Self::StaleRevision { expected, actual } => {
                write!(
                    f,
                    "stale state revision: expected {expected:?}, actual {actual:?}"
                )
            }
            Self::RevisionExhausted => f.write_str("state revision is exhausted"),
            Self::InvalidRecovery => f.write_str("recovery evidence does not form a valid plan"),
            Self::MutationCommitIndeterminate => {
                f.write_str("state publication outcome is indeterminate")
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Root(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RootError> for StoreError {
    fn from(error: RootError) -> Self {
        Self::Root(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    SessionStart,
    SessionEnd,
    UserPromptSubmitted,
    AgentStop,
    Notification,
    PostToolUse,
    ErrorOccurred,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    PermissionPrompt,
    ElicitationDialog,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedHook {
    #[serde(deserialize_with = "canonical_uuid")]
    pub session_id: Uuid,
    pub timestamp_ms: u64,
    pub kind: HookKind,
    #[serde(deserialize_with = "required_nullable")]
    pub notification: Option<NotificationKind>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessLifetime {
    pub pid: NonZeroU32,
    pub created_ms: NonZeroU64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingFork {
    #[serde(deserialize_with = "canonical_uuid")]
    pub id: Uuid,
    pub slot: Slot,
    #[serde(deserialize_with = "canonical_uuid")]
    pub source: Uuid,
    pub binding_revision: u64,
    pub process: ProcessLifetime,
    pub issued_ms: u64,
    pub event_offset: u64,
    #[serde(deserialize_with = "canonical_uuid_vec")]
    pub seen: Vec<Uuid>,
    #[serde(deserialize_with = "canonical_nullable_uuid")]
    pub child: Option<Uuid>,
    pub conflicted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SnapshotData {
    pub settings: Settings,
    pub bindings: SlotMap,
    pub pending_forks: Vec<PendingFork>,
    pub hooks: Vec<PersistedHook>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSnapshotData {
    settings: Settings,
    bindings: SlotMap,
    #[serde(default)]
    pending_forks: Vec<PendingFork>,
    #[serde(default)]
    hooks: Vec<PersistedHook>,
}

impl<'de> Deserialize<'de> for SnapshotData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawSnapshotData::deserialize(deserializer)?;
        let data = Self {
            settings: raw.settings,
            bindings: raw.bindings,
            pending_forks: raw.pending_forks,
            hooks: raw.hooks,
        };
        data.validate().map_err(de::Error::custom)?;
        Ok(data)
    }
}

impl SnapshotData {
    fn validate(&self) -> Result<(), StoreError> {
        if self.pending_forks.len() > 6 || self.hooks.len() > 256 {
            return Err(StoreError::InvalidSnapshot);
        }
        for (index, pending) in self.pending_forks.iter().enumerate() {
            if pending.id.is_nil()
                || pending.source.is_nil()
                || pending
                    .child
                    .is_some_and(|child| child.is_nil() || child == pending.source)
                || pending.seen.len() > 256
                || pending.seen.iter().any(|id| id.is_nil())
                || pending
                    .seen
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    != pending.seen.len()
                || pending
                    .child
                    .is_some_and(|child| pending.seen.contains(&child))
                || self.pending_forks[..index]
                    .iter()
                    .any(|prior| prior.id == pending.id || prior.slot == pending.slot)
            {
                return Err(StoreError::InvalidSnapshot);
            }
        }
        if self.hooks.iter().any(|hook| {
            hook.session_id.is_nil()
                || (hook.kind != HookKind::Notification && hook.notification.is_some())
        }) {
            return Err(StoreError::InvalidSnapshot);
        }
        Ok(())
    }
}

fn canonical_uuid<'de, D>(deserializer: D) -> Result<Uuid, D::Error>
where
    D: Deserializer<'de>,
{
    parse_canonical_uuid(String::deserialize(deserializer)?).map_err(de::Error::custom)
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn canonical_nullable_uuid<'de, D>(deserializer: D) -> Result<Option<Uuid>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(parse_canonical_uuid)
        .transpose()
        .map_err(de::Error::custom)
}

fn canonical_uuid_vec<'de, D>(deserializer: D) -> Result<Vec<Uuid>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(parse_canonical_uuid)
        .collect::<Result<Vec<_>, _>>()
        .map_err(de::Error::custom)
}

fn parse_canonical_uuid(text: String) -> Result<Uuid, &'static str> {
    let uuid = Uuid::parse_str(&text).map_err(|_| "invalid UUID")?;
    if uuid.hyphenated().to_string() != text {
        return Err("UUID must use lowercase hyphenated canonical form");
    }
    Ok(uuid)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Snapshot {
    version: u8,
    store_id: Uuid,
    revision: u64,
    data: SnapshotData,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSnapshot {
    version: u8,
    #[serde(deserialize_with = "canonical_uuid")]
    store_id: Uuid,
    revision: u64,
    data: SnapshotData,
}

impl<'de> Deserialize<'de> for Snapshot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawSnapshot::deserialize(deserializer)?;
        if raw.version != 1 || raw.store_id.is_nil() {
            return Err(de::Error::custom(
                "unsupported or invalid snapshot identity",
            ));
        }
        Ok(Self {
            version: 1,
            store_id: raw.store_id,
            revision: raw.revision,
            data: raw.data,
        })
    }
}

impl Snapshot {
    pub fn store_id(&self) -> Uuid {
        self.store_id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn data(&self) -> &SnapshotData {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut SnapshotData {
        &mut self.data
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryDecision {
    KeepCurrent,
    CompletePrepared,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryPlan {
    NoRecoveryRequired,
    EqualSnapshots {
        revision: u64,
    },
    Choices {
        current_revision: Option<u64>,
        prepared_revision: u64,
        keep_current: bool,
        complete_prepared: bool,
    },
}

#[derive(Clone)]
pub struct StateStore {
    root: PrivateRoot,
}

type RecoveryEvidence = (Option<Snapshot>, Option<Snapshot>, Vec<u8>);

impl StateStore {
    pub fn new(root: PrivateRoot) -> Self {
        Self { root }
    }

    pub fn initialize(&self) -> Result<Snapshot, StoreError> {
        let lock = self.lock()?;
        self.root.check_mutation_support()?;
        if !lock.is_empty_except_lock()? {
            return Err(StoreError::AlreadyInitialized);
        }
        let snapshot = Snapshot {
            version: 1,
            store_id: Uuid::new_v4(),
            revision: 0,
            data: SnapshotData::default(),
        };
        self.publish(&lock, &snapshot, PublishMode::Create)?;
        lock.validate().map_err(post_publication_error)?;
        Ok(snapshot)
    }

    pub fn load(&self) -> Result<Snapshot, StoreError> {
        let lock = self.lock()?;
        self.load_locked(&lock)
    }

    /// The closure executes once under the OS lock after comparing the disk
    /// revision. A mutation error never publishes state.
    pub fn transact<E>(
        &self,
        expected_revision: u64,
        mutation: impl FnOnce(&mut SnapshotData) -> Result<(), E>,
    ) -> Result<Snapshot, TransactionError<E>> {
        let lock = self.lock().map_err(TransactionError::Store)?;
        let mut current = self.load_locked(&lock).map_err(TransactionError::Store)?;
        if current.revision != expected_revision {
            return Err(TransactionError::Store(StoreError::StaleRevision {
                expected: Some(expected_revision),
                actual: Some(current.revision),
            }));
        }
        let next_revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| TransactionError::Store(StoreError::RevisionExhausted))?;
        mutation(&mut current.data).map_err(TransactionError::Mutation)?;
        current.data.validate().map_err(TransactionError::Store)?;
        current.revision = next_revision;
        self.publish(&lock, &current, PublishMode::Replace)
            .map_err(TransactionError::Store)?;
        lock.validate()
            .map_err(|error| TransactionError::Store(post_publication_error(error)))?;
        Ok(current)
    }

    pub fn plan_recovery(
        &self,
        expected_current_revision: Option<u64>,
    ) -> Result<RecoveryPlan, StoreError> {
        let lock = self.lock()?;
        let (current, prepared, _) = self.read_recovery_evidence(&lock)?;
        check_expected(
            expected_current_revision,
            current.as_ref().map(Snapshot::revision),
        )?;
        let Some(prepared) = prepared else {
            return Ok(RecoveryPlan::NoRecoveryRequired);
        };
        if let Some(current) = &current {
            if current == &prepared {
                return Ok(RecoveryPlan::EqualSnapshots {
                    revision: current.revision,
                });
            }
            if current.store_id != prepared.store_id
                || current.revision.checked_add(1) != Some(prepared.revision)
            {
                return Err(StoreError::InvalidRecovery);
            }
            Ok(RecoveryPlan::Choices {
                current_revision: Some(current.revision),
                prepared_revision: prepared.revision,
                keep_current: true,
                complete_prepared: true,
            })
        } else if prepared.revision == 0 {
            Ok(RecoveryPlan::Choices {
                current_revision: None,
                prepared_revision: prepared.revision,
                keep_current: false,
                complete_prepared: true,
            })
        } else {
            Err(StoreError::InvalidRecovery)
        }
    }

    pub fn recover(
        &self,
        expected_current_revision: Option<u64>,
        decision: RecoveryDecision,
    ) -> Result<Snapshot, StoreError> {
        let lock = self.lock()?;
        self.root.check_mutation_support()?;
        let (current, prepared, prepared_bytes) = self.read_recovery_evidence(&lock)?;
        check_expected(
            expected_current_revision,
            current.as_ref().map(Snapshot::revision),
        )?;
        let prepared = prepared.ok_or(StoreError::InvalidRecovery)?;

        if let Some(current) = &current {
            if current == &prepared {
                lock.sync_file(&current_name())?;
                lock.discard_prepared(&pending_name(), &prepared_bytes, MAX_SNAPSHOT_BYTES)
                    .map_err(recovery_root_error)?;
                lock.validate().map_err(post_publication_error)?;
                return Ok(current.clone());
            }
            if current.store_id != prepared.store_id
                || current.revision.checked_add(1) != Some(prepared.revision)
            {
                return Err(StoreError::InvalidRecovery);
            }
        } else if prepared.revision != 0 || decision == RecoveryDecision::KeepCurrent {
            return Err(StoreError::InvalidRecovery);
        }

        match decision {
            RecoveryDecision::KeepCurrent => {
                let current = current.ok_or(StoreError::InvalidRecovery)?;
                lock.discard_prepared(&pending_name(), &prepared_bytes, MAX_SNAPSHOT_BYTES)
                    .map_err(recovery_root_error)?;
                lock.validate().map_err(post_publication_error)?;
                Ok(current)
            }
            RecoveryDecision::CompletePrepared => {
                let mode = if current.is_some() {
                    PublishMode::Replace
                } else {
                    PublishMode::Create
                };
                lock.publish_prepared(
                    &pending_name(),
                    &current_name(),
                    mode,
                    &prepared_bytes,
                    MAX_SNAPSHOT_BYTES,
                )
                .map_err(recovery_root_error)?;
                lock.validate().map_err(post_publication_error)?;
                Ok(prepared)
            }
        }
    }

    fn lock(&self) -> Result<LockGuard, StoreError> {
        self.root
            .lock_bounded(&lock_name())
            .map_err(StoreError::Root)
    }

    fn load_locked(&self, lock: &LockGuard) -> Result<Snapshot, StoreError> {
        lock.validate()?;
        if self.root.open_read(&pending_name())?.is_some()
            || self.root.open_read(&replacement_name())?.is_some()
        {
            return Err(StoreError::RecoveryRequired);
        }
        let bytes = read_bounded_snapshot(&self.root, &current_name())?
            .ok_or(StoreError::NotInitialized)?;
        let snapshot = decode_snapshot(&bytes)?;
        lock.validate()?;
        Ok(snapshot)
    }

    fn read_recovery_evidence(&self, lock: &LockGuard) -> Result<RecoveryEvidence, StoreError> {
        lock.validate()?;
        let current_bytes = read_bounded_snapshot(&self.root, &current_name())?;
        let has_prepared = self.root.open_read(&pending_name())?.is_some();
        let has_replacement = self.root.open_read(&replacement_name())?.is_some();
        let current = current_bytes.as_deref().map(decode_snapshot).transpose()?;
        if !has_prepared {
            if has_replacement {
                return Err(StoreError::RecoveryRequired);
            }
            return Ok((current, None, Vec::new()));
        }
        let prepared_bytes = read_bounded_snapshot(&self.root, &pending_name())?
            .ok_or(StoreError::RecoveryRequired)?;
        let replacement_bytes = if has_replacement {
            Some(
                read_bounded_snapshot(&self.root, &replacement_name())?
                    .ok_or(StoreError::RecoveryRequired)?,
            )
        } else {
            None
        };
        let prepared = decode_snapshot(&prepared_bytes)?;
        if replacement_bytes
            .as_ref()
            .is_some_and(|replacement| replacement != &prepared_bytes)
        {
            return Err(StoreError::RecoveryRequired);
        }
        lock.validate()?;
        Ok((current, Some(prepared), prepared_bytes))
    }

    fn publish(
        &self,
        lock: &LockGuard,
        snapshot: &Snapshot,
        mode: PublishMode,
    ) -> Result<(), StoreError> {
        let bytes = encode_snapshot(snapshot)?;
        self.root.check_mutation_support()?;
        lock.create_exclusive(&pending_name(), &bytes)
            .map_err(map_publication_error)?;
        lock.publish_prepared(
            &pending_name(),
            &current_name(),
            mode,
            &bytes,
            MAX_SNAPSHOT_BYTES,
        )
        .map_err(map_publication_error)
    }
}

#[derive(Debug)]
pub enum TransactionError<E> {
    Store(StoreError),
    Mutation(E),
}

fn encode_snapshot(snapshot: &Snapshot) -> Result<Vec<u8>, StoreError> {
    snapshot.data.validate()?;
    let bytes = serde_json::to_vec(snapshot).map_err(|_| StoreError::InvalidSnapshot)?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(StoreError::SnapshotTooLarge {
            actual: bytes.len(),
            limit: MAX_SNAPSHOT_BYTES,
        });
    }
    Ok(bytes)
}

fn decode_snapshot(bytes: &[u8]) -> Result<Snapshot, StoreError> {
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(StoreError::SnapshotTooLarge {
            actual: bytes.len(),
            limit: MAX_SNAPSHOT_BYTES,
        });
    }
    serde_json::from_slice(bytes).map_err(|_| StoreError::CorruptSnapshot)
}

fn read_bounded_snapshot(root: &PrivateRoot, name: &Name) -> Result<Option<Vec<u8>>, StoreError> {
    root.read(name, MAX_SNAPSHOT_BYTES).map_err(|error| {
        if error.kind() == RootErrorKind::TooLarge {
            StoreError::SnapshotTooLarge {
                actual: MAX_SNAPSHOT_BYTES + 1,
                limit: MAX_SNAPSHOT_BYTES,
            }
        } else {
            StoreError::Root(error)
        }
    })
}

fn check_expected(expected: Option<u64>, actual: Option<u64>) -> Result<(), StoreError> {
    if expected == actual {
        Ok(())
    } else {
        Err(StoreError::StaleRevision { expected, actual })
    }
}

fn map_publication_error(error: RootError) -> StoreError {
    if matches!(
        error.kind(),
        RootErrorKind::Indeterminate | RootErrorKind::CleanupUncertain
    ) {
        StoreError::MutationCommitIndeterminate
    } else {
        StoreError::Root(error)
    }
}

fn post_publication_error(_error: RootError) -> StoreError {
    StoreError::MutationCommitIndeterminate
}

fn recovery_root_error(error: RootError) -> StoreError {
    if matches!(
        error.kind(),
        RootErrorKind::Indeterminate | RootErrorKind::CleanupUncertain
    ) {
        StoreError::MutationCommitIndeterminate
    } else if error.kind() == RootErrorKind::EvidenceMismatch {
        StoreError::RecoveryRequired
    } else {
        StoreError::Root(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn offline_snapshot_validation_is_strict_and_bounded() {
        let snapshot = Snapshot {
            version: 1,
            store_id: Uuid::from_u128(1),
            revision: 0,
            data: SnapshotData::default(),
        };
        let encoded = encode_snapshot(&snapshot).unwrap();
        assert_eq!(decode_snapshot(&encoded).unwrap(), snapshot);
        let valid = serde_json::to_value(&snapshot).unwrap();
        for (field, invalid) in [
            ("version", serde_json::json!(2)),
            ("store_id", serde_json::json!(Uuid::nil())),
            ("revision", serde_json::json!(0.5)),
            ("unexpected", serde_json::json!(true)),
        ] {
            let mut value = valid.clone();
            value[field] = invalid;
            assert!(decode_snapshot(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let mut unknown_nested = valid;
        unknown_nested["data"]["unexpected"] = serde_json::json!(true);
        assert!(decode_snapshot(&serde_json::to_vec(&unknown_nested).unwrap()).is_err());
        assert!(matches!(
            decode_snapshot(&vec![b' '; MAX_SNAPSHOT_BYTES + 1]),
            Err(StoreError::SnapshotTooLarge { .. })
        ));
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let base = PathBuf::from(
                std::env::var_os("COPILOT_PRIVATE_ROOT_FIXTURES")
                    .expect("set COPILOT_PRIVATE_ROOT_FIXTURES to an approved directory"),
            );
            assert!(base.is_absolute());
            assert_eq!(fs::canonicalize(&base).unwrap(), base);
            let path = base.join(format!(
                "store-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            private_dir(&path);
            Self(path)
        }

        fn store(&self) -> StateStore {
            let root = PrivateRoot::open(&self.0).unwrap();
            root.check_mutation_support().unwrap();
            StateStore::new(root)
        }

        fn root(&self) -> PrivateRoot {
            PrivateRoot::open(&self.0).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn private_dir(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        #[cfg(windows)]
        super::super::root::create_test_private_directory(path).unwrap();
    }

    fn prepared(store: &StateStore, revision: u64) -> (Snapshot, Vec<u8>) {
        let current = store.load().unwrap();
        let mut next = current;
        next.revision = revision;
        let bytes = encode_snapshot(&next).unwrap();
        let lock = store.lock().unwrap();
        lock.create_exclusive(&pending_name(), &bytes).unwrap();
        (next, bytes)
    }

    #[test]
    #[ignore = "requires explicitly approved native mutation filesystem; unsupported on Windows"]
    fn initialization_transaction_and_stale_revision_are_checked_on_disk() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = store.initialize().unwrap();
        assert_eq!(initial.revision(), 0);
        assert!(matches!(
            store.initialize(),
            Err(StoreError::AlreadyInitialized)
        ));

        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let updated = store
            .transact(0, move |data| -> Result<(), ()> {
                counted.fetch_add(1, Ordering::SeqCst);
                data.hooks.push(PersistedHook {
                    session_id: Uuid::new_v4(),
                    timestamp_ms: 10,
                    kind: HookKind::SessionStart,
                    notification: None,
                });
                Ok(())
            })
            .unwrap();
        assert_eq!(updated.revision(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let stale_calls = calls.clone();
        let stale_result = store.transact(0, move |_| -> Result<(), ()> {
            stale_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(
            matches!(
                &stale_result,
                Err(TransactionError::Store(StoreError::StaleRevision {
                    expected: Some(0),
                    actual: Some(1)
                }))
            ),
            "{stale_result:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let failing = store.transact(1, |_| Err::<(), _>("rejected"));
        assert!(matches!(
            failing,
            Err(TransactionError::Mutation("rejected"))
        ));
        assert_eq!(store.load().unwrap().revision(), 1);
    }

    #[test]
    #[ignore = "requires explicitly approved native mutation filesystem; unsupported on Windows"]
    fn competing_writers_have_one_winner_and_one_stale_revision() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store.initialize().unwrap();
        let first = store.clone();
        let second = store.clone();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for writer in [first, second] {
            let barrier = barrier.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                writer.transact(0, |data| -> Result<(), ()> {
                    data.hooks.push(PersistedHook {
                        session_id: Uuid::new_v4(),
                        timestamp_ms: 1,
                        kind: HookKind::SessionStart,
                        notification: None,
                    });
                    Ok(())
                })
            }));
        }
        barrier.wait();
        let mut results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        for result in &mut results {
            if matches!(
                result,
                Err(TransactionError::Store(StoreError::Root(error)))
                    if error.kind() == RootErrorKind::Busy
            ) {
                *result = store.transact(0, |data| -> Result<(), ()> {
                    data.hooks.push(PersistedHook {
                        session_id: Uuid::new_v4(),
                        timestamp_ms: 1,
                        kind: HookKind::SessionStart,
                        notification: None,
                    });
                    Ok(())
                });
            }
        }
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(TransactionError::Store(StoreError::StaleRevision { .. }))
                ))
                .count(),
            1,
            "{results:?}"
        );
        assert_eq!(store.load().unwrap().revision(), 1);
    }

    #[test]
    #[ignore = "requires explicitly approved native mutation filesystem; unsupported on Windows"]
    fn corruption_and_unknown_schema_are_preserved_without_recovery_guessing() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store.initialize().unwrap();
        let root = fixture.root();
        let current = current_name();
        let original = br#"{"version":1,"store_id":"00000000-0000-0000-0000-000000000001","revision":0,"data":{"settings":{"version":1},"bindings":{"version":1,"revision":0,"slots":{}},"unknown":true}}"#;
        let lock = root.lock_bounded(&lock_name()).unwrap();
        lock.create_exclusive(&pending_name(), original).unwrap();
        lock.publish_prepared(
            &pending_name(),
            &current,
            PublishMode::Replace,
            original,
            MAX_SNAPSHOT_BYTES,
        )
        .unwrap();
        drop(lock);

        assert!(matches!(store.load(), Err(StoreError::CorruptSnapshot)));
        assert_eq!(
            root.read(&current, MAX_SNAPSHOT_BYTES).unwrap().unwrap(),
            original
        );
    }

    #[test]
    #[ignore = "requires explicitly approved native mutation filesystem; unsupported on Windows"]
    fn recovery_plans_cover_keep_complete_equal_and_crash_artifacts() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let current = store.initialize().unwrap();
        let (_, _) = prepared(&store, 1);
        assert_eq!(
            store.plan_recovery(Some(0)).unwrap(),
            RecoveryPlan::Choices {
                current_revision: Some(0),
                prepared_revision: 1,
                keep_current: true,
                complete_prepared: true,
            }
        );
        let kept = store
            .recover(Some(0), RecoveryDecision::KeepCurrent)
            .unwrap();
        assert_eq!(kept.revision(), 0);
        assert!(store.load().is_ok());

        let (_, pending_bytes) = prepared(&store, 1);
        let completed = store
            .recover(Some(0), RecoveryDecision::CompletePrepared)
            .unwrap();
        assert_eq!(completed.revision(), 1);
        assert_eq!(completed.store_id(), current.store_id());

        let root = fixture.root();
        let lock = root.lock_bounded(&lock_name()).unwrap();
        lock.create_exclusive(&pending_name(), &pending_bytes)
            .unwrap();
        lock.create_exclusive(&replacement_name(), &pending_bytes)
            .unwrap();
        drop(lock);
        assert_eq!(
            store.plan_recovery(Some(1)).unwrap(),
            RecoveryPlan::EqualSnapshots { revision: 1 }
        );
        let equal = store
            .recover(Some(1), RecoveryDecision::KeepCurrent)
            .unwrap();
        assert_eq!(equal, prepared_snapshot_of(&store, 1));
        assert_eq!(equal.revision(), 1);
    }

    #[test]
    #[ignore = "requires explicitly approved native mutation filesystem; unsupported on Windows"]
    fn recovery_can_explicitly_complete_an_uncommitted_initialization_only() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = Snapshot {
            version: 1,
            store_id: Uuid::new_v4(),
            revision: 0,
            data: SnapshotData::default(),
        };
        let bytes = encode_snapshot(&initial).unwrap();
        let lock = store.lock().unwrap();
        lock.create_exclusive(&pending_name(), &bytes).unwrap();
        drop(lock);

        assert_eq!(
            store.plan_recovery(None).unwrap(),
            RecoveryPlan::Choices {
                current_revision: None,
                prepared_revision: 0,
                keep_current: false,
                complete_prepared: true,
            }
        );
        assert!(matches!(
            store.recover(None, RecoveryDecision::KeepCurrent),
            Err(StoreError::InvalidRecovery)
        ));
        let recovered = store
            .recover(None, RecoveryDecision::CompletePrepared)
            .unwrap();
        assert_eq!(recovered, initial);
        assert_eq!(store.load().unwrap(), initial);
    }

    fn prepared_snapshot_of(store: &StateStore, revision: u64) -> Snapshot {
        let current = store.load().unwrap();
        let mut snapshot = current;
        snapshot.revision = revision;
        snapshot
    }

    #[test]
    #[ignore = "requires explicitly approved native mutation filesystem; unsupported on Windows"]
    fn partial_or_orphaned_or_mismatching_evidence_is_never_discarded() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store.initialize().unwrap();
        let root = fixture.root();
        let lock = root.lock_bounded(&lock_name()).unwrap();
        lock.create_exclusive(&pending_name(), b"{partial").unwrap();
        drop(lock);
        let evidence = root
            .read(&pending_name(), MAX_SNAPSHOT_BYTES)
            .unwrap()
            .unwrap();
        assert!(matches!(store.load(), Err(StoreError::RecoveryRequired)));
        assert!(matches!(
            store.plan_recovery(Some(0)),
            Err(StoreError::CorruptSnapshot)
        ));
        assert_eq!(
            root.read(&pending_name(), MAX_SNAPSHOT_BYTES)
                .unwrap()
                .unwrap(),
            evidence
        );

        let fixture = Fixture::new();
        let store = fixture.store();
        store.initialize().unwrap();
        let root = fixture.root();
        root.create_exclusive(&replacement_name(), b"orphan")
            .unwrap();
        assert!(matches!(store.load(), Err(StoreError::RecoveryRequired)));
        assert!(matches!(
            store.recover(Some(0), RecoveryDecision::KeepCurrent),
            Err(StoreError::RecoveryRequired)
        ));
        assert_eq!(
            root.read(&replacement_name(), MAX_SNAPSHOT_BYTES)
                .unwrap()
                .unwrap(),
            b"orphan"
        );
    }

    #[test]
    #[ignore = "requires explicitly approved native mutation filesystem; unsupported on Windows"]
    fn recovery_rejects_unrelated_store_and_replacement_mismatch() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store.initialize().unwrap();
        let (_, bytes) = prepared(&store, 1);
        let root = fixture.root();
        let mut unrelated: Snapshot = serde_json::from_slice(&bytes).unwrap();
        unrelated.store_id = Uuid::new_v4();
        let unrelated_bytes = encode_snapshot(&unrelated).unwrap();
        let lock = root.lock_bounded(&lock_name()).unwrap();
        lock.discard_prepared(&pending_name(), &bytes, MAX_SNAPSHOT_BYTES)
            .unwrap();
        lock.create_exclusive(&pending_name(), &unrelated_bytes)
            .unwrap();
        lock.create_exclusive(&replacement_name(), &bytes).unwrap();
        drop(lock);
        assert!(matches!(
            store.recover(Some(0), RecoveryDecision::CompletePrepared),
            Err(StoreError::RecoveryRequired)
        ));
        assert_eq!(
            root.read(&pending_name(), MAX_SNAPSHOT_BYTES)
                .unwrap()
                .unwrap(),
            unrelated_bytes
        );
        assert_eq!(
            root.read(&replacement_name(), MAX_SNAPSHOT_BYTES)
                .unwrap()
                .unwrap(),
            bytes
        );

        let fixture = Fixture::new();
        let store = fixture.store();
        store.initialize().unwrap();
        let (_, valid_bytes) = prepared(&store, 1);
        let root = fixture.root();
        let mut unrelated: Snapshot = serde_json::from_slice(&valid_bytes).unwrap();
        unrelated.store_id = Uuid::new_v4();
        let unrelated_bytes = encode_snapshot(&unrelated).unwrap();
        let lock = root.lock_bounded(&lock_name()).unwrap();
        lock.discard_prepared(&pending_name(), &valid_bytes, MAX_SNAPSHOT_BYTES)
            .unwrap();
        lock.create_exclusive(&pending_name(), &unrelated_bytes)
            .unwrap();
        drop(lock);
        assert!(matches!(
            store.recover(Some(0), RecoveryDecision::CompletePrepared),
            Err(StoreError::InvalidRecovery)
        ));
        assert_eq!(
            root.read(&pending_name(), MAX_SNAPSHOT_BYTES)
                .unwrap()
                .unwrap(),
            unrelated_bytes
        );
    }

    #[test]
    #[ignore = "requires explicitly approved native mutation filesystem; unsupported on Windows"]
    fn serialized_state_is_bounded_and_invalid_mutations_publish_nothing() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store.initialize().unwrap();
        let failed = store.transact(0, |data| -> Result<(), ()> {
            data.hooks.resize_with(257, || PersistedHook {
                session_id: Uuid::new_v4(),
                timestamp_ms: 1,
                kind: HookKind::SessionStart,
                notification: None,
            });
            Ok(())
        });
        assert!(matches!(
            failed,
            Err(TransactionError::Store(StoreError::InvalidSnapshot))
        ));
        assert_eq!(store.load().unwrap().revision(), 0);
        let oversized = vec![b' '; MAX_SNAPSHOT_BYTES + 1];
        let root = fixture.root();
        let lock = root.lock_bounded(&lock_name()).unwrap();
        lock.create_exclusive(&pending_name(), &oversized).unwrap();
        drop(lock);
        assert!(matches!(store.load(), Err(StoreError::RecoveryRequired)));
        assert_eq!(
            root.read(&pending_name(), MAX_SNAPSHOT_BYTES + 1)
                .unwrap()
                .unwrap(),
            oversized
        );
    }
}
