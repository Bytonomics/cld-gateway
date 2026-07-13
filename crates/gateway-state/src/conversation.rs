use crate::{StateError, default_gateway_dir};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

const SESSION_SCHEMA_VERSION: u32 = 1;
const BRANCH_SCHEMA_VERSION: u32 = 1;
const SPARSE_CHECKPOINT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaudeSessionMetadata {
    pub schema_version: u32,
    pub claude_session_id: String,
    pub created_at_unix_seconds: i64,
    pub updated_at_unix_seconds: i64,
    pub branch_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BranchFingerprintSet {
    pub recent_message_tail_hash: Option<String>,
    pub last_user_message_hash: Option<String>,
    pub compaction_summary_hash: Option<String>,
    pub branch_state_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAiCheckpoint {
    pub response_id: String,
    pub previous_response_id: Option<String>,
    pub provider_model_fingerprint: String,
    pub request_compatibility_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchCheckpointRef {
    pub branch_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchMetadata {
    pub schema_version: u32,
    pub branch_id: String,
    pub parent_branch_id: Option<String>,
    pub fork_ancestor_checkpoint: Option<BranchCheckpointRef>,
    pub current_checkpoint_id: Option<String>,
    pub active_canonical_messages: Option<serde_json::Value>,
    pub fingerprints: BranchFingerprintSet,
    pub openai_checkpoint: Option<OpenAiCheckpoint>,
    pub compaction_reset_pending: bool,
    pub last_main_turn_id: Option<String>,
    pub created_at_unix_seconds: i64,
    pub updated_at_unix_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchCreateParams {
    pub parent_branch_id: Option<String>,
    pub fork_ancestor_checkpoint: Option<BranchCheckpointRef>,
    pub active_canonical_messages: Option<serde_json::Value>,
    pub fingerprints: BranchFingerprintSet,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationTurnScope {
    Main,
    Side,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchSelectionInput {
    pub active_canonical_messages: Option<serde_json::Value>,
    pub fingerprints: BranchFingerprintSet,
    pub turn_scope: ConversationTurnScope,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BranchSelectionAction {
    CreatedInitial,
    ContinuedExisting,
    ForkedFromAncestor,
    CreatedAmbiguous,
    CreatedUnmatched,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchSelectionResult {
    pub branch: BranchMetadata,
    pub action: BranchSelectionAction,
    pub matched_existing_branch: Option<BranchMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitTurnParams {
    pub turn_scope: ConversationTurnScope,
    pub turn_id: String,
    pub fingerprints: BranchFingerprintSet,
    pub provider_response_id: Option<String>,
    pub previous_response_id: Option<String>,
    pub provider_model_fingerprint: Option<String>,
    pub request_compatibility_fingerprint: Option<String>,
    pub provider_output_items: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconcileSnapshotParams {
    pub messages: serde_json::Value,
    pub fingerprints: BranchFingerprintSet,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SparseCheckpointKind {
    BranchCreated,
    BranchForkCreated,
    CompactionApplied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SparseCheckpoint {
    pub schema_version: u32,
    pub checkpoint_kind: SparseCheckpointKind,
    pub branch_id: String,
    pub current_checkpoint_id: Option<String>,
    pub parent_branch_id: Option<String>,
    pub fork_ancestor_checkpoint: Option<BranchCheckpointRef>,
    pub active_canonical_messages: Option<serde_json::Value>,
    pub fingerprints: BranchFingerprintSet,
    pub compaction_reset_pending: bool,
    pub last_main_turn_id: Option<String>,
    pub created_at_unix_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum CanonicalLedgerEvent {
    BranchCreated {
        branch_id: String,
        parent_branch_id: Option<String>,
        fork_ancestor_checkpoint: Option<BranchCheckpointRef>,
        created_at_unix_seconds: i64,
    },
    InboundCanonicalSnapshotReconciled {
        snapshot_hash: Option<String>,
        created_at_unix_seconds: i64,
    },
    MainTurnCommitted {
        turn_id: String,
        provider_response_id: Option<String>,
        request_fingerprint: Option<String>,
        provider_output_items: Vec<serde_json::Value>,
        created_at_unix_seconds: i64,
    },
    SideTurnObserved {
        turn_id: String,
        request_fingerprint: Option<String>,
        provider_output_items: Vec<serde_json::Value>,
        created_at_unix_seconds: i64,
    },
    CompactionApplied {
        summary_hash: Option<String>,
        created_at_unix_seconds: i64,
    },
}

#[derive(Clone)]
pub struct ConversationStateStore {
    root: PathBuf,
    corruption_policy: gateway_core::config::ConversationStateCorruptionPolicy,
}

type SessionLockMap = Mutex<HashMap<String, Arc<Mutex<()>>>>;

fn session_lock_registry() -> &'static SessionLockMap {
    static REGISTRY: OnceLock<SessionLockMap> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

impl Default for ConversationStateStore {
    fn default() -> Self {
        Self {
            root: default_conversation_state_root()
                .unwrap_or_else(|_| PathBuf::from(".gateway/sessions/claudecode")),
            corruption_policy: gateway_core::config::ConversationStateCorruptionPolicy::FailClosed,
        }
    }
}

impl ConversationStateStore {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self::new_with_policy(
            root,
            gateway_core::config::ConversationStateCorruptionPolicy::FailClosed,
        )
    }

    #[must_use]
    pub fn new_with_policy(
        root: &Path,
        corruption_policy: gateway_core::config::ConversationStateCorruptionPolicy,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            corruption_policy,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn session_dir(&self, claude_session_id: &str) -> PathBuf {
        self.root.join(format!("session-id-{claude_session_id}"))
    }

    #[must_use]
    pub fn branch_dir(&self, claude_session_id: &str, branch_id: &str) -> PathBuf {
        self.session_dir(claude_session_id)
            .join(format!("tab-{branch_id}"))
    }

    #[must_use]
    pub fn branch_checkpoints_dir(&self, claude_session_id: &str, branch_id: &str) -> PathBuf {
        self.branch_dir(claude_session_id, branch_id)
            .join("checkpoints")
    }

    /// # Errors
    /// Returns an error if the session metadata cannot be created or written.
    pub fn ensure_session(
        &self,
        claude_session_id: &str,
    ) -> Result<ClaudeSessionMetadata, StateError> {
        self.with_session_lock(claude_session_id, |store| {
            store.ensure_session_unlocked(claude_session_id)
        })
    }

    fn ensure_session_unlocked(
        &self,
        claude_session_id: &str,
    ) -> Result<ClaudeSessionMetadata, StateError> {
        let session_dir = self.session_dir(claude_session_id);
        std::fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("session.json");
        if session_path.exists() {
            return self.load_session_unlocked(claude_session_id);
        }

        let now = now_unix_seconds();
        let session = ClaudeSessionMetadata {
            schema_version: SESSION_SCHEMA_VERSION,
            claude_session_id: claude_session_id.to_string(),
            created_at_unix_seconds: now,
            updated_at_unix_seconds: now,
            branch_ids: Vec::new(),
        };
        write_json_atomically(&session_path, &session)?;
        Ok(session)
    }

    /// # Errors
    /// Returns an error if the session metadata cannot be read or parsed.
    pub fn load_session(
        &self,
        claude_session_id: &str,
    ) -> Result<ClaudeSessionMetadata, StateError> {
        self.load_session_unlocked(claude_session_id)
    }

    fn load_session_unlocked(
        &self,
        claude_session_id: &str,
    ) -> Result<ClaudeSessionMetadata, StateError> {
        let session_path = self.session_dir(claude_session_id).join("session.json");
        let bytes = std::fs::read(session_path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// # Errors
    /// Returns an error if branch metadata cannot be read or parsed.
    pub fn load_all_branches(
        &self,
        claude_session_id: &str,
    ) -> Result<Vec<BranchMetadata>, StateError> {
        let session = self.ensure_session_unlocked(claude_session_id)?;
        session
            .branch_ids
            .iter()
            .map(|branch_id| self.load_branch(claude_session_id, branch_id))
            .collect()
    }

    /// # Errors
    /// Returns an error if session metadata cannot be read or if expired session directories
    /// cannot be removed.
    pub fn cleanup_sessions_older_than_days(
        &self,
        max_session_age_days: u64,
    ) -> Result<usize, StateError> {
        let Some(max_age_seconds) = max_session_age_days
            .checked_mul(24)
            .and_then(|hours| hours.checked_mul(60))
            .and_then(|minutes| minutes.checked_mul(60))
            .and_then(|seconds| i64::try_from(seconds).ok())
        else {
            return Ok(0);
        };
        let now = now_unix_seconds();
        self.cleanup_sessions_older_than_unix_seconds(now - max_age_seconds)
    }

    fn cleanup_sessions_older_than_unix_seconds(
        &self,
        cutoff_unix_seconds: i64,
    ) -> Result<usize, StateError> {
        if !self.root.exists() {
            return Ok(0);
        }

        let mut removed = 0usize;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(file_name) = entry.file_name().to_str().map(ToString::to_string) else {
                continue;
            };
            let Some(claude_session_id) = file_name.strip_prefix("session-id-") else {
                continue;
            };
            let session = self.load_session_unlocked(claude_session_id)?;
            if session.updated_at_unix_seconds > cutoff_unix_seconds {
                continue;
            }
            std::fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
        Ok(removed)
    }

    fn load_all_branches_for_selection_unlocked(
        &self,
        claude_session_id: &str,
    ) -> Result<Vec<BranchMetadata>, StateError> {
        let session = self.ensure_session_unlocked(claude_session_id)?;
        session
            .branch_ids
            .iter()
            .map(|branch_id| self.rebuild_branch_from_disk(claude_session_id, branch_id))
            .collect()
    }

    /// # Errors
    /// Returns an error if the branch metadata cannot be written or the session cannot be updated.
    pub fn create_branch(
        &self,
        claude_session_id: &str,
        params: &BranchCreateParams,
    ) -> Result<BranchMetadata, StateError> {
        self.with_session_lock(claude_session_id, |store| {
            store.create_branch_unlocked(claude_session_id, params)
        })
    }

    fn create_branch_unlocked(
        &self,
        claude_session_id: &str,
        params: &BranchCreateParams,
    ) -> Result<BranchMetadata, StateError> {
        let mut session = self.ensure_session_unlocked(claude_session_id)?;
        let branch_id = Uuid::new_v4().to_string();
        let branch_dir = self.branch_dir(claude_session_id, &branch_id);
        std::fs::create_dir_all(branch_dir.join("checkpoints"))?;
        let now = now_unix_seconds();

        let branch = BranchMetadata {
            schema_version: BRANCH_SCHEMA_VERSION,
            branch_id: branch_id.clone(),
            parent_branch_id: params.parent_branch_id.clone(),
            fork_ancestor_checkpoint: params.fork_ancestor_checkpoint.clone(),
            current_checkpoint_id: None,
            active_canonical_messages: params.active_canonical_messages.clone(),
            fingerprints: params.fingerprints.clone(),
            openai_checkpoint: None,
            compaction_reset_pending: false,
            last_main_turn_id: None,
            created_at_unix_seconds: now,
            updated_at_unix_seconds: now,
        };

        let branch_path = self
            .branch_dir(claude_session_id, &branch_id)
            .join("branch.json");
        write_json_atomically(&branch_path, &branch)?;
        let branch_created = CanonicalLedgerEvent::BranchCreated {
            branch_id: branch_id.clone(),
            parent_branch_id: params.parent_branch_id.clone(),
            fork_ancestor_checkpoint: params.fork_ancestor_checkpoint.clone(),
            created_at_unix_seconds: now,
        };
        self.append_ledger_event_unlocked(claude_session_id, &branch_id, &branch_created)?;
        self.write_sparse_checkpoint_unlocked(
            claude_session_id,
            &branch,
            if params.parent_branch_id.is_some() {
                SparseCheckpointKind::BranchForkCreated
            } else {
                SparseCheckpointKind::BranchCreated
            },
            now,
        )?;

        session.branch_ids.push(branch_id);
        session.updated_at_unix_seconds = now;
        self.write_session(&session)?;
        Ok(branch)
    }

    /// # Errors
    /// Returns an error if session or branch metadata cannot be loaded or written.
    pub fn select_or_create_branch(
        &self,
        claude_session_id: &str,
        input: &BranchSelectionInput,
    ) -> Result<BranchSelectionResult, StateError> {
        self.with_session_lock(claude_session_id, |store| {
            store.select_or_create_branch_unlocked(claude_session_id, input)
        })
    }

    fn select_or_create_branch_unlocked(
        &self,
        claude_session_id: &str,
        input: &BranchSelectionInput,
    ) -> Result<BranchSelectionResult, StateError> {
        let existing = self.load_all_branches_for_selection_unlocked(claude_session_id)?;
        if existing.is_empty() {
            let branch = self.create_branch_unlocked(
                claude_session_id,
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: input.active_canonical_messages.clone(),
                    fingerprints: input.fingerprints.clone(),
                },
            )?;
            return Ok(BranchSelectionResult {
                branch,
                action: BranchSelectionAction::CreatedInitial,
                matched_existing_branch: None,
            });
        }

        let exact_matches = existing
            .iter()
            .filter(|branch| exact_branch_match(&branch.fingerprints, &input.fingerprints))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(mut branch) = (exact_matches.len() == 1).then(|| exact_matches[0].clone()) {
            let previous_branch = branch.clone();
            branch
                .active_canonical_messages
                .clone_from(&input.active_canonical_messages);
            branch.fingerprints = input.fingerprints.clone();
            self.store_branch_unlocked(claude_session_id, &branch)?;
            return Ok(BranchSelectionResult {
                branch,
                action: BranchSelectionAction::ContinuedExisting,
                matched_existing_branch: Some(previous_branch),
            });
        }
        if exact_matches.len() > 1 {
            let branch = self.create_branch_unlocked(
                claude_session_id,
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: input.active_canonical_messages.clone(),
                    fingerprints: input.fingerprints.clone(),
                },
            )?;
            return Ok(BranchSelectionResult {
                branch,
                action: BranchSelectionAction::CreatedAmbiguous,
                matched_existing_branch: None,
            });
        }

        let ancestor_candidates = existing
            .iter()
            .filter(|branch| ancestor_branch_match(&branch.fingerprints, &input.fingerprints))
            .cloned()
            .collect::<Vec<_>>();
        if let [ancestor] = ancestor_candidates.as_slice() {
            let branch = self.create_branch_unlocked(
                claude_session_id,
                &BranchCreateParams {
                    parent_branch_id: Some(ancestor.branch_id.clone()),
                    fork_ancestor_checkpoint: ancestor.current_checkpoint_id.as_ref().map(|id| {
                        BranchCheckpointRef {
                            branch_id: ancestor.branch_id.clone(),
                            checkpoint_id: id.clone(),
                        }
                    }),
                    active_canonical_messages: ancestor.active_canonical_messages.clone(),
                    fingerprints: input.fingerprints.clone(),
                },
            )?;
            return Ok(BranchSelectionResult {
                branch,
                action: BranchSelectionAction::ForkedFromAncestor,
                matched_existing_branch: Some(ancestor.clone()),
            });
        }

        let action = if ancestor_candidates.is_empty() {
            BranchSelectionAction::CreatedUnmatched
        } else {
            BranchSelectionAction::CreatedAmbiguous
        };
        let branch = self.create_branch_unlocked(
            claude_session_id,
            &BranchCreateParams {
                parent_branch_id: None,
                fork_ancestor_checkpoint: None,
                active_canonical_messages: input.active_canonical_messages.clone(),
                fingerprints: input.fingerprints.clone(),
            },
        )?;
        Ok(BranchSelectionResult {
            branch,
            action,
            matched_existing_branch: None,
        })
    }

    /// # Errors
    /// Returns an error if the branch metadata cannot be read or parsed.
    pub fn load_branch(
        &self,
        claude_session_id: &str,
        branch_id: &str,
    ) -> Result<BranchMetadata, StateError> {
        self.load_branch_unlocked(claude_session_id, branch_id)
    }

    fn load_branch_unlocked(
        &self,
        claude_session_id: &str,
        branch_id: &str,
    ) -> Result<BranchMetadata, StateError> {
        let path = self
            .branch_dir(claude_session_id, branch_id)
            .join("branch.json");
        let bytes = std::fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// # Errors
    /// Returns an error if the branch metadata cannot be written.
    pub fn store_branch(
        &self,
        claude_session_id: &str,
        branch: &BranchMetadata,
    ) -> Result<(), StateError> {
        self.with_session_lock(claude_session_id, |store| {
            store.store_branch_unlocked(claude_session_id, branch)
        })
    }

    fn store_branch_unlocked(
        &self,
        claude_session_id: &str,
        branch: &BranchMetadata,
    ) -> Result<(), StateError> {
        let mut updated = branch.clone();
        updated.updated_at_unix_seconds = now_unix_seconds();
        let branch_path = self
            .branch_dir(claude_session_id, &branch.branch_id)
            .join("branch.json");
        write_json_atomically(&branch_path, &updated)
    }

    /// # Errors
    /// Returns an error if the branch ledger cannot be appended.
    pub fn append_ledger_event(
        &self,
        claude_session_id: &str,
        branch_id: &str,
        event: &CanonicalLedgerEvent,
    ) -> Result<(), StateError> {
        self.with_session_lock(claude_session_id, |store| {
            store.append_ledger_event_unlocked(claude_session_id, branch_id, event)
        })
    }

    fn append_ledger_event_unlocked(
        &self,
        claude_session_id: &str,
        branch_id: &str,
        event: &CanonicalLedgerEvent,
    ) -> Result<(), StateError> {
        let ledger_path = self
            .branch_dir(claude_session_id, branch_id)
            .join("ledger.jsonl");
        if let Some(parent) = ledger_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(ledger_path)?;
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    /// # Errors
    /// Returns an error if the branch metadata or ledger cannot be updated.
    pub fn commit_turn(
        &self,
        claude_session_id: &str,
        branch_id: &str,
        params: &CommitTurnParams,
    ) -> Result<BranchMetadata, StateError> {
        self.with_session_lock(claude_session_id, |store| {
            store.commit_turn_unlocked(claude_session_id, branch_id, params)
        })
    }

    fn commit_turn_unlocked(
        &self,
        claude_session_id: &str,
        branch_id: &str,
        params: &CommitTurnParams,
    ) -> Result<BranchMetadata, StateError> {
        let mut branch = self.load_branch_unlocked(claude_session_id, branch_id)?;
        branch.fingerprints = params.fingerprints.clone();
        branch.updated_at_unix_seconds = now_unix_seconds();
        if matches!(params.turn_scope, ConversationTurnScope::Main) {
            branch.last_main_turn_id = Some(params.turn_id.clone());
            if let (Some(response_id), Some(provider_model_fingerprint)) = (
                params.provider_response_id.clone(),
                params.provider_model_fingerprint.clone(),
            ) {
                branch.current_checkpoint_id = Some(response_id.clone());
                branch.compaction_reset_pending = false;
                branch.openai_checkpoint = Some(OpenAiCheckpoint {
                    response_id,
                    previous_response_id: params.previous_response_id.clone(),
                    provider_model_fingerprint,
                    request_compatibility_fingerprint: params
                        .request_compatibility_fingerprint
                        .clone(),
                });
            }
        }

        let event = match params.turn_scope {
            ConversationTurnScope::Main => CanonicalLedgerEvent::MainTurnCommitted {
                turn_id: params.turn_id.clone(),
                provider_response_id: params.provider_response_id.clone(),
                request_fingerprint: branch.fingerprints.recent_message_tail_hash.clone(),
                provider_output_items: params.provider_output_items.clone(),
                created_at_unix_seconds: now_unix_seconds(),
            },
            ConversationTurnScope::Side => CanonicalLedgerEvent::SideTurnObserved {
                turn_id: params.turn_id.clone(),
                request_fingerprint: branch.fingerprints.recent_message_tail_hash.clone(),
                provider_output_items: params.provider_output_items.clone(),
                created_at_unix_seconds: now_unix_seconds(),
            },
        };
        self.append_ledger_event_unlocked(claude_session_id, branch_id, &event)?;
        self.store_branch_unlocked(claude_session_id, &branch)?;
        Ok(branch)
    }

    /// # Errors
    /// Returns an error if the branch metadata or ledger cannot be updated.
    pub fn reconcile_branch_snapshot(
        &self,
        claude_session_id: &str,
        branch_id: &str,
        params: &ReconcileSnapshotParams,
    ) -> Result<BranchMetadata, StateError> {
        self.with_session_lock(claude_session_id, |store| {
            store.reconcile_branch_snapshot_unlocked(claude_session_id, branch_id, params)
        })
    }

    fn reconcile_branch_snapshot_unlocked(
        &self,
        claude_session_id: &str,
        branch_id: &str,
        params: &ReconcileSnapshotParams,
    ) -> Result<BranchMetadata, StateError> {
        let mut branch = self.load_branch_unlocked(claude_session_id, branch_id)?;
        let snapshot_changed = branch.active_canonical_messages.as_ref() != Some(&params.messages);
        branch.active_canonical_messages = Some(params.messages.clone());
        branch.fingerprints = params.fingerprints.clone();
        branch.updated_at_unix_seconds = now_unix_seconds();
        if snapshot_changed {
            self.append_ledger_event_unlocked(
                claude_session_id,
                branch_id,
                &CanonicalLedgerEvent::InboundCanonicalSnapshotReconciled {
                    snapshot_hash: params.fingerprints.branch_state_hash.clone(),
                    created_at_unix_seconds: now_unix_seconds(),
                },
            )?;
        }
        self.store_branch_unlocked(claude_session_id, &branch)?;
        Ok(branch)
    }

    /// # Errors
    /// Returns an error if the branch metadata or ledger cannot be updated.
    pub fn apply_compaction(
        &self,
        claude_session_id: &str,
        branch_id: &str,
        summary_hash: Option<&str>,
        fingerprints: &BranchFingerprintSet,
    ) -> Result<BranchMetadata, StateError> {
        self.with_session_lock(claude_session_id, |store| {
            store.apply_compaction_unlocked(
                claude_session_id,
                branch_id,
                summary_hash,
                fingerprints,
            )
        })
    }

    fn apply_compaction_unlocked(
        &self,
        claude_session_id: &str,
        branch_id: &str,
        summary_hash: Option<&str>,
        fingerprints: &BranchFingerprintSet,
    ) -> Result<BranchMetadata, StateError> {
        let mut branch = self.load_branch_unlocked(claude_session_id, branch_id)?;
        branch.fingerprints = fingerprints.clone();
        branch.compaction_reset_pending = true;
        let now = now_unix_seconds();
        branch.updated_at_unix_seconds = now;
        self.append_ledger_event_unlocked(
            claude_session_id,
            branch_id,
            &CanonicalLedgerEvent::CompactionApplied {
                summary_hash: summary_hash.map(ToString::to_string),
                created_at_unix_seconds: now,
            },
        )?;
        self.store_branch_unlocked(claude_session_id, &branch)?;
        self.write_sparse_checkpoint_unlocked(
            claude_session_id,
            &branch,
            SparseCheckpointKind::CompactionApplied,
            now,
        )?;
        Ok(branch)
    }

    /// # Errors
    /// Returns an error if the branch metadata cannot be updated.
    pub fn invalidate_openai_checkpoint(
        &self,
        claude_session_id: &str,
        branch_id: &str,
    ) -> Result<BranchMetadata, StateError> {
        self.with_session_lock(claude_session_id, |store| {
            store.invalidate_openai_checkpoint_unlocked(claude_session_id, branch_id)
        })
    }

    fn invalidate_openai_checkpoint_unlocked(
        &self,
        claude_session_id: &str,
        branch_id: &str,
    ) -> Result<BranchMetadata, StateError> {
        let mut branch = self.load_branch_unlocked(claude_session_id, branch_id)?;
        branch.current_checkpoint_id = None;
        branch.openai_checkpoint = None;
        branch.updated_at_unix_seconds = now_unix_seconds();
        self.store_branch_unlocked(claude_session_id, &branch)?;
        Ok(branch)
    }

    /// # Errors
    /// Returns an error if persisted branch state cannot be validated or reconstructed safely.
    pub fn rebuild_branch_from_disk(
        &self,
        claude_session_id: &str,
        branch_id: &str,
    ) -> Result<BranchMetadata, StateError> {
        let branch = self.load_branch(claude_session_id, branch_id)?;
        let events = self.load_branch_ledger_events(claude_session_id, branch_id)?;
        validate_branch_against_ledger(&branch, &events)?;
        Ok(branch)
    }

    fn load_branch_ledger_events(
        &self,
        claude_session_id: &str,
        branch_id: &str,
    ) -> Result<Vec<CanonicalLedgerEvent>, StateError> {
        let ledger_path = self
            .branch_dir(claude_session_id, branch_id)
            .join("ledger.jsonl");
        let text = std::fs::read_to_string(ledger_path)?;
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str::<CanonicalLedgerEvent>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(StateError::from)
    }

    fn write_sparse_checkpoint_unlocked(
        &self,
        claude_session_id: &str,
        branch: &BranchMetadata,
        checkpoint_kind: SparseCheckpointKind,
        created_at_unix_seconds: i64,
    ) -> Result<(), StateError> {
        let checkpoint = SparseCheckpoint {
            schema_version: SPARSE_CHECKPOINT_SCHEMA_VERSION,
            checkpoint_kind,
            branch_id: branch.branch_id.clone(),
            current_checkpoint_id: branch.current_checkpoint_id.clone(),
            parent_branch_id: branch.parent_branch_id.clone(),
            fork_ancestor_checkpoint: branch.fork_ancestor_checkpoint.clone(),
            active_canonical_messages: branch.active_canonical_messages.clone(),
            fingerprints: branch.fingerprints.clone(),
            compaction_reset_pending: branch.compaction_reset_pending,
            last_main_turn_id: branch.last_main_turn_id.clone(),
            created_at_unix_seconds,
        };
        let file_name = format!(
            "{created_at_unix_seconds:020}-{:?}-{}.json",
            checkpoint_kind,
            Uuid::new_v4()
        )
        .to_ascii_lowercase();
        let path = self
            .branch_checkpoints_dir(claude_session_id, &branch.branch_id)
            .join(file_name);
        write_json_atomically(&path, &checkpoint)
    }

    fn write_session(&self, session: &ClaudeSessionMetadata) -> Result<(), StateError> {
        let session_path = self
            .session_dir(&session.claude_session_id)
            .join("session.json");
        write_json_atomically(&session_path, session)
    }

    fn with_session_lock<T>(
        &self,
        claude_session_id: &str,
        mut operation: impl FnMut(&Self) -> Result<T, StateError>,
    ) -> Result<T, StateError> {
        let lock = {
            let mut registry = session_lock_registry()
                .lock()
                .map_err(|_| StateError::Invariant("session lock registry poisoned".to_string()))?;
            registry
                .entry(claude_session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock
            .lock()
            .map_err(|_| StateError::Invariant("session lock poisoned".to_string()))?;
        let _file_guard =
            SessionFileLockGuard::acquire(&self.session_lock_path(claude_session_id))?;
        match operation(self) {
            Ok(value) => Ok(value),
            Err(err)
                if self.corruption_policy
                    == gateway_core::config::ConversationStateCorruptionPolicy::QuarantineAndReset
                    && is_recoverable_state_corruption(&err) =>
            {
                self.quarantine_session_unlocked(claude_session_id, &err)?;
                operation(self)
            }
            Err(err) => Err(err),
        }
    }

    fn session_lock_path(&self, claude_session_id: &str) -> PathBuf {
        self.session_dir(claude_session_id).join(".session.lock")
    }

    fn quarantine_root(&self) -> PathBuf {
        self.root.parent().map_or_else(
            || PathBuf::from(format!("{}-quarantine", self.root.display())),
            |parent| {
                let root_name = self
                    .root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map_or_else(|| "claudecode".to_string(), ToString::to_string);
                parent.join(format!("{root_name}-quarantine"))
            },
        )
    }

    fn quarantine_session_unlocked(
        &self,
        claude_session_id: &str,
        reason: &StateError,
    ) -> Result<(), StateError> {
        let session_dir = self.session_dir(claude_session_id);
        if !session_dir.exists() {
            return Ok(());
        }
        let quarantine_root = self.quarantine_root();
        std::fs::create_dir_all(&quarantine_root)?;
        let quarantine_dir = quarantine_root.join(format!(
            "session-id-{claude_session_id}-{}-{}",
            now_unix_seconds(),
            Uuid::new_v4()
        ));
        std::fs::rename(&session_dir, &quarantine_dir)?;
        let metadata_path = quarantine_dir.join("quarantine-metadata.json");
        let metadata = serde_json::json!({
            "claude_session_id": claude_session_id,
            "reason": reason.to_string(),
            "quarantined_at_unix_seconds": now_unix_seconds(),
        });
        write_json_atomically(&metadata_path, &metadata)?;
        Ok(())
    }
}

fn default_conversation_state_root() -> Result<PathBuf, StateError> {
    if let Ok(path) = std::env::var("CLD_GATEWAY_CONVERSATION_STATE_ROOT") {
        return Ok(PathBuf::from(path));
    }
    Ok(default_gateway_dir()?.join("sessions").join("claudecode"))
}

fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> Result<(), StateError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("tmp");
    let encoded = serde_json::to_vec_pretty(value)?;
    std::fs::write(&tmp_path, encoded)?;
    std::fs::rename(tmp_path, path)?;
    Ok(())
}

struct SessionFileLockGuard {
    file: File,
}

impl SessionFileLockGuard {
    fn acquire(path: &Path) -> Result<Self, StateError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;
        Ok(Self { file })
    }
}

impl Drop for SessionFileLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn is_recoverable_state_corruption(err: &StateError) -> bool {
    matches!(err, StateError::Invariant(_) | StateError::Json(_))
}

fn now_unix_seconds() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64);
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn exact_branch_match(existing: &BranchFingerprintSet, incoming: &BranchFingerprintSet) -> bool {
    fingerprint_equal(
        existing.last_user_message_hash.as_deref(),
        incoming.last_user_message_hash.as_deref(),
    ) && fingerprint_equal(
        existing.recent_message_tail_hash.as_deref(),
        incoming.recent_message_tail_hash.as_deref(),
    )
}

fn ancestor_branch_match(existing: &BranchFingerprintSet, incoming: &BranchFingerprintSet) -> bool {
    fingerprint_equal(
        existing.last_user_message_hash.as_deref(),
        incoming.last_user_message_hash.as_deref(),
    ) && existing.recent_message_tail_hash != incoming.recent_message_tail_hash
}

fn fingerprint_equal(existing: Option<&str>, incoming: Option<&str>) -> bool {
    matches!((existing, incoming), (Some(left), Some(right)) if left == right)
}

fn validate_branch_against_ledger(
    branch: &BranchMetadata,
    events: &[CanonicalLedgerEvent],
) -> Result<(), StateError> {
    if !matches!(
        events.first(),
        Some(CanonicalLedgerEvent::BranchCreated { .. })
    ) {
        return Err(StateError::Invariant(
            "branch ledger is missing its initial branch_created event".to_string(),
        ));
    }

    let mut last_main_turn_id = None;
    let mut last_provider_response_id = None;
    let mut compaction_reset_pending = false;

    for event in events {
        match event {
            CanonicalLedgerEvent::MainTurnCommitted {
                turn_id,
                provider_response_id,
                ..
            } => {
                last_main_turn_id = Some(turn_id.as_str());
                last_provider_response_id = provider_response_id.as_deref();
                compaction_reset_pending = false;
            }
            CanonicalLedgerEvent::CompactionApplied { .. } => {
                compaction_reset_pending = true;
            }
            CanonicalLedgerEvent::BranchCreated { .. }
            | CanonicalLedgerEvent::InboundCanonicalSnapshotReconciled { .. }
            | CanonicalLedgerEvent::SideTurnObserved { .. } => {}
        }
    }

    if branch.last_main_turn_id.as_deref() != last_main_turn_id {
        return Err(StateError::Invariant(format!(
            "branch last_main_turn_id {:?} does not match ledger {:?}",
            branch.last_main_turn_id, last_main_turn_id
        )));
    }

    if branch.current_checkpoint_id.as_deref() != last_provider_response_id {
        return Err(StateError::Invariant(format!(
            "branch current_checkpoint_id {:?} does not match ledger {:?}",
            branch.current_checkpoint_id, last_provider_response_id
        )));
    }

    if branch.compaction_reset_pending != compaction_reset_pending {
        return Err(StateError::Invariant(format!(
            "branch compaction_reset_pending {} does not match ledger {}",
            branch.compaction_reset_pending, compaction_reset_pending
        )));
    }

    if branch.current_checkpoint_id.is_none() && branch.openai_checkpoint.is_some() {
        return Err(StateError::Invariant(
            "branch has OpenAI checkpoint metadata without a current checkpoint id".to_string(),
        ));
    }

    if let Some(checkpoint) = branch.openai_checkpoint.as_ref()
        && Some(checkpoint.response_id.as_str()) != branch.current_checkpoint_id.as_deref()
    {
        return Err(StateError::Invariant(format!(
            "branch checkpoint response_id {:?} does not match current checkpoint {:?}",
            checkpoint.response_id, branch.current_checkpoint_id
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("gateway_conversation_store_{name}_{nanos}"))
    }

    #[test]
    fn ensure_session_creates_expected_layout() {
        let root = temp_root("session_layout");
        let store = ConversationStateStore::new(&root);
        let session = store.ensure_session("claude-session-1").expect("session");
        assert_eq!(session.claude_session_id, "claude-session-1");
        assert!(
            root.join("session-id-claude-session-1")
                .join("session.json")
                .exists()
        );
    }

    #[test]
    fn create_branch_persists_branch_metadata_and_ledger() {
        let root = temp_root("branch_create");
        let store = ConversationStateStore::new(&root);
        let branch = store
            .create_branch(
                "claude-session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet {
                        last_user_message_hash: Some("user-hash".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                },
            )
            .expect("branch");

        let branch_dir = root
            .join("session-id-claude-session-1")
            .join(format!("tab-{}", branch.branch_id));
        assert!(branch_dir.join("branch.json").exists());
        assert!(branch_dir.join("ledger.jsonl").exists());
        assert!(branch_dir.join("checkpoints").exists());
        let checkpoint_files = std::fs::read_dir(branch_dir.join("checkpoints"))
            .expect("read checkpoints dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect checkpoint dir entries");
        assert_eq!(checkpoint_files.len(), 1);
        let checkpoint: SparseCheckpoint = serde_json::from_slice(
            &std::fs::read(checkpoint_files[0].path()).expect("read sparse checkpoint"),
        )
        .expect("decode sparse checkpoint");
        assert_eq!(
            checkpoint.checkpoint_kind,
            SparseCheckpointKind::BranchCreated
        );
        assert_eq!(checkpoint.branch_id, branch.branch_id);

        let session = store.load_session("claude-session-1").expect("session");
        assert_eq!(session.branch_ids, vec![branch.branch_id.clone()]);
    }

    #[test]
    fn create_branch_allocates_distinct_opaque_branch_ids() {
        let root = temp_root("branch_ids");
        let store = ConversationStateStore::new(&root);
        let branch_a = store
            .create_branch(
                "claude-session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet::default(),
                },
            )
            .expect("branch a");
        let branch_b = store
            .create_branch(
                "claude-session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet::default(),
                },
            )
            .expect("branch b");

        assert_ne!(branch_a.branch_id, branch_b.branch_id);
        assert!(uuid::Uuid::parse_str(&branch_a.branch_id).is_ok());
        assert!(uuid::Uuid::parse_str(&branch_b.branch_id).is_ok());
    }

    #[test]
    fn append_ledger_event_writes_jsonl_line() {
        let root = temp_root("ledger_append");
        let store = ConversationStateStore::new(&root);
        let branch = store
            .create_branch(
                "claude-session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet::default(),
                },
            )
            .expect("branch");
        let event = CanonicalLedgerEvent::CompactionApplied {
            summary_hash: Some("summary-1".to_string()),
            created_at_unix_seconds: 123,
        };
        store
            .append_ledger_event("claude-session-1", &branch.branch_id, &event)
            .expect("append");

        let ledger = std::fs::read_to_string(
            root.join("session-id-claude-session-1")
                .join(format!("tab-{}", branch.branch_id))
                .join("ledger.jsonl"),
        )
        .expect("read ledger");
        assert!(ledger.contains("\"event_type\":\"branch_created\""));
        assert!(ledger.contains("\"event_type\":\"compaction_applied\""));
    }

    #[test]
    fn select_or_create_branch_continues_exact_match() {
        let root = temp_root("branch_continue");
        let store = ConversationStateStore::new(&root);
        let created = store
            .create_branch(
                "claude-session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet {
                        recent_message_tail_hash: Some("tail-1".to_string()),
                        last_user_message_hash: Some("user-1".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                },
            )
            .expect("create branch");

        let selected = store
            .select_or_create_branch(
                "claude-session-1",
                &BranchSelectionInput {
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet {
                        recent_message_tail_hash: Some("tail-1".to_string()),
                        last_user_message_hash: Some("user-1".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                    turn_scope: ConversationTurnScope::Main,
                },
            )
            .expect("select");

        assert_eq!(selected.action, BranchSelectionAction::ContinuedExisting);
        assert_eq!(selected.branch.branch_id, created.branch_id);
        assert_eq!(
            selected
                .matched_existing_branch
                .as_ref()
                .map(|branch| branch.branch_id.as_str()),
            Some(created.branch_id.as_str())
        );
    }

    #[test]
    fn select_or_create_branch_forks_on_ancestor_like_match() {
        let root = temp_root("branch_fork");
        let store = ConversationStateStore::new(&root);
        let created = store
            .create_branch(
                "claude-session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet {
                        recent_message_tail_hash: Some("tail-1".to_string()),
                        last_user_message_hash: Some("user-1".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                },
            )
            .expect("create branch");

        let selected = store
            .select_or_create_branch(
                "claude-session-1",
                &BranchSelectionInput {
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet {
                        recent_message_tail_hash: Some("tail-2".to_string()),
                        last_user_message_hash: Some("user-1".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                    turn_scope: ConversationTurnScope::Main,
                },
            )
            .expect("select");

        assert_eq!(selected.action, BranchSelectionAction::ForkedFromAncestor);
        assert_ne!(selected.branch.branch_id, created.branch_id);
        assert_eq!(
            selected.branch.parent_branch_id,
            Some(created.branch_id.clone())
        );
        assert_eq!(
            selected
                .matched_existing_branch
                .as_ref()
                .map(|branch| branch.branch_id.as_str()),
            Some(created.branch_id.as_str())
        );
    }

    #[test]
    fn commit_turn_updates_branch_and_ledger() {
        let root = temp_root("commit_turn");
        let store = ConversationStateStore::new(&root);
        let branch = store
            .create_branch(
                "claude-session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet::default(),
                },
            )
            .expect("create branch");
        let committed = store
            .commit_turn(
                "claude-session-1",
                &branch.branch_id,
                &CommitTurnParams {
                    turn_scope: ConversationTurnScope::Main,
                    turn_id: "turn-1".to_string(),
                    fingerprints: BranchFingerprintSet {
                        recent_message_tail_hash: Some("tail-1".to_string()),
                        last_user_message_hash: Some("user-1".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                    provider_response_id: None,
                    previous_response_id: None,
                    provider_model_fingerprint: None,
                    request_compatibility_fingerprint: None,
                    provider_output_items: Vec::new(),
                },
            )
            .expect("commit");
        assert_eq!(committed.last_main_turn_id.as_deref(), Some("turn-1"));
        let ledger = std::fs::read_to_string(
            root.join("session-id-claude-session-1")
                .join(format!("tab-{}", branch.branch_id))
                .join("ledger.jsonl"),
        )
        .expect("read ledger");
        assert!(ledger.contains("\"event_type\":\"main_turn_committed\""));
    }

    #[test]
    fn commit_turn_persists_openai_checkpoint_for_main_turn() {
        let root = temp_root("checkpoint_commit");
        let store = ConversationStateStore::new(&root);
        let branch = store
            .create_branch(
                "claude-session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet::default(),
                },
            )
            .expect("create branch");
        let committed = store
            .commit_turn(
                "claude-session-1",
                &branch.branch_id,
                &CommitTurnParams {
                    turn_scope: ConversationTurnScope::Main,
                    turn_id: "turn-2".to_string(),
                    fingerprints: BranchFingerprintSet {
                        recent_message_tail_hash: Some("tail-2".to_string()),
                        last_user_message_hash: Some("user-2".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                    provider_response_id: Some("resp_2".to_string()),
                    previous_response_id: Some("resp_1".to_string()),
                    provider_model_fingerprint: Some("gpt-5.4".to_string()),
                    request_compatibility_fingerprint: Some("fingerprint-2".to_string()),
                    provider_output_items: vec![
                        serde_json::json!({"type":"message","role":"assistant"}),
                    ],
                },
            )
            .expect("commit");
        assert_eq!(committed.current_checkpoint_id.as_deref(), Some("resp_2"));
        assert_eq!(
            committed
                .openai_checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.previous_response_id.as_deref()),
            Some("resp_1")
        );
    }

    #[test]
    fn invalidate_openai_checkpoint_clears_branch_checkpoint_state() {
        let root = temp_root("checkpoint_invalidation");
        let store = ConversationStateStore::new(&root);

        let branch = store
            .create_branch(
                "session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet::default(),
                },
            )
            .expect("create branch");

        store
            .commit_turn(
                "session-1",
                &branch.branch_id,
                &CommitTurnParams {
                    turn_scope: ConversationTurnScope::Main,
                    turn_id: "turn-1".to_string(),
                    fingerprints: BranchFingerprintSet::default(),
                    provider_response_id: Some("resp_2".to_string()),
                    previous_response_id: Some("resp_1".to_string()),
                    provider_model_fingerprint: Some("gpt-5".to_string()),
                    request_compatibility_fingerprint: Some("fingerprint-1".to_string()),
                    provider_output_items: Vec::new(),
                },
            )
            .expect("commit branch checkpoint");

        let invalidated = store
            .invalidate_openai_checkpoint("session-1", &branch.branch_id)
            .expect("invalidate checkpoint");

        assert_eq!(invalidated.current_checkpoint_id, None);
        assert_eq!(invalidated.openai_checkpoint, None);
    }

    #[test]
    fn apply_compaction_sets_reset_flag_and_records_event() {
        let root = temp_root("apply_compaction");
        let store = ConversationStateStore::new(&root);
        let branch = store
            .create_branch(
                "session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet::default(),
                },
            )
            .expect("create branch");

        let compacted = store
            .apply_compaction(
                "session-1",
                &branch.branch_id,
                Some("summary-1"),
                &BranchFingerprintSet {
                    compaction_summary_hash: Some("summary-1".to_string()),
                    branch_state_hash: Some("state-1".to_string()),
                    ..BranchFingerprintSet::default()
                },
            )
            .expect("apply compaction");

        assert!(compacted.compaction_reset_pending);
        assert_eq!(
            compacted.fingerprints.compaction_summary_hash.as_deref(),
            Some("summary-1")
        );
        let ledger = std::fs::read_to_string(
            root.join("session-id-session-1")
                .join(format!("tab-{}", branch.branch_id))
                .join("ledger.jsonl"),
        )
        .expect("read ledger");
        assert!(ledger.contains("\"event_type\":\"compaction_applied\""));
        let checkpoint_dir = root
            .join("session-id-session-1")
            .join(format!("tab-{}", branch.branch_id))
            .join("checkpoints");
        let checkpoint_files = std::fs::read_dir(checkpoint_dir)
            .expect("read checkpoints dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect checkpoints");
        assert_eq!(checkpoint_files.len(), 2);
        let latest_path = checkpoint_files
            .iter()
            .map(std::fs::DirEntry::path)
            .max()
            .expect("latest checkpoint path");
        let latest: SparseCheckpoint =
            serde_json::from_slice(&std::fs::read(latest_path).expect("read checkpoint"))
                .expect("decode checkpoint");
        assert_eq!(
            latest.checkpoint_kind,
            SparseCheckpointKind::CompactionApplied
        );
        assert!(latest.compaction_reset_pending);
    }

    #[test]
    fn commit_turn_clears_compaction_reset_after_successful_main_turn() {
        let root = temp_root("clear_compaction_reset");
        let store = ConversationStateStore::new(&root);
        let branch = store
            .create_branch(
                "session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet::default(),
                },
            )
            .expect("create branch");

        store
            .apply_compaction(
                "session-1",
                &branch.branch_id,
                Some("summary-1"),
                &BranchFingerprintSet {
                    compaction_summary_hash: Some("summary-1".to_string()),
                    branch_state_hash: Some("state-1".to_string()),
                    ..BranchFingerprintSet::default()
                },
            )
            .expect("apply compaction");

        let committed = store
            .commit_turn(
                "session-1",
                &branch.branch_id,
                &CommitTurnParams {
                    turn_scope: ConversationTurnScope::Main,
                    turn_id: "turn-after-compaction".to_string(),
                    fingerprints: BranchFingerprintSet {
                        compaction_summary_hash: Some("summary-1".to_string()),
                        branch_state_hash: Some("state-2".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                    provider_response_id: Some("resp_after".to_string()),
                    previous_response_id: None,
                    provider_model_fingerprint: Some("gpt-5".to_string()),
                    request_compatibility_fingerprint: Some("fingerprint-after".to_string()),
                    provider_output_items: Vec::new(),
                },
            )
            .expect("commit after compaction");

        assert!(!committed.compaction_reset_pending);
        assert_eq!(
            committed.current_checkpoint_id.as_deref(),
            Some("resp_after")
        );
    }

    #[test]
    fn reconcile_branch_snapshot_is_idempotent_for_repeated_transcript() {
        let root = temp_root("reconcile_snapshot_idempotent");
        let store = ConversationStateStore::new(&root);
        let messages = serde_json::json!([
            { "role": "user", "content": "hello" }
        ]);
        let branch = store
            .create_branch(
                "session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet {
                        recent_message_tail_hash: Some("tail-1".to_string()),
                        last_user_message_hash: Some("user-1".to_string()),
                        branch_state_hash: Some("state-1".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                },
            )
            .expect("create branch");

        let params = ReconcileSnapshotParams {
            messages: messages.clone(),
            fingerprints: BranchFingerprintSet {
                recent_message_tail_hash: Some("tail-1".to_string()),
                last_user_message_hash: Some("user-1".to_string()),
                branch_state_hash: Some("state-1".to_string()),
                ..BranchFingerprintSet::default()
            },
        };

        store
            .reconcile_branch_snapshot("session-1", &branch.branch_id, &params)
            .expect("first reconcile");
        store
            .reconcile_branch_snapshot("session-1", &branch.branch_id, &params)
            .expect("second reconcile");

        let reloaded = store
            .load_branch("session-1", &branch.branch_id)
            .expect("reload branch");
        assert_eq!(reloaded.active_canonical_messages, Some(messages));

        let ledger = std::fs::read_to_string(
            root.join("session-id-session-1")
                .join(format!("tab-{}", branch.branch_id))
                .join("ledger.jsonl"),
        )
        .expect("read ledger");
        assert_eq!(
            ledger
                .matches("\"event_type\":\"inbound_canonical_snapshot_reconciled\"")
                .count(),
            1
        );
    }

    #[test]
    fn rebuild_branch_from_disk_accepts_valid_persisted_state() {
        let root = temp_root("rebuild_valid");
        let store = ConversationStateStore::new(&root);
        let branch = store
            .create_branch(
                "session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: Some(serde_json::json!([
                        { "role": "user", "content": "hello" }
                    ])),
                    fingerprints: BranchFingerprintSet {
                        recent_message_tail_hash: Some("tail-1".to_string()),
                        last_user_message_hash: Some("user-1".to_string()),
                        branch_state_hash: Some("state-1".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                },
            )
            .expect("create branch");
        store
            .reconcile_branch_snapshot(
                "session-1",
                &branch.branch_id,
                &ReconcileSnapshotParams {
                    messages: serde_json::json!([{ "role": "user", "content": "hello" }]),
                    fingerprints: BranchFingerprintSet {
                        recent_message_tail_hash: Some("tail-1".to_string()),
                        last_user_message_hash: Some("user-1".to_string()),
                        branch_state_hash: Some("state-1".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                },
            )
            .expect("reconcile");
        store
            .commit_turn(
                "session-1",
                &branch.branch_id,
                &CommitTurnParams {
                    turn_scope: ConversationTurnScope::Main,
                    turn_id: "turn-1".to_string(),
                    fingerprints: BranchFingerprintSet {
                        recent_message_tail_hash: Some("tail-2".to_string()),
                        last_user_message_hash: Some("user-2".to_string()),
                        branch_state_hash: Some("state-2".to_string()),
                        ..BranchFingerprintSet::default()
                    },
                    provider_response_id: Some("resp_1".to_string()),
                    previous_response_id: None,
                    provider_model_fingerprint: Some("gpt-5".to_string()),
                    request_compatibility_fingerprint: Some("fingerprint-1".to_string()),
                    provider_output_items: Vec::new(),
                },
            )
            .expect("commit");

        let rebuilt = store
            .rebuild_branch_from_disk("session-1", &branch.branch_id)
            .expect("rebuild branch");
        assert_eq!(rebuilt.current_checkpoint_id.as_deref(), Some("resp_1"));
        assert_eq!(rebuilt.last_main_turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn rebuild_branch_from_disk_fails_closed_on_inconsistent_branch_metadata() {
        let root = temp_root("rebuild_invalid");
        let store = ConversationStateStore::new(&root);
        let branch = store
            .create_branch(
                "session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: None,
                    fingerprints: BranchFingerprintSet::default(),
                },
            )
            .expect("create branch");
        store
            .commit_turn(
                "session-1",
                &branch.branch_id,
                &CommitTurnParams {
                    turn_scope: ConversationTurnScope::Main,
                    turn_id: "turn-1".to_string(),
                    fingerprints: BranchFingerprintSet::default(),
                    provider_response_id: Some("resp_1".to_string()),
                    previous_response_id: None,
                    provider_model_fingerprint: Some("gpt-5".to_string()),
                    request_compatibility_fingerprint: Some("fingerprint-1".to_string()),
                    provider_output_items: Vec::new(),
                },
            )
            .expect("commit");

        let branch_path = root
            .join("session-id-session-1")
            .join(format!("tab-{}", branch.branch_id))
            .join("branch.json");
        let mut corrupted: BranchMetadata =
            serde_json::from_slice(&std::fs::read(&branch_path).expect("read branch.json"))
                .expect("decode branch json");
        corrupted.current_checkpoint_id = Some("resp_wrong".to_string());
        std::fs::write(
            &branch_path,
            serde_json::to_vec_pretty(&corrupted).expect("encode corrupted branch"),
        )
        .expect("write corrupted branch");

        let err = store
            .rebuild_branch_from_disk("session-1", &branch.branch_id)
            .expect_err("rebuild must fail closed");
        assert!(matches!(err, StateError::Invariant(_)));
    }

    #[test]
    fn cleanup_sessions_older_than_days_removes_only_expired_session_buckets() {
        let root = temp_root("cleanup_sessions");
        let store = ConversationStateStore::new(&root);
        let old = store
            .ensure_session("old-session")
            .expect("create old session");
        let fresh = store
            .ensure_session("fresh-session")
            .expect("create fresh session");

        let old_path = root.join("session-id-old-session").join("session.json");
        let mut old_session: ClaudeSessionMetadata =
            serde_json::from_slice(&std::fs::read(&old_path).expect("read old session"))
                .expect("decode old session");
        old_session.updated_at_unix_seconds = old.created_at_unix_seconds - (10 * 24 * 60 * 60);
        std::fs::write(
            &old_path,
            serde_json::to_vec_pretty(&old_session).expect("encode old session"),
        )
        .expect("write old session");

        let fresh_path = root.join("session-id-fresh-session").join("session.json");
        let mut fresh_session: ClaudeSessionMetadata =
            serde_json::from_slice(&std::fs::read(&fresh_path).expect("read fresh session"))
                .expect("decode fresh session");
        fresh_session.updated_at_unix_seconds = fresh.created_at_unix_seconds;
        std::fs::write(
            &fresh_path,
            serde_json::to_vec_pretty(&fresh_session).expect("encode fresh session"),
        )
        .expect("write fresh session");

        let removed = store
            .cleanup_sessions_older_than_days(7)
            .expect("cleanup expired sessions");
        assert_eq!(removed, 1);
        assert!(!root.join("session-id-old-session").exists());
        assert!(root.join("session-id-fresh-session").exists());
    }

    #[test]
    fn select_or_create_branch_fails_closed_when_persisted_branch_is_corrupted() {
        let root = temp_root("selection_rebuild_invalid");
        let store = ConversationStateStore::new(&root);
        let fingerprints = BranchFingerprintSet {
            recent_message_tail_hash: Some("tail-1".to_string()),
            last_user_message_hash: Some("user-1".to_string()),
            branch_state_hash: Some("state-1".to_string()),
            ..BranchFingerprintSet::default()
        };
        let branch = store
            .create_branch(
                "session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: Some(serde_json::json!([
                        { "role": "user", "content": "hello" }
                    ])),
                    fingerprints: fingerprints.clone(),
                },
            )
            .expect("create branch");
        store
            .commit_turn(
                "session-1",
                &branch.branch_id,
                &CommitTurnParams {
                    turn_scope: ConversationTurnScope::Main,
                    turn_id: "turn-1".to_string(),
                    fingerprints: fingerprints.clone(),
                    provider_response_id: Some("resp_1".to_string()),
                    previous_response_id: None,
                    provider_model_fingerprint: Some("gpt-5".to_string()),
                    request_compatibility_fingerprint: Some("fingerprint-1".to_string()),
                    provider_output_items: Vec::new(),
                },
            )
            .expect("commit");

        let branch_path = root
            .join("session-id-session-1")
            .join(format!("tab-{}", branch.branch_id))
            .join("branch.json");
        let mut corrupted: BranchMetadata =
            serde_json::from_slice(&std::fs::read(&branch_path).expect("read branch.json"))
                .expect("decode branch json");
        corrupted.current_checkpoint_id = Some("resp_wrong".to_string());
        std::fs::write(
            &branch_path,
            serde_json::to_vec_pretty(&corrupted).expect("encode corrupted branch"),
        )
        .expect("write corrupted branch");

        let err = store
            .select_or_create_branch(
                "session-1",
                &BranchSelectionInput {
                    active_canonical_messages: Some(serde_json::json!([
                        { "role": "user", "content": "hello" }
                    ])),
                    fingerprints,
                    turn_scope: ConversationTurnScope::Main,
                },
            )
            .expect_err("selection must fail closed on corrupted persisted branch");
        assert!(matches!(err, StateError::Invariant(_)));
    }

    #[test]
    fn quarantine_and_reset_moves_corrupted_session_and_rebuilds_live_state() {
        let root = temp_root("selection_rebuild_quarantine");
        let store = ConversationStateStore::new_with_policy(
            &root,
            gateway_core::config::ConversationStateCorruptionPolicy::QuarantineAndReset,
        );
        let fingerprints = BranchFingerprintSet {
            recent_message_tail_hash: Some("tail-1".to_string()),
            last_user_message_hash: Some("user-1".to_string()),
            branch_state_hash: Some("state-1".to_string()),
            ..BranchFingerprintSet::default()
        };
        let branch = store
            .create_branch(
                "session-1",
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: Some(serde_json::json!([
                        { "role": "user", "content": "hello" }
                    ])),
                    fingerprints: fingerprints.clone(),
                },
            )
            .expect("create branch");
        store
            .commit_turn(
                "session-1",
                &branch.branch_id,
                &CommitTurnParams {
                    turn_scope: ConversationTurnScope::Main,
                    turn_id: "turn-1".to_string(),
                    fingerprints: fingerprints.clone(),
                    provider_response_id: Some("resp_1".to_string()),
                    previous_response_id: None,
                    provider_model_fingerprint: Some("gpt-5".to_string()),
                    request_compatibility_fingerprint: Some("fingerprint-1".to_string()),
                    provider_output_items: Vec::new(),
                },
            )
            .expect("commit");

        let branch_path = root
            .join("session-id-session-1")
            .join(format!("tab-{}", branch.branch_id))
            .join("branch.json");
        let mut corrupted: BranchMetadata =
            serde_json::from_slice(&std::fs::read(&branch_path).expect("read branch.json"))
                .expect("decode branch json");
        corrupted.current_checkpoint_id = Some("resp_wrong".to_string());
        std::fs::write(
            &branch_path,
            serde_json::to_vec_pretty(&corrupted).expect("encode corrupted branch"),
        )
        .expect("write corrupted branch");

        let recovered = store
            .select_or_create_branch(
                "session-1",
                &BranchSelectionInput {
                    active_canonical_messages: Some(serde_json::json!([
                        { "role": "user", "content": "hello" }
                    ])),
                    fingerprints,
                    turn_scope: ConversationTurnScope::Main,
                },
            )
            .expect("selection should quarantine and rebuild");
        assert!(root.join("session-id-session-1").exists());
        assert_ne!(recovered.branch.branch_id, branch.branch_id);

        let quarantine_root = store.quarantine_root();
        let quarantined = std::fs::read_dir(&quarantine_root)
            .expect("read quarantine root")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect quarantine entries");
        assert_eq!(quarantined.len(), 1);
        assert!(
            quarantined[0]
                .path()
                .join(format!("tab-{}", branch.branch_id))
                .exists()
        );
    }
}
