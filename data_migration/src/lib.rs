//! Data migration, import/export utilities for Remitwise contracts.
//!
//! Supports multiple formats (JSON, binary, CSV), checksum validation,
//! version compatibility checks, and data integrity verification.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

/// Current schema version for migration compatibility.
pub const SCHEMA_VERSION: u32 = 1;

/// Minimum supported schema version for import.
pub const MIN_SUPPORTED_VERSION: u32 = 1;

/// Maximum allowed canonical payload size for migration snapshots.
///
/// @dev This caps memory and CPU spent on checksum generation, serialization,
/// and deserialization for a single migration payload.
pub const MAX_MIGRATION_PAYLOAD_BYTES: usize = 64 * 1024;

/// Maximum allowed number of logical records in a migration payload.
///
/// @dev Record counting is payload-specific:
/// - `RemittanceSplit`: always `1`
/// - `SavingsGoals`: `goals.len()`
/// - `Generic`: number of top-level map entries
pub const MAX_MIGRATION_RECORDS: usize = 1_024;

/// Maximum allowed serialized snapshot size accepted by JSON and binary imports.
///
/// @dev This is larger than `MAX_MIGRATION_PAYLOAD_BYTES` to account for
/// snapshot metadata and encoding overhead while still rejecting oversized
/// requests before deserialization.
pub const MAX_MIGRATION_SNAPSHOT_BYTES: usize = MAX_MIGRATION_PAYLOAD_BYTES + (32 * 1024);

/// Maximum allowed size for base64-encoded encrypted payload imports.
///
/// @dev Base64 expands 3 bytes of input into 4 bytes of output.
pub const MAX_ENCRYPTED_PAYLOAD_BYTES: usize = MAX_MIGRATION_PAYLOAD_BYTES.div_ceil(3) * 4;

/// Versioned migration event payload meant for indexing and historical tracking.
///
/// # Indexer Migration Guidance
/// - **v1**: Indexers should match on `MigrationEvent::V1`. This is the fundamental schema containing baseline metadata (contract, type, version, timestamp).
/// - **v2+**: Future schemas will add new variants (e.g., `MigrationEvent::V2`) potentially mapping to new data structures.
///
/// Indexers must be prepared to handle unknown variants gracefully (e.g., by logging a warning/alert) rather than crashing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MigrationEvent {
    V1(MigrationEventV1),
    // V2(MigrationEventV2), // Add in the future when schema changes and update indexers
}

/// Base migration event containing metadata about the migration operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationEventV1 {
    pub contract_id: String,
    pub migration_type: String, // e.g., "export", "import", "upgrade"
    pub version: u32,
    pub timestamp_ms: u64,
}

/// Export format for snapshot data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportFormat {
    /// Human-readable JSON.
    Json,
    /// Compact binary (bincode).
    Binary,
    /// CSV for spreadsheet compatibility (tabular exports).
    Csv,
    /// Opaque encrypted payload (caller handles encryption/decryption).
    Encrypted,
}

/// Snapshot header with version and checksum for integrity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotHeader {
    pub version: u32,
    pub checksum: String,
    pub format: String,
    pub created_at_ms: Option<u64>,
}

/// Full export snapshot for remittance split or other contract data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSnapshot {
    pub header: SnapshotHeader,
    pub payload: SnapshotPayload,
}

/// Payload variants per contract type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SnapshotPayload {
    RemittanceSplit(RemittanceSplitExport),
    SavingsGoals(SavingsGoalsExport),
    Generic(HashMap<String, serde_json::Value>),
}

impl SnapshotPayload {
    /// Return the logical record count used for migration guardrails.
    ///
    /// @dev Generic payloads count top-level entries as records so callers can
    /// chunk large datasets instead of sending one oversized import.
    pub fn record_count(&self) -> usize {
        match self {
            SnapshotPayload::RemittanceSplit(_) => 1,
            SnapshotPayload::SavingsGoals(export) => export.goals.len(),
            SnapshotPayload::Generic(entries) => entries.len(),
        }
    }
}

