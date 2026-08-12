//! Static footprint inference for Soroban smart contracts.
//!
//! `slipstream-analyzer` parses Rust contract source with [`syn`] and infers
//! which storage keys each function reads and writes, then runs a set of
//! heuristic detectors that flag known contention anti-patterns.
//!
//! This is a *static* approximation: key expressions that cannot be resolved
//! to a constant are marked dynamic, and detectors treat dynamic keys
//! conservatively. The heuristics are documented in `docs/DETECTORS.md`.

use std::collections::BTreeMap;
use std::path::Path;

use syn::visit::{self, Visit};
use syn::{Expr, ExprMethodCall, ItemFn};

pub mod detectors;

/// Identifier used for a key segment that cannot be statically resolved.
pub const DYNAMIC_SEGMENT: &str = "(dynamic)";

/// Methods treated as storage reads.
pub const READ_METHODS: &[&str] = &["get", "has"];
/// Methods treated as storage writes.
pub const WRITE_METHODS: &[&str] = &["put", "set", "remove", "del"];

/// A statically inferred contract storage key. The key is a sequence of
/// segments; if any segment is unresolvable the key is dynamic.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StaticKey {
    pub segments: Vec<String>,
}

impl StaticKey {
    /// A fully dynamic key.
    pub fn dynamic() -> Self {
        StaticKey {
            segments: vec![DYNAMIC_SEGMENT.to_string()],
        }
    }

    /// Whether any segment could not be resolved statically.
    pub fn is_dynamic(&self) -> bool {
        self.segments.iter().any(|s| s == DYNAMIC_SEGMENT)
    }

    /// The canonical string form of the key.
    pub fn as_key(&self) -> String {
        self.segments.join(".")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    Read,
    Write,
}

/// The storage accesses inferred for a single function.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FunctionAccess {
    pub function_name: String,
    pub storage_reads: Vec<StaticKey>,
    pub storage_writes: Vec<StaticKey>,
}

/// The result of statically analyzing one source file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnalysisReport {
    pub source_name: String,
    pub functions: Vec<FunctionAccess>,
    pub detectors: Vec<detectors::DetectorFinding>,
}

/// Errors produced while analyzing a source file.
#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    #[error("failed to parse source {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: syn::Error,
    },
    #[error("failed to read source {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Statically analyzes a directory or file of Rust contract sources.
pub fn analyze(source: &str, source_name: impl Into<String>) -> Result<AnalysisReport, syn::Error> {
    let source_name = source_name.into();
    let ast: syn::File = syn::parse_file(source)?;
    let mut visitor = StorageVisitor::default();
    visitor.visit_file(&ast);
    Ok(AnalysisReport {
        source_name,
        functions: aggregate(&visitor.accesses),
        detectors: detectors::run(&visitor.accesses),
    })
}

