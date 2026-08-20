//! Pure, versioned run-journal schema and hash-chain verification.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Current run-journal schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// One immutable event in a run journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEventEnvelope {
    pub schema_version: u32,
    pub run_id: String,
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub kind: String,
    pub payload: Value,
    pub previous_hash: Option<String>,
    pub hash: String,
}

/// A pure in-memory event-chain builder.
#[derive(Debug, Clone)]
pub struct RunChain {
    run_id: String,
    events: Vec<RunEventEnvelope>,
}

impl RunChain {
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            events: Vec::new(),
        }
    }

    pub fn events(&self) -> &[RunEventEnvelope] {
        &self.events
    }

    pub fn append(
        &mut self,
        timestamp_ms: u64,
        kind: impl Into<String>,
        payload: Value,
    ) -> Result<&RunEventEnvelope, serde_json::Error> {
        let mut event = RunEventEnvelope {
            schema_version: SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            sequence: self.events.len() as u64,
            timestamp_ms,
            kind: kind.into(),
            payload,
            previous_hash: self.events.last().map(|event| event.hash.clone()),
            hash: String::new(),
        };
        event.hash = calculate_hash(&event)?;
        self.events.push(event);
        Ok(self.events.last().expect("the event was just appended"))
    }
}

/// Stable reasons why an in-memory chain cannot be verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationError {
    UnsupportedSchemaVersion {
        sequence: u64,
        expected: u32,
        actual: u32,
    },
    SequenceMismatch {
        expected: u64,
        actual: u64,
    },
    RunIdMismatch {
        sequence: u64,
        expected: String,
        actual: String,
    },
    BrokenLink {
        sequence: u64,
    },
    HashMismatch {
        sequence: u64,
    },
}