/// Exportable remittance split config (mirrors contract SplitConfig).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemittanceSplitExport {
    pub owner: String,
    pub spending_percent: u32,
    pub savings_percent: u32,
    pub bills_percent: u32,
    pub insurance_percent: u32,
}

/// Exportable savings goals list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavingsGoalsExport {
    pub next_id: u32,
    pub goals: Vec<SavingsGoalExport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavingsGoalExport {
    pub id: u32,
    pub owner: String,
    pub name: String,
    pub target_amount: i64,
    pub current_amount: i64,
    pub target_date: u64,
    pub locked: bool,
}

impl ExportSnapshot {
    fn payload_bytes(&self) -> Result<Vec<u8>, MigrationError> {
        canonical_payload_bytes(&self.payload)
    }

    fn checksum_for_payload_bytes(payload_bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(payload_bytes);
        hex::encode(hasher.finalize().as_ref())
    }

    /// Compute SHA256 checksum of the payload (canonical JSON).
    pub fn compute_checksum(&self) -> String {
        let payload_bytes = self
            .payload_bytes()
            .unwrap_or_else(|_| panic!("payload must be serializable"));
        Self::checksum_for_payload_bytes(&payload_bytes)
    }

    /// Verify stored checksum matches payload.
    pub fn verify_checksum(&self) -> bool {
        self.header.checksum == self.compute_checksum()
    }

    /// Check if snapshot version is supported for import.
    pub fn is_version_compatible(&self) -> bool {
        self.header.version >= MIN_SUPPORTED_VERSION && self.header.version <= SCHEMA_VERSION
    }

    /// Validate payload size and logical record bounds.
    ///
    /// @dev Export paths call this before serializing so oversized payloads fail
    /// fast. Import paths reuse the same checks after decoding the envelope.
    pub fn validate_payload_constraints(&self) -> Result<(), MigrationError> {
        let payload_bytes = self.payload_bytes()?;
        validate_payload_bounds(self.payload.record_count(), payload_bytes.len())
    }

    /// Validate snapshot for import: version, payload bounds, and checksum.
    pub fn validate_for_import(&self) -> Result<(), MigrationError> {
        if !self.is_version_compatible() {
            return Err(MigrationError::IncompatibleVersion {
                found: self.header.version,
                min: MIN_SUPPORTED_VERSION,
                max: SCHEMA_VERSION,
            });
        }
        let payload_bytes = self.payload_bytes()?;
        validate_payload_bounds(self.payload.record_count(), payload_bytes.len())?;
        if self.header.checksum != Self::checksum_for_payload_bytes(&payload_bytes) {
            return Err(MigrationError::ChecksumMismatch);
        }
        Ok(())
    }

    /// Build a new snapshot with correct version and checksum.
    pub fn new(payload: SnapshotPayload, format: ExportFormat) -> Self {
        let mut snapshot = Self {
            header: SnapshotHeader {
                version: SCHEMA_VERSION,
                checksum: String::new(),
                format: format_label(format),
                created_at_ms: None,
            },
            payload,
        };
        snapshot.header.checksum = snapshot.compute_checksum();
        snapshot
    }
}

fn format_label(f: ExportFormat) -> String {
    match f {
        ExportFormat::Json => "json".into(),
        ExportFormat::Binary => "binary".into(),
        ExportFormat::Csv => "csv".into(),
        ExportFormat::Encrypted => "encrypted".into(),
    }
}

fn canonical_payload_bytes(payload: &SnapshotPayload) -> Result<Vec<u8>, MigrationError> {
    match payload {
        SnapshotPayload::RemittanceSplit(export) => {
            serialize_json_bytes(&serde_json::json!({ "RemittanceSplit": export }))
        }
        SnapshotPayload::SavingsGoals(export) => {
            serialize_json_bytes(&serde_json::json!({ "SavingsGoals": export }))
        }
        SnapshotPayload::Generic(entries) => {
            let ordered_entries: BTreeMap<&str, &serde_json::Value> = entries
                .iter()
                .map(|(key, value)| (key.as_str(), value))
                .collect();
            serialize_json_bytes(&serde_json::json!({ "Generic": ordered_entries }))
        }
    }
}

