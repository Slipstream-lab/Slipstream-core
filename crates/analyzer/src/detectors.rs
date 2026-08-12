//! Heuristic detectors for contention anti-patterns.
//!
//! Detectors operate on the raw access records collected by the AST visitor.
//! They are intentionally conservative: they may produce false positives but
//! should never silently miss an obvious pattern. The semantics of each
//! detector are documented in `docs/DETECTORS.md`.

use std::collections::{BTreeMap, BTreeSet};

use crate::AccessRecord;

/// Detector name: a static, fully-resolvable key is written from multiple
/// functions. Indicates a global contention point.
pub const GLOBAL_STATIC_WRITE: &str = "global-static-write";

/// Detector name: a storage write occurs inside a loop body. Indicates
/// potential write amplification and unbounded footprint growth.
pub const WRITE_IN_LOOP: &str = "write-in-loop";

/// Detector name: a function both reads and writes the same key. Indicates a
/// read-modify-write pattern that serializes access to that key.
pub const READ_MODIFY_WRITE: &str = "read-modify-write";

/// Detector name: the same static key is read more than once in a function.
/// Indicates redundant reads / read amplification.
pub const DUPLICATE_READ: &str = "duplicate-read";

/// A single detector finding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DetectorFinding {
    /// The detector that produced the finding (one of the `*_WRITE`,
    /// `*_READ` constants above).
    pub detector: String,
    /// The function the finding is associated with, when relevant.
    pub function: Option<String>,
    /// The storage key the finding is associated with, when relevant.
    pub key: Option<String>,
    /// A human-readable explanation.
    pub message: String,
}

/// Runs all detectors over the raw access records.
pub(crate) fn run(accesses: &[AccessRecord]) -> Vec<DetectorFinding> {
    let mut findings = Vec::new();
    findings.extend(global_static_write(accesses));
    findings.extend(write_in_loop(accesses));
    findings.extend(read_modify_write(accesses));
    findings.extend(duplicate_read(accesses));
    findings
}

fn global_static_write(accesses: &[AccessRecord]) -> Vec<DetectorFinding> {
    let mut writers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for a in accesses
        .iter()
        .filter(|a| a.kind == crate::AccessKind::Write && !a.key.is_dynamic())
    {
        writers
            .entry(a.key.as_key())
            .or_default()
            .insert(a.function.clone());
    }
    writers
        .into_iter()
        .filter(|(_, functions)| functions.len() > 1)
        .map(|(key, functions)| {
            let mut fns: Vec<_> = functions.into_iter().collect();
            fns.sort();
            DetectorFinding {
                detector: GLOBAL_STATIC_WRITE.to_string(),
                function: None,
                key: Some(key.clone()),
                message: format!(
                    "static key `{key}` is written from multiple functions ({}); \
                     a global contention point that serializes concurrent access",
                    fns.join(", ")
                ),
            }
        })
        .collect()
}

fn write_in_loop(accesses: &[AccessRecord]) -> Vec<DetectorFinding> {
    accesses
        .iter()
        .filter(|a| a.kind == crate::AccessKind::Write && a.in_loop)
        .map(|a| DetectorFinding {
            detector: WRITE_IN_LOOP.to_string(),
            function: Some(a.function.clone()),
            key: Some(a.key.as_key()),
            message: format!(
                "function `{}` writes storage key `{}` inside a loop; \
                 potential write amplification and unbounded footprint growth",
                a.function,
                a.key.as_key()
            ),
        })
        .collect()
}

fn read_modify_write(accesses: &[AccessRecord]) -> Vec<DetectorFinding> {
    let mut by_fn: BTreeMap<&str, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    for a in accesses {
        let (reads, writes) = by_fn.entry(a.function.as_str()).or_default();
        match a.kind {
            crate::AccessKind::Read => {
                reads.insert(a.key.as_key());
            }
            crate::AccessKind::Write => {
                writes.insert(a.key.as_key());
            }
        }
    }
    by_fn
        .into_iter()
        .flat_map(|(function, (reads, writes))| {
            reads
                .intersection(&writes)
                .map(|key| DetectorFinding {
                    detector: READ_MODIFY_WRITE.to_string(),
                    function: Some(function.to_string()),
                    key: Some(key.clone()),
                    message: format!(
                        "function `{function}` both reads and writes key `{key}`; \
                         read-modify-write access serializes every writer to that key"
                    ),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn duplicate_read(accesses: &[AccessRecord]) -> Vec<DetectorFinding> {
    let mut by_fn: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
    for a in accesses
        .iter()
        .filter(|a| a.kind == crate::AccessKind::Read)
    {
        *by_fn
            .entry(a.function.as_str())
            .or_default()
            .entry(a.key.as_key())
            .or_default() += 1;
    }
    by_fn
        .into_iter()
        .flat_map(|(function, counts)| {
            counts
                .into_iter()
                .filter(|(_, count)| *count > 1)
                .map(|(key, count)| DetectorFinding {
                    detector: DUPLICATE_READ.to_string(),
                    function: Some(function.to_string()),
                    key: Some(key.clone()),
                    message: format!(
                        "function `{function}` reads key `{key}` {count} times; \
                         redundant reads amplify the read set"
                    ),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccessKind, StaticKey};

    fn rec(function: &str, kind: AccessKind, key: &str, in_loop: bool) -> AccessRecord {
        AccessRecord {
            function: function.to_string(),
            kind,
            key: StaticKey {
                segments: vec![key.to_string()],
            },
            method: String::new(),
            in_loop,
        }
    }

    #[test]
    fn global_static_write_needs_two_functions() {
        let accesses = vec![
            rec("a", AccessKind::Write, "count", false),
            rec("b", AccessKind::Write, "count", false),
            rec("a", AccessKind::Write, "own", false),
        ];
        let findings = run(&accesses);
        assert_eq!(
            findings
                .iter()
                .filter(|f| f.detector == GLOBAL_STATIC_WRITE)
                .count(),
            1
        );
        let f = findings
            .iter()
            .find(|f| f.detector == GLOBAL_STATIC_WRITE)
            .unwrap();
        assert_eq!(f.key.as_deref(), Some("count"));
    }

    #[test]
    fn single_function_write_is_not_global() {
        let accesses = vec![rec("a", AccessKind::Write, "count", false)];
        assert!(run(&accesses)
            .iter()
            .all(|f| f.detector != GLOBAL_STATIC_WRITE));
    }

    #[test]
    fn write_in_loop_fires_per_access() {
        let accesses = vec![
            rec("a", AccessKind::Write, "k", true),
            rec("a", AccessKind::Write, "k", false),
        ];
        let in_loop = run(&accesses)
            .iter()
            .filter(|f| f.detector == WRITE_IN_LOOP)
            .count();
        assert_eq!(in_loop, 1);
    }

    #[test]
    fn read_modify_write_detected() {
        let accesses = vec![
            rec("bump", AccessKind::Read, "bal", false),
            rec("bump", AccessKind::Write, "bal", false),
        ];
        assert!(run(&accesses)
            .iter()
            .any(|f| f.detector == READ_MODIFY_WRITE));
    }

    #[test]
    fn duplicate_read_detected() {
        let accesses = vec![
            rec("a", AccessKind::Read, "cfg", false),
            rec("a", AccessKind::Read, "cfg", false),
        ];
        assert!(run(&accesses).iter().any(|f| f.detector == DUPLICATE_READ));
    }
}