/// Reads and analyzes a single source file.
pub fn analyze_file(path: impl AsRef<Path>) -> Result<AnalysisReport, AnalyzeError> {
    let path = path.as_ref();
    let source = std::fs::read_to_string(path).map_err(|source| AnalyzeError::Io {
        path: path.display().to_string(),
        source,
    })?;
    analyze(&source, path.display().to_string()).map_err(|source| AnalyzeError::Parse {
        path: path.display().to_string(),
        source,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccessRecord {
    function: String,
    kind: AccessKind,
    key: StaticKey,
    method: String,
    in_loop: bool,
}

#[derive(Default)]
struct StorageVisitor {
    current_fn: Option<String>,
    loop_depth: usize,
    accesses: Vec<AccessRecord>,
}

impl<'ast> Visit<'ast> for StorageVisitor {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let prev = self.current_fn.replace(node.sig.ident.to_string());
        visit::visit_item_fn(self, node);
        self.current_fn = prev;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let prev = self.current_fn.replace(node.sig.ident.to_string());
        visit::visit_impl_item_fn(self, node);
        self.current_fn = prev;
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.loop_depth += 1;
        visit::visit_expr_while(self, node);
        self.loop_depth -= 1;
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.loop_depth += 1;
        visit::visit_expr_for_loop(self, node);
        self.loop_depth -= 1;
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.loop_depth += 1;
        visit::visit_expr_loop(self, node);
        self.loop_depth -= 1;
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let method = node.method.to_string();
        if let Some(kind) = classify(&method) {
            if is_storage_chain(&node.receiver) {
                if let Some(function) = self.current_fn.clone() {
                    let key = extract_key(node.args.first());
                    self.accesses.push(AccessRecord {
                        function,
                        kind,
                        key,
                        method,
                        in_loop: self.loop_depth > 0,
                    });
                }
            }
        }
        visit::visit_expr_method_call(self, node);
    }
}

fn classify(method: &str) -> Option<AccessKind> {
    if READ_METHODS.contains(&method) {
        Some(AccessKind::Read)
    } else if WRITE_METHODS.contains(&method) {
        Some(AccessKind::Write)
    } else {
        None
    }
}

/// Walks a method-call receiver chain and checks whether it references a
/// `storage` object (e.g. `env.storage().instance()`, `env.storage()`).
fn is_storage_chain(expr: &Expr) -> bool {
    match expr {
        Expr::Path(p) => p.path.segments.iter().any(|s| s.ident == "storage"),
        Expr::Field(f) => {
            matches!(&f.member, syn::Member::Named(ident) if ident == "storage")
                || is_storage_chain(&f.base)
        }
        Expr::MethodCall(m) => m.method == "storage" || is_storage_chain(&m.receiver),
        Expr::Call(c) => is_storage_chain(&c.func),
        Expr::Reference(r) => is_storage_chain(&r.expr),
        _ => false,
    }
}

/// Extracts a best-effort static representation of a storage key expression.
fn extract_key(arg: Option<&Expr>) -> StaticKey {
    let Some(arg) = arg else {
        return StaticKey::dynamic();
    };
    let arg = match arg {
        Expr::Reference(r) => &r.expr,
        e => e,
    };
    match arg {
        Expr::Paren(p) => extract_key(Some(&p.expr)),
        Expr::Group(g) => extract_key(Some(&g.expr)),
        Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Str(s) => StaticKey {
                segments: vec![s.value()],
            },
            syn::Lit::ByteStr(b) => StaticKey {
                segments: vec![String::from_utf8_lossy(&b.value()).into_owned()],
            },
            _ => StaticKey::dynamic(),
        },
        Expr::Path(p) => {
            // A multi-segment path (e.g. `DataKey::Owner`) is treated as a
            // static enum reference; a bare ident is most likely a runtime
            // variable and is treated conservatively as dynamic.
            if p.path.segments.len() > 1 {
                if let Some(seg) = p.path.segments.last() {
                    return StaticKey {
                        segments: vec![seg.ident.to_string()],
                    };
                }
            }
            StaticKey::dynamic()
        }
        // Symbol::new("name") / Name::new(...) style constructors.
        Expr::Call(c) => {
            if let Expr::Path(p) = c.func.as_ref() {
                if let Some(seg) = p.path.segments.last() {
                    if seg.ident == "new" {
                        return extract_key(c.args.first());
                    }
                }
            }
            StaticKey::dynamic()
        }
        // symbol_short! / symbol_long! macros carry a literal key name.
        Expr::Macro(m) => {
            let name = m
                .mac
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            if name == "symbol_short" || name == "symbol_long" {
                if let Ok(expr) = syn::parse2::<Expr>(m.mac.tokens.clone()) {
                    return extract_key(Some(&expr));
                }
            }
            StaticKey::dynamic()
        }
        _ => StaticKey::dynamic(),
    }
}

/// Collapses access records into per-function read/write key lists,
/// de-duplicated while preserving first-seen order.
fn aggregate(accesses: &[AccessRecord]) -> Vec<FunctionAccess> {
    let mut by_fn: BTreeMap<&str, Vec<&AccessRecord>> = BTreeMap::new();
    for a in accesses {
        by_fn.entry(a.function.as_str()).or_default().push(a);
    }
    by_fn
        .into_iter()
        .map(|(name, recs)| {
            let mut reads = Vec::new();
            let mut writes = Vec::new();
            for r in recs {
                let bucket = match r.kind {
                    AccessKind::Read => &mut reads,
                    AccessKind::Write => &mut writes,
                };
                if !bucket.contains(&r.key) {
                    bucket.push(r.key.clone());
                }
            }
            FunctionAccess {
                function_name: name.to_string(),
                storage_reads: reads,
                storage_writes: writes,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLOBAL_COUNTER: &str = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env, Symbol};

#[contract]
pub struct Counter;

#[contractimpl]
impl Counter {
    pub fn increment(env: Env) -> u32 {
        let mut count: u32 = env.storage().instance().get(&symbol_short!("count")).unwrap_or(0);
        count += 1;
        env.storage().instance().put(&symbol_short!("count"), &count);
        count
    }

    pub fn reset(env: Env) {
        env.storage().instance().put(&symbol_short!("count"), &0u32);
    }
}
"#;

    const LOOP_WRITER: &str = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, Env, Symbol};

#[contract]
pub struct BulkWriter;

#[contractimpl]
impl BulkWriter {
    pub fn write_all(env: Env, owner: Symbol, items: soroban_sdk::Vec<u32>) {
        for (i, item) in items.iter().enumerate() {
            let key = Symbol::new(&format!("{owner}_{i}"));
            env.storage().persistent().put(&key, &item);
        }
    }
}
"#;

    const RMW: &str = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};

#[contract]
pub struct Balance;

#[contractimpl]
impl Balance {
    pub fn bump(env: Env) -> u32 {
        let b: u32 = env.storage().instance().get(&symbol_short!("bal")).unwrap_or(0);
        env.storage().instance().put(&symbol_short!("bal"), &(b + 1));
        b
    }
}
"#;

    #[test]
    fn detects_reads_and_writes_per_function() {
        let report = analyze(GLOBAL_COUNTER, "counter.rs").expect("parses");
        let increment = report
            .functions
            .iter()
            .find(|f| f.function_name == "increment")
            .expect("increment fn");
        assert_eq!(increment.storage_reads.len(), 1);
        assert_eq!(increment.storage_writes.len(), 1);
        let reset = report
            .functions
            .iter()
            .find(|f| f.function_name == "reset")
            .expect("reset fn");
        assert_eq!(reset.storage_writes.len(), 1);
    }

    #[test]
    fn flags_shared_static_key_writes() {
        let report = analyze(GLOBAL_COUNTER, "counter.rs").expect("parses");
        let shared = report
            .detectors
            .iter()
            .filter(|d| d.detector == detectors::GLOBAL_STATIC_WRITE)
            .count();
        assert_eq!(shared, 1, "count key is written from two functions");
    }

    #[test]
    fn flags_writes_inside_loops_with_dynamic_keys() {
        let report = analyze(LOOP_WRITER, "bulk.rs").expect("parses");
        assert!(
            report
                .detectors
                .iter()
                .any(|d| d.detector == detectors::WRITE_IN_LOOP),
            "expected write-in-loop finding"
        );
        let write_all = report
            .functions
            .iter()
            .find(|f| f.function_name == "write_all")
            .expect("write_all fn");
        assert!(write_all.storage_writes[0].is_dynamic());
    }

    #[test]
    fn flags_read_modify_write_pattern() {
        let report = analyze(RMW, "rmw.rs").expect("parses");
        assert!(
            report
                .detectors
                .iter()
                .any(|d| d.detector == detectors::READ_MODIFY_WRITE),
            "expected read-modify-write finding"
        );
    }

    #[test]
    fn clean_contract_fires_no_detectors() {
        let src = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};

#[contract]
pub struct Settings;

#[contractimpl]
impl Settings {
    pub fn get(env: Env) -> u32 {
        env.storage().instance().get(&symbol_short!("value")).unwrap_or(0)
    }

    pub fn set(env: Env, v: u32) {
        env.storage().instance().put(&symbol_short!("value"), &v);
    }
}
"#;
        let report = analyze(src, "settings.rs").expect("parses");
        assert!(
            report.detectors.is_empty(),
            "expected no findings, got: {:#?}",
            report.detectors
        );
    }

    #[test]
    fn serde_round_trip() {
        let report = analyze(GLOBAL_COUNTER, "counter.rs").expect("parses");
        let json = serde_json::to_string(&report).expect("serialize");
        let back: AnalysisReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(report, back);
    }
}
