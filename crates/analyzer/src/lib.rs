//! Static footprint inference for Soroban smart contracts.
//!
//! `slipstream-analyzer` parses Rust contract source with [`syn`] and infers
//! which storage keys each function reads and writes, then runs a set of
//! heuristic detectors that flag known contention anti-patterns.
//!
//! This is a *static* approximation: key expressions that cannot be resolved
//! to a constant are marked dynamic, and detectors treat dynamic keys
//! conservatively. The heuristics are documented in `docs/DETECTORS.md`.
//!
//! Since real contention often spans contracts, the analyzer can also build a
//! lightweight call graph over a set of source files ([`analyze_set`]): method
//! calls on Soroban contract clients are resolved to functions in the analyzed
//! set where possible, and each function's *effective footprint* becomes its
//! own accesses unioned with the transitively called functions' accesses.
//! Calls that cannot be resolved statically are reported as `dynamic` so
//! detectors and consumers remain conservative rather than silently dropping
//! them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};
use syn::{Expr, ExprMethodCall, ItemFn};

pub mod detectors;

/// A per-function key: source file + function name.
type FuncKey = (String, String);
/// A per-function footprint: own reads, own writes.
type Footprint = (Vec<StaticKey>, Vec<StaticKey>);
/// Adjacency map: caller -> transitively reachable callee keys.
type Adjacency = BTreeMap<FuncKey, Vec<FuncKey>>;
/// Memoized effective footprints keyed by function.
type EffectiveMemo = BTreeMap<FuncKey, Footprint>;
/// The effective footprints of all functions in a set.
type OwnMap = BTreeMap<FuncKey, Footprint>;

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
    /// Effective reads: own reads unioned with the reads of every transitively
    /// called function (across the analyzed file set). Populated by
    /// [`analyze_set`]; identical to `storage_reads` when no calls resolve.
    pub effective_storage_reads: Vec<StaticKey>,
    /// Effective writes: see [`FunctionAccess::effective_storage_reads`].
    pub effective_storage_writes: Vec<StaticKey>,
}

impl FunctionAccess {}

/// A resolved call edge between two functions in an analyzed file set.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CallEdge {
    /// The function containing the call.
    pub from_function: String,
    /// The source file `from_function` lives in.
    pub from_source: String,
    /// The function the call resolved to.
    pub to_function: String,
    /// The source file `to_function` lives in.
    pub to_source: String,
}

/// A call that could not be resolved to a concrete function.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnresolvedCall {
    /// The function containing the call.
    pub from_function: String,
    /// The source file `from_function` lives in.
    pub from_source: String,
    /// The receiver expression written at the call site (e.g. `client`).
    pub receiver: String,
    /// The method name called, when it could be identified.
    pub method: Option<String>,
    /// Why the call was treated as dynamic / unresolved.
    pub reason: String,
}

/// The lightweight call graph built over an analyzed file set.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CallGraph {
    /// Resolved call edges.
    pub edges: Vec<CallEdge>,
    /// Calls that could not be resolved statically. These are never silently
    /// dropped: consumers must treat them as potentially touching any key.
    pub unresolved: Vec<UnresolvedCall>,
}

/// The result of statically analyzing one source file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnalysisReport {
    pub source_name: String,
    pub functions: Vec<FunctionAccess>,
    pub detectors: Vec<detectors::DetectorFinding>,
    /// The call graph this file participates in. For a single-file [`analyze`]
    /// call this is empty; [`analyze_set`] populates edges across files.
    pub call_graph: CallGraph,
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

/// Statically analyzes a single source string.
pub fn analyze(source: &str, source_name: impl Into<String>) -> Result<AnalysisReport, syn::Error> {
    let source_name = source_name.into();
    let ast: syn::File = syn::parse_file(source)?;
    let mut visitor = StorageVisitor::default();
    visitor.visit_file(&ast);
    Ok(AnalysisReport {
        source_name,
        functions: aggregate(&visitor.accesses),
        detectors: detectors::run(&visitor.accesses),
        call_graph: CallGraph::default(),
    })
}