fn serialize_json_bytes<T>(value: &T) -> Result<Vec<u8>, MigrationError>
where
    T: Serialize,
{
    serde_json::to_vec(value).map_err(|e| MigrationError::DeserializeError(e.to_string()))
}

fn validate_payload_bounds(record_count: usize, payload_len: usize) -> Result<(), MigrationError> {
    if record_count > MAX_MIGRATION_RECORDS {
        return Err(MigrationError::TooManyRecords {
            count: record_count,
            max: MAX_MIGRATION_RECORDS,
        });
    }
    if payload_len > MAX_MIGRATION_PAYLOAD_BYTES {
        return Err(MigrationError::PayloadTooLarge {
            size: payload_len,
            max: MAX_MIGRATION_PAYLOAD_BYTES,
        });
    }
    Ok(())
}

fn validate_snapshot_size(snapshot_len: usize) -> Result<(), MigrationError> {
    if snapshot_len > MAX_MIGRATION_SNAPSHOT_BYTES {
        return Err(MigrationError::SnapshotTooLarge {
            size: snapshot_len,
            max: MAX_MIGRATION_SNAPSHOT_BYTES,
        });
    }
    Ok(())
}

fn validate_encrypted_payload_size(encoded_len: usize) -> Result<(), MigrationError> {
    if encoded_len > MAX_ENCRYPTED_PAYLOAD_BYTES {
        return Err(MigrationError::PayloadTooLarge {
            size: encoded_len,
            max: MAX_ENCRYPTED_PAYLOAD_BYTES,
        });
    }
    Ok(())
}

/// Migration/import errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    IncompatibleVersion { found: u32, min: u32, max: u32 },
    ChecksumMismatch,
    PayloadTooLarge { size: usize, max: usize },
    SnapshotTooLarge { size: usize, max: usize },
    TooManyRecords { count: usize, max: usize },
    InvalidFormat(String),
    ValidationFailed(String),
    DeserializeError(String),
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::IncompatibleVersion { found, min, max } => {
                write!(
                    f,
                    "incompatible version {} (supported {}-{})",
                    found, min, max
                )
            }
            MigrationError::ChecksumMismatch => write!(f, "checksum mismatch"),
            MigrationError::PayloadTooLarge { size, max } => {
                write!(f, "payload too large: {} bytes (max {})", size, max)
            }
            MigrationError::SnapshotTooLarge { size, max } => {
                write!(f, "snapshot too large: {} bytes (max {})", size, max)
            }
            MigrationError::TooManyRecords { count, max } => {
                write!(f, "too many records: {} (max {})", count, max)
            }
            MigrationError::InvalidFormat(s) => write!(f, "invalid format: {}", s),
            MigrationError::ValidationFailed(s) => write!(f, "validation failed: {}", s),
            MigrationError::DeserializeError(s) => write!(f, "deserialize error: {}", s),
        }
    }
}

impl std::error::Error for MigrationError {}

/// Export snapshot to JSON bytes.
pub fn export_to_json(snapshot: &ExportSnapshot) -> Result<Vec<u8>, MigrationError> {
    snapshot.validate_payload_constraints()?;
    let bytes = serde_json::to_vec_pretty(snapshot)
        .map_err(|e| MigrationError::DeserializeError(e.to_string()))?;
    validate_snapshot_size(bytes.len())?;
    Ok(bytes)
}

/// Export snapshot to binary bytes (bincode).
pub fn export_to_binary(snapshot: &ExportSnapshot) -> Result<Vec<u8>, MigrationError> {
    snapshot.validate_payload_constraints()?;
    let bytes = bincode::serialize(snapshot)
        .map_err(|e| MigrationError::DeserializeError(e.to_string()))?;
    validate_snapshot_size(bytes.len())?;
    Ok(bytes)
}