/// Verify each event's self-hash and link to its predecessor.
pub fn verify_chain(events: &[RunEventEnvelope]) -> Result<(), VerificationError> {
    let expected_run_id = events.first().map(|event| event.run_id.as_str());
    for (index, event) in events.iter().enumerate() {
        if event.schema_version != SCHEMA_VERSION {
            return Err(VerificationError::UnsupportedSchemaVersion {
                sequence: event.sequence,
                expected: SCHEMA_VERSION,
                actual: event.schema_version,
            });
        }
        let expected_sequence = index as u64;
        if event.sequence != expected_sequence {
            return Err(VerificationError::SequenceMismatch {
                expected: expected_sequence,
                actual: event.sequence,
            });
        }
        if Some(event.run_id.as_str()) != expected_run_id {
            return Err(VerificationError::RunIdMismatch {
                sequence: event.sequence,
                expected: expected_run_id.unwrap_or_default().to_string(),
                actual: event.run_id.clone(),
            });
        }
        let expected_previous = index
            .checked_sub(1)
            .map(|previous| events[previous].hash.as_str());
        if event.previous_hash.as_deref() != expected_previous {
            return Err(VerificationError::BrokenLink {
                sequence: event.sequence,
            });
        }
        let actual = calculate_hash(event).map_err(|_| VerificationError::HashMismatch {
            sequence: event.sequence,
        })?;
        if actual != event.hash {
            return Err(VerificationError::HashMismatch {
                sequence: event.sequence,
            });
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct HashMaterial<'a> {
    schema_version: u32,
    run_id: &'a str,
    sequence: u64,
    timestamp_ms: u64,
    kind: &'a str,
    payload: &'a Value,
    previous_hash: &'a Option<String>,
}

fn calculate_hash(event: &RunEventEnvelope) -> Result<String, serde_json::Error> {
    let material = HashMaterial {
        schema_version: event.schema_version,
        run_id: &event.run_id,
        sequence: event.sequence,
        timestamp_ms: event.timestamp_ms,
        kind: &event.kind,
        payload: &event.payload,
        previous_hash: &event.previous_hash,
    };
    let bytes = serde_json::to_vec(&material)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn first_event_starts_a_verifiable_chain() {
        let mut chain = super::RunChain::new("run-1");

        let event = chain
            .append(1_725_000_000_000, "run_started", json!({"surface": "cli"}))
            .unwrap();

        let _: u64 = event.timestamp_ms;
        assert_eq!(event.schema_version, 1);
        assert_eq!(event.run_id, "run-1");
        assert_eq!(event.sequence, 0);
        assert_eq!(event.previous_hash, None);
        assert_eq!(event.hash.len(), 64);
        assert_eq!(super::verify_chain(chain.events()), Ok(()));
    }

    #[test]
    fn second_event_links_to_the_first_hash() {
        let mut chain = super::RunChain::new("run-1");
        let first_hash = chain
            .append(1_725_000_000_000, "run_started", json!({}))
            .unwrap()
            .hash
            .clone();

        let second = chain
            .append(1_725_000_000_001, "request_prepared", json!({}))
            .unwrap();

        assert_eq!(second.sequence, 1);
        assert_eq!(second.previous_hash.as_deref(), Some(first_hash.as_str()));
        assert_eq!(super::verify_chain(chain.events()), Ok(()));
    }

    #[test]
    fn payload_mutation_fails_verification() {
        let mut chain = super::RunChain::new("run-1");
        chain
            .append(1_725_000_000_000, "tool_completed", json!({"exit_code": 0}))
            .unwrap();
        let mut events = chain.events().to_vec();
        events[0].payload["exit_code"] = json!(1);

        assert_eq!(
            super::verify_chain(&events),
            Err(super::VerificationError::HashMismatch { sequence: 0 })
        );
    }

    #[test]
    fn sequence_gap_fails_even_when_the_event_hash_was_recomputed() {
        let mut chain = super::RunChain::new("run-1");
        chain.append(1, "run_started", json!({})).unwrap();
        chain.append(2, "run_completed", json!({})).unwrap();
        let mut events = chain.events().to_vec();
        events[1].sequence = 2;
        events[1].hash = super::calculate_hash(&events[1]).unwrap();

        assert_eq!(
            super::verify_chain(&events),
            Err(super::VerificationError::SequenceMismatch {
                expected: 1,
                actual: 2,
            })
        );
    }

    #[test]
    fn cross_run_event_fails_even_when_the_event_hash_was_recomputed() {
        let mut chain = super::RunChain::new("run-1");
        chain.append(1, "run_started", json!({})).unwrap();
        chain.append(2, "run_completed", json!({})).unwrap();
        let mut events = chain.events().to_vec();
        events[1].run_id = "run-2".to_string();
        events[1].hash = super::calculate_hash(&events[1]).unwrap();

        assert_eq!(
            super::verify_chain(&events),
            Err(super::VerificationError::RunIdMismatch {
                sequence: 1,
                expected: "run-1".to_string(),
                actual: "run-2".to_string(),
            })
        );
    }

    #[test]
    fn unsupported_schema_fails_at_any_sequence_with_recomputed_hashes() {
        let mut chain = super::RunChain::new("run-1");
        chain.append(1, "run_started", json!({})).unwrap();
        chain.append(2, "run_completed", json!({})).unwrap();

        for sequence in 0..2 {
            let mut events = chain.events().to_vec();
            events[sequence].schema_version = 2;
            events[sequence].hash = super::calculate_hash(&events[sequence]).unwrap();
            if sequence == 0 {
                events[1].previous_hash = Some(events[0].hash.clone());
                events[1].hash = super::calculate_hash(&events[1]).unwrap();
            }

            assert_eq!(
                super::verify_chain(&events),
                Err(super::VerificationError::UnsupportedSchemaVersion {
                    sequence: sequence as u64,
                    expected: super::SCHEMA_VERSION,
                    actual: 2,
                })
            );
        }
    }

    #[test]
    fn unknown_event_kind_roundtrips_as_raw_json() {
        let mut chain = super::RunChain::new("run-1");
        chain
            .append(
                1_725_000_000_000,
                "future_event_kind",
                json!({"future": {"value": 7}}),
            )
            .unwrap();

        let encoded = serde_json::to_string(chain.events()).unwrap();
        let decoded: Vec<super::RunEventEnvelope> = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded[0].kind, "future_event_kind");
        assert_eq!(decoded[0].payload, json!({"future": {"value": 7}}));
        assert_eq!(super::verify_chain(&decoded), Ok(()));
    }
}