/// Statically analyzes a set of source files together, building a call graph
/// across them and computing each function's effective footprint (own accesses
/// unioned with the transitively called functions' accesses).
///
/// The sources are `(source_name, source)` pairs; names should be unique so
/// call edges can be attributed to the right file. Calls on Soroban contract
/// clients (e.g. `client.increment(...)`) are resolved against the functions
/// defined across the set; calls that cannot be resolved are reported as
/// dynamic/unresolved and are never silently dropped.
pub fn analyze_set(sources: &[(String, String)]) -> Result<Vec<AnalysisReport>, syn::Error> {
    struct RawFile {
        source_name: String,
        accesses: Vec<AccessRecord>,
        calls: Vec<CallRecord>,
    }

    let mut files = Vec::with_capacity(sources.len());
    for (name, source) in sources {
        let ast: syn::File = syn::parse_file(source)?;
        let mut visitor = StorageVisitor::default();
        visitor.visit_file(&ast);
        files.push(RawFile {
            source_name: name.clone(),
            accesses: visitor.accesses,
            calls: visitor.calls,
        });
    }

    // Function index across the whole set: method name -> candidate targets.
    let mut index: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in &files {
        for a in &file.accesses {
            index
                .entry(a.function.clone())
                .or_default()
                .insert(file.source_name.clone());
        }
    }

    let mut edges = Vec::new();
    let mut unresolved = Vec::new();
    for file in &files {
        for call in &file.calls {
            match index.get(&call.method) {
                Some(sources) if sources.len() == 1 => {
                    edges.push(CallEdge {
                        from_function: call.function.clone(),
                        from_source: file.source_name.clone(),
                        to_function: call.method.clone(),
                        to_source: sources.iter().next().cloned().expect("one source"),
                    });
                }
                Some(sources) => {
                    unresolved.push(UnresolvedCall {
                        from_function: call.function.clone(),
                        from_source: file.source_name.clone(),
                        receiver: call.receiver.clone(),
                        method: Some(call.method.clone()),
                        reason: format!(
                            "`{}` is ambiguous across the file set ({} candidates)",
                            call.method,
                            sources.len()
                        ),
                    });
                }
                None => {
                    if NOISE_METHODS.contains(&call.method.as_str()) {
                        continue;
                    }
                    unresolved.push(UnresolvedCall {
                        from_function: call.function.clone(),
                        from_source: file.source_name.clone(),
                        receiver: call.receiver.clone(),
                        method: Some(call.method.clone()),
                        reason: format!(
                            "no function named `{}` in the analyzed file set",
                            call.method
                        ),
                    });
                }
            }
        }
    }

    // Own footprints per (source, function). Include functions that appear
    // only in call records (callers with no storage accesses of their own).
    let mut own: OwnMap = BTreeMap::new();
    for file in &files {
        for f in aggregate_with_calls(&file.accesses, &file.calls) {
            own.insert(
                (file.source_name.clone(), f.function_name.clone()),
                (f.storage_reads.clone(), f.storage_writes.clone()),
            );
        }
    }

    // Adjacency from resolved edges.
    let mut adj: Adjacency = BTreeMap::new();
    for e in &edges {
        adj.entry((e.from_source.clone(), e.from_function.clone()))
            .or_default()
            .push((e.to_source.clone(), e.to_function.clone()));
    }

    let mut memo: EffectiveMemo = BTreeMap::new();
    let mut effective = |key: &FuncKey| -> Footprint {
        let mut visiting = BTreeSet::new();
        compute_effective(key, &own, &adj, &mut memo, &mut visiting)
    };

    let mut reports = Vec::with_capacity(files.len());
    for file in &files {
        let mut functions = aggregate_with_calls(&file.accesses, &file.calls);
        for f in &mut functions {
            let (reads, writes) = effective(&(file.source_name.clone(), f.function_name.clone()));
            f.effective_storage_reads = reads;
            f.effective_storage_writes = writes;
        }
        reports.push(AnalysisReport {
            source_name: file.source_name.clone(),
            functions,
            detectors: detectors::run(&file.accesses),
            call_graph: CallGraph {
                edges: edges.clone(),
                unresolved: unresolved.clone(),
            },
        });
    }
    Ok(reports)
}