/// Export to CSV (for tabular payloads only; e.g. goals list).
pub fn export_to_csv(payload: &SavingsGoalsExport) -> Result<Vec<u8>, MigrationError> {
    let payload_bytes = serialize_json_bytes(payload)?;
    validate_payload_bounds(payload.goals.len(), payload_bytes.len())?;

    let mut wtr = csv::Writer::from_writer(Vec::new());
    wtr.write_record([
        "id",
        "owner",
        "name",
        "target_amount",
        "current_amount",
        "target_date",
        "locked",
    ])
    .map_err(|e| MigrationError::InvalidFormat(e.to_string()))?;
    for g in &payload.goals {
        wtr.write_record(&[
            g.id.to_string(),
            g.owner.clone(),
            g.name.clone(),
            g.target_amount.to_string(),
            g.current_amount.to_string(),
            g.target_date.to_string(),
            g.locked.to_string(),
        ])
        .map_err(|e| MigrationError::InvalidFormat(e.to_string()))?;
    }
    wtr.flush()
        .map_err(|e| MigrationError::InvalidFormat(e.to_string()))?;
    let csv_bytes = wtr
        .into_inner()
        .map_err(|e| MigrationError::InvalidFormat(e.to_string()))?;
    if csv_bytes.len() > MAX_MIGRATION_PAYLOAD_BYTES {
        return Err(MigrationError::PayloadTooLarge {
            size: csv_bytes.len(),
            max: MAX_MIGRATION_PAYLOAD_BYTES,
        });
    }
    Ok(csv_bytes)
}

/// Encrypted format: store base64-encoded payload (caller encrypts before passing).
pub fn export_to_encrypted_payload(plain_bytes: &[u8]) -> Result<String, MigrationError> {
    if plain_bytes.len() > MAX_MIGRATION_PAYLOAD_BYTES {
        return Err(MigrationError::PayloadTooLarge {
            size: plain_bytes.len(),
            max: MAX_MIGRATION_PAYLOAD_BYTES,
        });
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(plain_bytes))
}

/// Decode encrypted payload from base64 (caller decrypts after).
pub fn import_from_encrypted_payload(encoded: &str) -> Result<Vec<u8>, MigrationError> {
    validate_encrypted_payload_size(encoded.len())?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| MigrationError::InvalidFormat(e.to_string()))
        .and_then(|bytes| {
            if bytes.len() > MAX_MIGRATION_PAYLOAD_BYTES {
                Err(MigrationError::PayloadTooLarge {
                    size: bytes.len(),
                    max: MAX_MIGRATION_PAYLOAD_BYTES,
                })
            } else {
                Ok(bytes)
            }
        })
}

/// Import snapshot from JSON bytes with validation.
pub fn import_from_json(bytes: &[u8]) -> Result<ExportSnapshot, MigrationError> {
    validate_snapshot_size(bytes.len())?;
    let snapshot: ExportSnapshot = serde_json::from_slice(bytes)
        .map_err(|e| MigrationError::DeserializeError(e.to_string()))?;
    snapshot.validate_for_import()?;
    Ok(snapshot)
}

/// Import snapshot from binary bytes with validation.
pub fn import_from_binary(bytes: &[u8]) -> Result<ExportSnapshot, MigrationError> {
    validate_snapshot_size(bytes.len())?;
    let snapshot: ExportSnapshot =
        bincode::deserialize(bytes).map_err(|e| MigrationError::DeserializeError(e.to_string()))?;
    snapshot.validate_for_import()?;
    Ok(snapshot)
}

/// Import goals from CSV into SavingsGoalsExport (no header checksum; use for merge/import).
pub fn import_goals_from_csv(bytes: &[u8]) -> Result<Vec<SavingsGoalExport>, MigrationError> {
    if bytes.len() > MAX_MIGRATION_PAYLOAD_BYTES {
        return Err(MigrationError::PayloadTooLarge {
            size: bytes.len(),
            max: MAX_MIGRATION_PAYLOAD_BYTES,
        });
    }

    let mut rdr = csv::Reader::from_reader(bytes);
    let mut goals = Vec::new();
    for result in rdr.deserialize() {
        if goals.len() == MAX_MIGRATION_RECORDS {
            return Err(MigrationError::TooManyRecords {
                count: MAX_MIGRATION_RECORDS + 1,
                max: MAX_MIGRATION_RECORDS,
            });
        }
        let record: CsvGoalRow =
            result.map_err(|e| MigrationError::DeserializeError(e.to_string()))?;
        goals.push(SavingsGoalExport {
            id: record.id,
            owner: record.owner,
            name: record.name,
            target_amount: record.target_amount,
            current_amount: record.current_amount,
            target_date: record.target_date,
            locked: record.locked,
        });
    }
    Ok(goals)
}

#[derive(Debug, Deserialize)]
struct CsvGoalRow {
    id: u32,
    owner: String,
    name: String,
    target_amount: i64,
    current_amount: i64,
    target_date: u64,
    locked: bool,
}

/// Version compatibility check for migration scripts.
pub fn check_version_compatibility(version: u32) -> Result<(), MigrationError> {
    if version >= MIN_SUPPORTED_VERSION && version <= SCHEMA_VERSION {
        Ok(())
    } else {
        Err(MigrationError::IncompatibleVersion {
            found: version,
            min: MIN_SUPPORTED_VERSION,
            max: SCHEMA_VERSION,
        })
    }
}

/// Rollback metadata (for migration scripts to record last good state).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackMetadata {
    pub previous_version: u32,
    pub previous_checksum: String,
    pub timestamp_ms: u64,
}