/// Computes the effective footprint of `key` (own accesses ∪ transitive
/// callees), memoized and cycle-safe.
fn compute_effective(
    key: &FuncKey,
    own: &OwnMap,
    adj: &Adjacency,
    memo: &mut EffectiveMemo,
    visiting: &mut BTreeSet<FuncKey>,
) -> Footprint {
    if let Some(m) = memo.get(key) {
        return m.clone();
    }
    if visiting.contains(key) {
        return own.get(key).cloned().unwrap_or_default();
    }
    visiting.insert(key.clone());
    let mut result = own.get(key).cloned().unwrap_or_default();
    if let Some(callees) = adj.get(key) {
        for callee in callees {
            let (creads, cwrites) = compute_effective(callee, own, adj, memo, visiting);
            for k in creads {
                if !result.0.contains(&k) {
                    result.0.push(k);
                }
            }
            for k in cwrites {
                if !result.1.contains(&k) {
                    result.1.push(k);
                }
            }
        }
    }
    visiting.remove(key);
    memo.insert(key.clone(), result.clone());
    result
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

/// Reads and analyzes a set of source files together (see [`analyze_set`]).
pub fn analyze_files(files: &[PathBuf]) -> Result<Vec<AnalysisReport>, AnalyzeError> {
    let mut sources = Vec::with_capacity(files.len());
    for f in files {
        let source = std::fs::read_to_string(f).map_err(|source| AnalyzeError::Io {
            path: f.display().to_string(),
            source,
        })?;
        sources.push((f.display().to_string(), source));
    }
    analyze_set(&sources).map_err(|source| AnalyzeError::Parse {
        path: "<file set>".to_string(),
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

/// A candidate cross-contract (or intra-contract) method call, e.g.
/// `client.increment(...)` or `self.helper(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallRecord {
    function: String,
    method: String,
    receiver: String,
    in_loop: bool,
}

/// Method names that are overwhelmingly standard-library / SDK helpers rather
/// than contract calls. These are skipped as *unresolved* noise, but a method
/// that actually resolves to a function in the analyzed set is still linked.
const NOISE_METHODS: &[&str] = &[
    "iter",
    "into_iter",
    "clone",
    "cloned",
    "copied",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "expect",
    "map",
    "filter",
    "collect",
    "enumerate",
    "zip",
    "fold",
    "for_each",
    "find",
    "any",
    "all",
    "count",
    "sum",
    "product",
    "len",
    "is_empty",
    "contains",
    "push",
    "pop",
    "insert",
    "remove",
    "extend",
    "get",
    "first",
    "last",
    "as_ref",
    "as_str",
    "as_bytes",
    "to_string",
    "to_vec",
    "to_u32",
    "to_i64",
    "to_u64",
    "from_xdr",
    "to_xdr",
    "bytes",
    "require_auth",
    "require_auth_for_args",
    "current_contract_address",
    "contract_id",
    "network",
    "ledger",
    "printer",
    "events",
    "storage",
    "invoke_contract",
];

#[derive(Default)]
struct StorageVisitor {
    current_fn: Option<String>,
    loop_depth: usize,
    accesses: Vec<AccessRecord>,
    calls: Vec<CallRecord>,
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
        if let Some(receiver) = call_receiver(&node.receiver) {
            if let Some(function) = self.current_fn.clone() {
                self.calls.push(CallRecord {
                    function,
                    method: node.method.to_string(),
                    receiver,
                    in_loop: self.loop_depth > 0,
                });
            }
        }
        visit::visit_expr_method_call(self, node);
    }
}

/// Returns a display name for a receiver that looks like a contract client
/// (e.g. `client.increment(...)`, `self.helper(...)`), or `None` for
/// receivers that are not client candidates (storage chains, `env`, ...).
fn call_receiver(expr: &Expr) -> Option<String> {
    if is_storage_chain(expr) {
        return None;
    }
    match expr {
        Expr::Reference(r) => call_receiver(&r.expr),
        Expr::Paren(p) => call_receiver(&p.expr),
        Expr::Group(g) => call_receiver(&g.expr),
        Expr::Path(p) => {
            // A bare `env.` method call is a host function, not a contract
            // call. Anything else (a client variable, `self`, etc.) is a
            // candidate.
            if p.path.segments.len() == 1 && p.path.segments[0].ident == "env" {
                return None;
            }
            Some(p.path.segments[0].ident.to_string())
        }
        Expr::Field(f) => match &f.member {
            syn::Member::Named(ident) => Some(ident.to_string()),
            syn::Member::Unnamed(idx) => Some(idx.index.to_string()),
        },
        Expr::Call(c) => call_receiver(&c.func),
        Expr::MethodCall(m) => call_receiver(&m.receiver),
        _ => None,
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
                storage_reads: reads.clone(),
                storage_writes: writes.clone(),
                effective_storage_reads: reads,
                effective_storage_writes: writes,
            }
        })
        .collect()
}