// Re-export hex for checksum display if needed; use hex crate for encoding in compute_checksum.
mod hex {
    const HEX: &[u8] = b"0123456789abcdef";
    pub fn encode(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0xf) as usize] as char);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_goal(id: u32) -> SavingsGoalExport {
        SavingsGoalExport {
            id,
            owner: "G1".into(),
            name: format!("Goal {id}"),
            target_amount: 1_000,
            current_amount: 100,
            target_date: 2_000_000_000,
            locked: false,
        }
    }

    fn sample_goals_export(count: usize) -> SavingsGoalsExport {
        SavingsGoalsExport {
            next_id: count as u32,
            goals: (1..=count as u32).map(sample_goal).collect(),
        }
    }

    #[test]
    fn test_snapshot_checksum_roundtrip_succeeds() {
        let payload = SnapshotPayload::RemittanceSplit(RemittanceSplitExport {
            owner: "GABC".into(),
            spending_percent: 50,
            savings_percent: 30,
            bills_percent: 15,
            insurance_percent: 5,
        });
        let snapshot = ExportSnapshot::new(payload, ExportFormat::Json);
        assert!(snapshot.verify_checksum());
        assert!(snapshot.is_version_compatible());
        assert!(snapshot.validate_for_import().is_ok());
    }

    #[test]
    fn test_export_import_json_succeeds() {
        let payload = SnapshotPayload::RemittanceSplit(RemittanceSplitExport {
            owner: "GXYZ".into(),
            spending_percent: 40,
            savings_percent: 40,
            bills_percent: 10,
            insurance_percent: 10,
        });
        let snapshot = ExportSnapshot::new(payload, ExportFormat::Json);
        let bytes = export_to_json(&snapshot).unwrap();
        let loaded = import_from_json(&bytes).unwrap();
        assert_eq!(loaded.header.version, SCHEMA_VERSION);
        assert!(loaded.verify_checksum());
    }

    #[test]
    fn test_export_import_binary_succeeds() {
        let payload = SnapshotPayload::RemittanceSplit(RemittanceSplitExport {
            owner: "GBIN".into(),
            spending_percent: 25,
            savings_percent: 25,
            bills_percent: 25,
            insurance_percent: 25,
        });
        let snapshot = ExportSnapshot::new(payload, ExportFormat::Binary);
        let bytes = export_to_binary(&snapshot).unwrap();
        let loaded = import_from_binary(&bytes).unwrap();
        assert!(loaded.verify_checksum());
    }

    #[test]
    fn test_checksum_mismatch_import_fails() {
        let payload = SnapshotPayload::RemittanceSplit(RemittanceSplitExport {
            owner: "GX".into(),
            spending_percent: 100,
            savings_percent: 0,
            bills_percent: 0,
            insurance_percent: 0,
        });
        let mut snapshot = ExportSnapshot::new(payload, ExportFormat::Json);
        snapshot.header.checksum = "wrong".into();
        assert!(!snapshot.verify_checksum());
        assert!(snapshot.validate_for_import().is_err());
    }

    #[test]
    fn test_check_version_compatibility_succeeds() {
        assert!(check_version_compatibility(1).is_ok());
        assert!(check_version_compatibility(SCHEMA_VERSION).is_ok());
        assert!(check_version_compatibility(0).is_err());
        assert!(check_version_compatibility(SCHEMA_VERSION + 1).is_err());
    }

    #[test]
    fn test_csv_export_import_goals_succeeds() {
        let export = SavingsGoalsExport {
            next_id: 2,
            goals: vec![SavingsGoalExport {
                locked: true,
                current_amount: 500,
                ..sample_goal(1)
            }],
        };
        let csv_bytes = export_to_csv(&export).unwrap();
        let goals = import_goals_from_csv(&csv_bytes).unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].name, "Goal 1");
        assert_eq!(goals[0].target_amount, 1000);
    }

    #[test]
    fn test_migration_event_serialization_succeeds() {
        let event = MigrationEvent::V1(MigrationEventV1 {
            contract_id: "CABCD".into(),
            migration_type: "export".into(),
            version: SCHEMA_VERSION,
            timestamp_ms: 123456789,
        });

        // Ensure we can serialize cleanly for indexers.
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""V1":{"#));
        assert!(json.contains(r#""contract_id":"CABCD""#));
        assert!(json.contains(r#""version":1"#));

        let loaded: MigrationEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, loaded);

        let MigrationEvent::V1(v1) = loaded;
        assert_eq!(v1.version, SCHEMA_VERSION);
    }

    #[test]
    fn test_export_rejects_payload_larger_than_limit() {
        let mut entries = HashMap::new();
        entries.insert(
            "blob".into(),
            serde_json::Value::String("x".repeat(MAX_MIGRATION_PAYLOAD_BYTES)),
        );
        let snapshot = ExportSnapshot::new(SnapshotPayload::Generic(entries), ExportFormat::Json);

        assert!(matches!(
            export_to_json(&snapshot),
            Err(MigrationError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn test_export_binary_rejects_too_many_records() {
        let payload = SnapshotPayload::SavingsGoals(sample_goals_export(MAX_MIGRATION_RECORDS + 1));
        let snapshot = ExportSnapshot::new(payload, ExportFormat::Binary);

        assert_eq!(
            export_to_binary(&snapshot),
            Err(MigrationError::TooManyRecords {
                count: MAX_MIGRATION_RECORDS + 1,
                max: MAX_MIGRATION_RECORDS,
            })
        );
    }

    #[test]
    fn test_import_json_rejects_oversized_snapshot_before_deserialize() {
        let oversized = vec![b' '; MAX_MIGRATION_SNAPSHOT_BYTES + 1];

        assert!(matches!(
            import_from_json(&oversized),
            Err(MigrationError::SnapshotTooLarge {
                size,
                max: MAX_MIGRATION_SNAPSHOT_BYTES,
            }) if size == MAX_MIGRATION_SNAPSHOT_BYTES + 1
        ));
    }

    #[test]
    fn test_import_binary_rejects_oversized_snapshot_before_deserialize() {
        let oversized = vec![0u8; MAX_MIGRATION_SNAPSHOT_BYTES + 1];

        assert!(matches!(
            import_from_binary(&oversized),
            Err(MigrationError::SnapshotTooLarge {
                size,
                max: MAX_MIGRATION_SNAPSHOT_BYTES,
            }) if size == MAX_MIGRATION_SNAPSHOT_BYTES + 1
        ));
    }

    #[test]
    fn test_export_accepts_max_record_count_when_payload_fits_size_limit() {
        let entries: HashMap<String, serde_json::Value> = (0..MAX_MIGRATION_RECORDS)
            .map(|idx| (format!("k{idx}"), serde_json::json!(idx)))
            .collect();
        let snapshot = ExportSnapshot::new(SnapshotPayload::Generic(entries), ExportFormat::Json);

        let bytes = export_to_json(&snapshot).unwrap();
        let loaded = import_from_json(&bytes).unwrap();

        assert_eq!(loaded.payload.record_count(), MAX_MIGRATION_RECORDS);
    }

    #[test]
    fn test_csv_export_rejects_too_many_records() {
        let export = sample_goals_export(MAX_MIGRATION_RECORDS + 1);

        assert_eq!(
            export_to_csv(&export),
            Err(MigrationError::TooManyRecords {
                count: MAX_MIGRATION_RECORDS + 1,
                max: MAX_MIGRATION_RECORDS,
            })
        );
    }

    #[test]
    fn test_csv_import_rejects_too_many_records() {
        let export = sample_goals_export(MAX_MIGRATION_RECORDS + 1);
        let mut csv =
            String::from("id,owner,name,target_amount,current_amount,target_date,locked\n");
        for goal in export.goals {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{}\n",
                goal.id,
                goal.owner,
                goal.name,
                goal.target_amount,
                goal.current_amount,
                goal.target_date,
                goal.locked
            ));
        }

        assert!(matches!(
            import_goals_from_csv(csv.as_bytes()),
            Err(MigrationError::TooManyRecords {
                count,
                max: MAX_MIGRATION_RECORDS,
            }) if count == MAX_MIGRATION_RECORDS + 1
        ));
    }

    #[test]
    fn test_encrypted_payload_roundtrip_at_size_limit_succeeds() {
        let plain = vec![42u8; MAX_MIGRATION_PAYLOAD_BYTES];
        let encoded = export_to_encrypted_payload(&plain).unwrap();

        assert_eq!(encoded.len(), MAX_ENCRYPTED_PAYLOAD_BYTES);
        assert_eq!(import_from_encrypted_payload(&encoded).unwrap(), plain);
    }

    #[test]
    fn test_import_from_encrypted_payload_rejects_oversized_input() {
        let oversized = "A".repeat(MAX_ENCRYPTED_PAYLOAD_BYTES + 1);

        assert_eq!(
            import_from_encrypted_payload(&oversized),
            Err(MigrationError::PayloadTooLarge {
                size: MAX_ENCRYPTED_PAYLOAD_BYTES + 1,
                max: MAX_ENCRYPTED_PAYLOAD_BYTES,
            })
        );
    }

    #[test]
    fn test_generic_payload_checksum_is_stable_across_map_order() {
        let mut first = HashMap::new();
        first.insert("b".into(), serde_json::json!(2));
        first.insert("a".into(), serde_json::json!(1));

        let mut second = HashMap::new();
        second.insert("a".into(), serde_json::json!(1));
        second.insert("b".into(), serde_json::json!(2));

        let first_snapshot =
            ExportSnapshot::new(SnapshotPayload::Generic(first), ExportFormat::Json);
        let second_snapshot =
            ExportSnapshot::new(SnapshotPayload::Generic(second), ExportFormat::Json);

        assert_eq!(
            first_snapshot.compute_checksum(),
            second_snapshot.compute_checksum()
        );
    }
}