/// Like [`aggregate`], but also materializes functions that appear only in
/// call records (e.g. a caller that itself touches no storage). This keeps the
/// report and the effective-footprint computation complete across the set.
fn aggregate_with_calls(accesses: &[AccessRecord], calls: &[CallRecord]) -> Vec<FunctionAccess> {
    let mut by_fn: BTreeMap<&str, Vec<&AccessRecord>> = BTreeMap::new();
    for a in accesses {
        by_fn.entry(a.function.as_str()).or_default().push(a);
    }
    for c in calls {
        by_fn.entry(c.function.as_str()).or_default();
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
                storage_reads: reads.clone(),
                storage_writes: writes.clone(),
                effective_storage_reads: reads,
                effective_storage_writes: writes,
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

    const CALLER: &str = r##"
#![no_std]
use soroban_sdk::{contract, contractimpl, Address, Env};

#[contract]
pub struct Caller;

#[contractimpl]
impl Caller {
    pub fn run(env: Env, target: Address) {
        let client = CounterClient::new(&env, &target);
        client.increment();
    }
}
"##;

    const CALLEE: &str = r##"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};

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
}
"##;

    #[test]
    fn cross_contract_call_adds_callee_accesses() {
        let reports = analyze_set(&[
            ("caller.rs".to_string(), CALLER.to_string()),
            ("callee.rs".to_string(), CALLEE.to_string()),
        ])
        .expect("parses both");
        let caller = reports
            .iter()
            .find(|r| r.source_name == "caller.rs")
            .expect("caller report");
        let run = caller
            .functions
            .iter()
            .find(|f| f.function_name == "run")
            .expect("run fn");
        assert_eq!(run.effective_storage_reads.len(), 1, "includes callee read");
        assert_eq!(
            run.effective_storage_writes.len(),
            1,
            "includes callee write"
        );
        assert_eq!(run.storage_reads.len(), 0, "own reads are empty");
        assert!(
            caller
                .call_graph
                .edges
                .iter()
                .any(|e| e.from_function == "run" && e.to_function == "increment"),
            "call edge run -> increment: {:#?}",
            caller.call_graph.edges
        );
    }

    const NESTED_CALLER: &str = r##"
#![no_std]
use soroban_sdk::{contract, contractimpl, Address, Env};

#[contract]
pub struct Router;

#[contractimpl]
impl Router {
    pub fn dispatch(env: Env, target: Address) {
        let client = CounterClient::new(&env, &target);
        client.increment();
    }
}
"##;

    const NESTED_CALLEE: &str = r##"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};

#[contract]
pub struct Counter;

#[contractimpl]
impl Counter {
    pub fn increment(env: Env) -> u32 {
        let mut count: u32 = env.storage().instance().get(&symbol_short!("count")).unwrap_or(0);
        count += 1;
        env.storage().instance().put(&symbol_short!("count"), &count);
        self.touch()
    }

    fn touch(&self) -> u32 {
        self.env.storage().instance().get(&symbol_short!("count")).unwrap_or(0)
    }
}
"##;

    #[test]
    fn nested_calls_transitively_extend_effective_footprint() {
        let reports = analyze_set(&[
            ("router.rs".to_string(), NESTED_CALLER.to_string()),
            ("counter.rs".to_string(), NESTED_CALLEE.to_string()),
        ])
        .expect("parses both");
        let router = reports
            .iter()
            .find(|r| r.source_name == "router.rs")
            .expect("router report");
        let dispatch = router
            .functions
            .iter()
            .find(|f| f.function_name == "dispatch")
            .expect("dispatch fn");
        assert_eq!(
            dispatch.effective_storage_reads.len(),
            1,
            "transitive callee read included"
        );
        assert_eq!(
            dispatch.effective_storage_writes.len(),
            1,
            "transitive callee write included"
        );
    }

    #[test]
    fn unresolved_calls_are_reported_dynamic() {
        let reports = analyze_set(&[
            ("caller.rs".to_string(), CALLER.to_string()),
            (
                "other.rs".to_string(),
                r##"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};

#[contract]
pub struct Other;

#[contractimpl]
impl Other {
    pub fn unrelated(env: Env) -> u32 {
        env.storage().instance().get(&symbol_short!("x")).unwrap_or(0)
    }
}
"##
                .to_string(),
            ),
        ])
        .expect("parses both");
        let caller = reports
            .iter()
            .find(|r| r.source_name == "caller.rs")
            .expect("caller report");
        let unresolved = caller
            .call_graph
            .unresolved
            .iter()
            .find(|u| u.method.as_deref() == Some("increment"))
            .expect("increment is unresolved");
        assert_eq!(unresolved.receiver, "client");
        assert!(caller.call_graph.edges.is_empty());
    }

    #[test]
    fn call_graph_serializes() {
        let reports = analyze_set(&[
            ("caller.rs".to_string(), CALLER.to_string()),
            ("callee.rs".to_string(), CALLEE.to_string()),
        ])
        .expect("parses both");
        let caller = reports
            .iter()
            .find(|r| r.source_name == "caller.rs")
            .expect("caller report");
        let json = serde_json::to_string(&caller).expect("serialize");
        let back: AnalysisReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(caller.call_graph, back.call_graph);
    }
}
