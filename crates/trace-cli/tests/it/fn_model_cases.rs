//! Function-model summaries: dataflow through bodyless memory functions,
//! terminators (`clears`), and heap-return models.

use crate::common;

use common::*;
use std::sync::Arc;
use trace_analysis::{analyze_with_options, AnalyzeOptions, ResolutionKind};
use trace_db::open_db;
use trace_ir::Program;
use trace_parse::build_program;

fn build() -> Program {
    let root = fixture("fn_models");
    build_program(&root, &default_opts(&root)).expect("build")
}

fn analyze_no_models(program: &Program) -> trace_analysis::AnalysisResult {
    analyze_with_options(
        program,
        AnalyzeOptions {
            models: Arc::new(trace_analysis::FnModelSet::default()),
            ..Default::default()
        },
    )
    .1
}

fn analyze_builtin(program: &Program) -> trace_analysis::AnalysisResult {
    analyze_with_options(program, AnalyzeOptions::default()).1
}

fn indirect_targets(
    program: &Program,
    analysis: &trace_analysis::AnalysisResult,
    caller: &str,
) -> Vec<String> {
    analysis
        .call_edges
        .iter()
        .filter(|e| {
            fn_name(program, e.caller) == caller && e.resolution == ResolutionKind::Indirect
        })
        .map(|e| fn_name(program, e.callee))
        .collect()
}

#[test]
fn memcpy_s_model_flows_ops_table_to_copy() {
    let program = build();

    // Without models the copy is invisible and the target is missing.
    let base = analyze_no_models(&program);
    assert!(
        !indirect_targets(&program, &base, "copy_via_memcpy_s").contains(&"impl_run".to_string()),
        "baseline must not resolve impl_run through memcpy_s"
    );

    // With built-in models the ops table flows dst <- src (arg 2 -> arg 0).
    let with = analyze_builtin(&program);
    assert!(
        indirect_targets(&program, &with, "copy_via_memcpy_s").contains(&"impl_run".to_string()),
        "expected impl_run resolved through modeled memcpy_s"
    );
}

#[test]
fn user_toml_models_project_wrapper() {
    let root = fixture("fn_models");
    let toml_path = root.join("models.toml");
    let src = std::fs::read_to_string(&toml_path).expect("models.toml");

    let program = build();
    let base = analyze_no_models(&program);
    assert!(
        !indirect_targets(&program, &base, "call_through_wrapper_copy")
            .contains(&"impl_run".to_string()),
        "baseline must not resolve impl_run through unmodeled wrapper"
    );

    let opts = AnalyzeOptions {
        models: Arc::new(trace_analysis::FnModelSet::from_toml_str(&src).expect("parse toml")),
        ..Default::default()
    };
    let with = analyze_with_options(&program, opts).1;
    assert!(
        indirect_targets(&program, &with, "call_through_wrapper_copy")
            .contains(&"impl_run".to_string()),
        "expected impl_run resolved through TOML-modeled wrapper"
    );
}

#[test]
fn terminator_event_recorded_and_exported() {
    let program = build();

    // No terminator events without models.
    let base = analyze_no_models(&program);
    assert!(base.terminator_events.is_empty());

    let with = analyze_builtin(&program);
    assert_eq!(
        with.terminator_events.len(),
        1,
        "exactly one clears event expected (memset_s arg 0)"
    );
    assert_eq!(with.terminator_events[0].1, 0, "cleared parameter index");

    // Exported flow graph carries a terminator node with a terminates edge.
    let pag =
        trace_analysis::Pag::build_with_models(&program, &trace_analysis::FnModelSet::builtin());
    let db = export_program(&program, &pag, &with);
    let conn = open_db(&db).expect("open db");
    let (kind, label): (String, String) = conn
        .query_row(
            "SELECT kind, label FROM flow_nodes WHERE kind='terminator'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("terminator node exported");
    assert_eq!(kind, "terminator");
    assert!(label.contains("memset_s"), "label mentions callee: {label}");
    let n_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM flow_edges WHERE kind='terminates'",
            [],
            |r| r.get(0),
        )
        .expect("terminates edge count");
    assert_eq!(n_edges, 1);
    // Issue #127 review: the edge leaves the cleared object, not the `&x`
    // temporary lowering passes in its place.
    let cleared: String = conn
        .query_row(
            "SELECT s.label FROM flow_edges e JOIN flow_nodes_text s ON s.id = e.src_node \
             WHERE e.kind='terminates'",
            [],
            |r| r.get(0),
        )
        .expect("terminates edge source");
    assert_eq!(cleared, "g_cleared");
}

#[test]
fn malloc_return_heap_summary() {
    let program = build();
    let opts = AnalyzeOptions::default();
    let (pag, _analysis) = analyze_with_options(&program, opts);
    assert!(
        pag.locations.iter().any(|l| l.desc.contains("malloc")),
        "expected a fresh heap location for modeled malloc"
    );
}

#[test]
fn realloc_return_alias_keeps_pointees() {
    let program = build();
    // With the builtin realloc model the returned pointer may be the old
    // storage; without models the CallReturn expansion yields nothing.
    let opts = AnalyzeOptions {
        models: Arc::new(trace_analysis::FnModelSet::default()),
        ..Default::default()
    };
    let (pag_no, _) = analyze_with_options(&program, opts);
    let heap_before = pag_no
        .locations
        .iter()
        .filter(|l| l.desc.contains("realloc") || l.desc.contains("malloc"))
        .count();

    let (pag_with, _) = analyze_with_options(&program, AnalyzeOptions::default());
    let heap_after = pag_with
        .locations
        .iter()
        .filter(|l| l.desc.contains("realloc") || l.desc.contains("malloc"))
        .count();
    assert!(
        heap_after > heap_before,
        "modeled malloc/realloc must contribute fresh storage locations"
    );

    // return_alias: grow_ops's result may alias its parameter's storage.
    // The use_grow return chain must therefore carry impl_run's function
    // location is NOT expected here (no fn stored in OpsA instances via
    // heap), but the heap loc must reach use_alloc's return value.
    assert!(
        pag_with
            .locations
            .iter()
            .any(|l| l.kind == trace_analysis::LocKind::Heap),
        "heap loc kind must be used for modeled allocations"
    );
}

/// A `content_store` model adds its store mid-solve, after the value's
/// pointees may already have propagated: its first firing must still write
/// them. Covers both wiring paths — a direct call (models applied before the
/// worklist) and a call through a function pointer (applied when the edge is
/// discovered, after `f`'s set has settled).
#[test]
fn content_store_model_writes_values_present_before_wiring() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("store.c"),
        "typedef int (*op_t)(int);\n\
         static int impl_a(int x) { return x; }\n\
         static int impl_b(int x) { return x; }\n\
         void set_slot(op_t *slot, op_t fn);\n\
         op_t g_direct;\n\
         op_t g_indirect;\n\
         void (*g_setter)(op_t *, op_t) = set_slot;\n\
         void store_direct(void) { op_t f = impl_a; set_slot(&g_direct, f); (void)g_direct(1); }\n\
         void store_indirect(void) { op_t f = impl_b; g_setter(&g_indirect, f); (void)g_indirect(2); }\n",
    )
    .unwrap();
    let program = build_program(dir.path(), &default_opts(dir.path())).expect("build");

    let base = analyze_no_models(&program);
    for caller in ["store_direct", "store_indirect"] {
        let targets = indirect_targets(&program, &base, caller);
        assert!(
            !targets.iter().any(|t| t.starts_with("impl_")),
            "{caller} must not resolve through the unmodeled setter: {targets:?}"
        );
    }

    let models = trace_analysis::FnModelSet::from_toml_str(
        "version = 1\n\
         [[model]]\n\
         name = \"set_slot\"\n\
         effects = [ { kind = \"content_store\", ptr = 0, value = 1 } ]\n",
    )
    .expect("parse toml");
    let with = analyze_with_options(
        &program,
        AnalyzeOptions {
            models: Arc::new(models),
            ..Default::default()
        },
    )
    .1;
    for (caller, target) in [("store_direct", "impl_a"), ("store_indirect", "impl_b")] {
        let targets = indirect_targets(&program, &with, caller);
        assert!(
            targets.contains(&target.to_string()),
            "{caller}: expected {target} stored through the modeled setter: {targets:?}"
        );
    }
}

/// Argument and return models match the callee's exact name only
/// (`docs/MODELS.md`, "Model matching"): the libc `memset`, `memcpy` and
/// `malloc` models do not reach a class's or namespace's own `memset`,
/// `memcpy` or `malloc`, defined in the tree or not, while the plain,
/// `::`- and `std::`-qualified calls keep them.
#[test]
fn libc_models_skip_same_named_members() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
typedef unsigned long size_t;
extern "C" void *memset(void *s, int c, size_t n);
extern "C" void *memcpy(void *d, const void *s, size_t n);
extern "C" void *malloc(size_t n);
typedef int (*op_t)(int);
static int impl_a(int x) { return x; }
static int impl_b(int x) { return x; }
op_t g_member_cleared;
struct SecureBuffer {
    void *memset(void *p, int c, size_t n);
    void wipe() { memset(&g_member_cleared, 0, sizeof(op_t)); }
};
void *SecureBuffer::memset(void *p, int c, size_t n) { return p; }
struct Pool {
    void *memcpy(op_t *d, op_t *s, size_t n);
    int copy() {
        op_t src = impl_a;
        op_t dst = 0;
        memcpy(&dst, &src, sizeof(op_t));
        return dst(1);
    }
};
void *Pool::memcpy(op_t *d, op_t *s, size_t n) { return d; }
struct Arena { void *malloc(size_t n); };
op_t g_plain_cleared;
void plain_clear(void) { memset(&g_plain_cleared, 0, sizeof(op_t)); }
int plain_copy(void) {
    op_t src = impl_b;
    op_t dst = 0;
    memcpy(&dst, &src, sizeof(op_t));
    return dst(2);
}
void *member_alloc(Arena &arena) { void *p = arena.malloc(8); return p; }
void *plain_alloc(void) { void *q = malloc(8); return q; }
void *global_alloc(void) { void *r = ::malloc(8); return r; }
namespace secure { void *memset(void *p, int c, size_t n) { return p; } }
op_t g_ns_cleared;
void ns_clear(void) { secure::memset(&g_ns_cleared, 0, sizeof(op_t)); }
namespace std { using ::memset; }
op_t g_std_cleared;
void std_clear(void) { std::memset(&g_std_cleared, 0, sizeof(op_t)); }
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).expect("build");
    let (pag, analysis) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );

    let cleared_in: Vec<String> = analysis
        .terminator_events
        .iter()
        .map(|(cs, _)| {
            let site = program
                .symbols
                .call_sites
                .iter()
                .find(|s| s.id == *cs)
                .expect("terminator call site");
            fn_name(&program, site.caller)
        })
        .collect();
    assert_eq!(
        cleared_in,
        ["plain_clear", "std_clear"],
        "only libc memset clears"
    );

    assert!(
        indirect_targets(&program, &analysis, "plain_copy").contains(&"impl_b".to_string()),
        "libc memcpy copies the function pointer"
    );
    assert!(
        !indirect_targets(&program, &analysis, "Pool::copy").contains(&"impl_a".to_string()),
        "Pool::memcpy is not libc memcpy"
    );

    let points_to_heap = |function: &str, var: &str| {
        let var = local_variable(&program, function, var);
        pag.var_node
            .get(&var)
            .and_then(|n| analysis.points_to.get(n))
            .is_some_and(|pts| {
                pts.iter()
                    .any(|l| pag.locations[l.0 as usize].kind == trace_analysis::LocKind::Heap)
            })
    };
    assert!(points_to_heap("plain_alloc", "q"), "libc malloc allocates");
    assert!(
        points_to_heap("global_alloc", "r"),
        "::malloc is libc malloc"
    );
    assert!(
        !points_to_heap("member_alloc", "p"),
        "Arena::malloc is not libc malloc"
    );
}

/// `return_field` models a bodyless accessor: a call to `get_run(&g_ops)`
/// returns the `run` member of the object, so a function stored there is
/// reachable through the accessor's result (docs/MODELS.md, "Modeling
/// recipes (examples)").
#[test]
fn return_field_model_resolves_a_bodyless_accessor() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("getter.c"),
        "typedef int (*op_t)(int);\n\
         static int impl_run(int x) { return x; }\n\
         struct Ops { op_t run; int value; };\n\
         struct Ops g_ops;\n\
         void init_ops(void) { g_ops.run = impl_run; }\n\
         op_t get_run(struct Ops *o);\n\
         op_t get_null(struct Ops *o) { return 0; }\n\
         void use_getter(void) { op_t f = get_run(&g_ops); (void)f(1); }\n\
         void consume(op_t f) { (void)f(1); }\n\
         void use_nested(void) { consume(get_run(&g_ops)); }\n\
         void use_null_argument(void) { op_t f = get_run(0); (void)f(1); }\n\
         void use_indexed_body(void) { op_t f = get_null(&g_ops); (void)f(1); }\n",
    )
    .unwrap();
    let program = build_program(dir.path(), &default_opts(dir.path())).expect("build");

    let base = analyze_no_models(&program);
    assert!(
        !indirect_targets(&program, &base, "use_getter").contains(&"impl_run".to_string()),
        "baseline must not resolve through the unmodeled accessor"
    );

    let models = trace_analysis::FnModelSet::from_toml_str(
        "version = 1\n\
         [[model]]\n\
         name = \"get_run\"\n\
         effects = [ { kind = \"return_field\", param = 0, field = \"run\" } ]\n\
         [[model]]\n\
         name = \"get_null\"\n\
         effects = [ { kind = \"return_field\", param = 0, field = \"run\" } ]\n",
    )
    .expect("parse toml");
    let with = analyze_with_options(
        &program,
        AnalyzeOptions {
            models: Arc::new(models),
            ..Default::default()
        },
    )
    .1;
    assert!(
        indirect_targets(&program, &with, "use_getter").contains(&"impl_run".to_string()),
        "the accessor's result must reach the member stored in the object"
    );
    assert_eq!(indirect_targets(&program, &with, "consume"), ["impl_run"]);
    for caller in ["use_null_argument", "use_indexed_body"] {
        assert!(
            indirect_targets(&program, &with, caller).is_empty(),
            "{caller}"
        );
    }
}

#[test]
fn return_field_distinguishes_member_values_and_call_occurrences() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("getter.c"),
        "typedef int (*op_t)(int);\n\
         static int impl_run(int x) { return x; }\n\
         struct Ops { op_t run; op_t *indirect; };\n\
         struct Ops a, b;\n\
         op_t slot;\n\
         op_t *get_run_address(struct Ops *ops);\n\
         op_t *get_slot(struct Ops *ops);\n\
         void write_slot(void) {\n\
             op_t *p = get_slot(&a);\n\
             *p = impl_run;\n\
             a.indirect = &slot;\n\
             (void)slot(1);\n\
         }\n\
         void write_both(void) {\n\
             op_t *p = get_run_address(&a);\n\
             p = get_run_address(&b);\n\
             *p = impl_run;\n\
         }\n\
         op_t *wrap(void) { get_run_address(&a); return get_run_address(&b); }\n\
         void write_wrapped(void) { op_t *p = wrap(); *p = impl_run; }\n",
    )
    .unwrap();
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let models = trace_analysis::FnModelSet::from_toml_str(
        "version = 1\n\
         [[model]]\n\
         name = \"get_run_address\"\n\
         effects = [ { kind = \"return_field\", param = 0, field = \"run\" } ]\n\
         [[model]]\n\
         name = \"get_slot\"\n\
         effects = [ { kind = \"return_field\", param = 0, field = \"indirect\" } ]\n",
    )
    .unwrap();
    let (pag, analysis) = analyze_with_options(
        &program,
        AnalyzeOptions {
            models: Arc::new(models),
            ..Default::default()
        },
    );
    assert_eq!(
        indirect_targets(&program, &analysis, "write_slot"),
        ["impl_run"]
    );
    for (caller, expected) in [
        ("write_slot", &["slot"][..]),
        ("write_both", &["a", "b"][..]),
        ("write_wrapped", &["b"][..]),
    ] {
        let pointer = pag.var_node[&local_variable(&program, caller, "p")];
        let mut owners: Vec<_> = analysis
            .memory_accesses
            .iter()
            .filter_map(|access| {
                let constraint = &pag.constraints[access.constraint.0 as usize];
                if constraint.dst != pointer
                    || constraint.kind != trace_analysis::ConstraintKind::Store
                {
                    return None;
                }
                let loc = &pag.locations[access.loc?.0 as usize];
                Some(program.symbols.variable(loc.var?).name.as_str())
            })
            .collect();
        owners.sort_unstable();
        assert_eq!(owners, expected, "{caller}");
    }
}

#[test]
fn return_field_static_member_uses_explicit_argument() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("getter.cpp"),
        "using Op = int (*)(int);\n\
         struct Ops { Op run; };\n\
         struct Wrapper { Op run; static Op get(Ops *ops); };\n\
         int actual(int x) { return x; }\n\
         int wrong(int x) { return x; }\n\
         Ops ops;\n\
         Wrapper wrapper;\n\
         void setup() { ops.run = actual; wrapper.run = wrong; }\n\
         void through_object() { Ops *ps = &ops; Op f = wrapper.get(ps); (void)f(1); }\n\
         void through_class() { Op f = Wrapper::get(&ops); (void)f(1); }\n",
    )
    .unwrap();
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let models = trace_analysis::FnModelSet::from_toml_str(
        "version = 1\n\
         [[model]]\n\
         name = \"Wrapper::get\"\n\
         effects = [ { kind = \"return_field\", param = 0, field = \"run\" } ]\n",
    )
    .unwrap();
    let (_, analysis) = analyze_with_options(
        &program,
        AnalyzeOptions {
            models: Arc::new(models),
            ..Default::default()
        },
    );
    for caller in ["through_object", "through_class"] {
        assert_eq!(
            indirect_targets(&program, &analysis, caller),
            ["actual"],
            "{caller}"
        );
    }
}

/// The README's shape: a getter of a struct-valued member returns a pointer to
/// the member, and a later field access on that pointer (`get_ops(&d)->run`)
/// projects through the accessor's result. The member's field summary can
/// resolve the callback even without the model, so the projection is checked in
/// the getter result's points-to.
#[test]
fn return_field_model_projects_a_struct_member() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("nested_getter.c"),
        "typedef int (*op_t)(int);\n\
         static int impl_run(int x) { return x; }\n\
         struct Inner { op_t run; int value; };\n\
         struct Driver { struct Inner ops; int id; };\n\
         struct Driver g_driver;\n\
         void init_driver(void) { g_driver.ops.run = impl_run; }\n\
         struct Inner *get_ops(struct Driver *d);\n\
         void use_nested(void) { struct Inner *p = get_ops(&g_driver); (void)p->run(1); }\n",
    )
    .unwrap();
    let program = build_program(dir.path(), &default_opts(dir.path())).expect("build");

    let result_points_to = |models: trace_analysis::FnModelSet| {
        let (pag, analysis) = analyze_with_options(
            &program,
            AnalyzeOptions {
                retain_points_to: true,
                models: Arc::new(models),
                ..Default::default()
            },
        );
        let p = local_variable(&program, "use_nested", "p");
        analysis
            .points_to
            .get(&pag.var_node[&p])
            .cloned()
            .unwrap_or_default()
    };

    assert!(
        result_points_to(trace_analysis::FnModelSet::default()).is_empty(),
        "baseline: the unmodeled accessor's result has no pointee"
    );

    let models = trace_analysis::FnModelSet::from_toml_str(
        "version = 1\n\
         [[model]]\n\
         name = \"get_ops\"\n\
         effects = [ { kind = \"return_field\", param = 0, field = \"ops\" } ]\n",
    )
    .expect("parse toml");
    assert!(
        !result_points_to(models).is_empty(),
        "the accessor's result must point at the projected member"
    );
}

/// A class-member prototype records no parameter variables, so `return_field`
/// must take the object from the call site's actual argument (here the
/// receiver) rather than a formal (docs/MODELS.md, "Accessor getters"). Both
/// `p->get_ops()` and `obj.get_ops()` receivers must work.
#[test]
fn return_field_model_supports_an_in_class_prototype_accessor() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("proto_getter.cpp"),
        "typedef int (*op_t)(int);\n\
         struct Inner { op_t run; };\n\
         struct Driver { Inner ops; Inner *get_ops(); };\n\
         static int impl_run(int x) { return x; }\n\
         Driver g_driver;\n\
         void init_driver(void) { g_driver.ops.run = impl_run; }\n\
         void use_proto(Driver *d) { Inner *p = d->get_ops(); (void)p->run(1); }\n\
         void use_proto_obj(void) { Inner *q = g_driver.get_ops(); (void)q->run(1); }\n\
         void drive(void) { use_proto(&g_driver); }\n",
    )
    .unwrap();
    let program = build_program(dir.path(), &default_opts(dir.path())).expect("build");

    let result_points_to = |models: trace_analysis::FnModelSet, caller: &str, name: &str| {
        let (pag, analysis) = analyze_with_options(
            &program,
            AnalyzeOptions {
                retain_points_to: true,
                models: Arc::new(models),
                ..Default::default()
            },
        );
        let var = local_variable(&program, caller, name);
        analysis
            .points_to
            .get(&pag.var_node[&var])
            .cloned()
            .unwrap_or_default()
    };

    for (caller, name) in [("use_proto", "p"), ("use_proto_obj", "q")] {
        assert!(
            result_points_to(trace_analysis::FnModelSet::default(), caller, name).is_empty(),
            "{caller}: baseline prototype accessor result has no pointee"
        );
    }

    let models = trace_analysis::FnModelSet::from_toml_str(
        "version = 1\n\
         [[model]]\n\
         name = \"Driver::get_ops\"\n\
         effects = [ { kind = \"return_field\", param = 0, field = \"ops\" } ]\n",
    )
    .expect("parse toml");
    for (caller, name) in [("use_proto", "p"), ("use_proto_obj", "q")] {
        assert!(
            !result_points_to(models.clone(), caller, name).is_empty(),
            "{caller}: the prototype accessor's result must point at the projected member"
        );
    }
}

/// `return_field` needs a pointer-like result and a one-pointer-deep parameter
/// (docs/MODELS.md, "Documented imprecision"): a scalar-returning getter and a
/// getter whose parameter is `T **` project nothing.
#[test]
fn return_field_ignores_scalar_and_double_pointer_shapes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("shapes.c"),
        "typedef int (*op_t)(int);\n\
         static int impl_run(int x) { return x; }\n\
         struct Ops { op_t run; int value; };\n\
         struct Ops g_ops;\n\
         struct Ops *g_ptr;\n\
         void init_ops(void) { g_ops.run = impl_run; }\n\
         int get_value(struct Ops *o);\n\
         op_t get_deep(struct Ops **o);\n\
         void use_shapes(void) {\n\
             int v = get_value(&g_ops);\n\
             op_t f = get_deep(&g_ptr);\n\
             (void)v;\n\
             (void)f;\n\
         }\n",
    )
    .unwrap();
    let program = build_program(dir.path(), &default_opts(dir.path())).expect("build");

    let models = trace_analysis::FnModelSet::from_toml_str(
        "version = 1\n\
         [[model]]\n\
         name = \"get_value\"\n\
         effects = [ { kind = \"return_field\", param = 0, field = \"run\" } ]\n\
         [[model]]\n\
         name = \"get_deep\"\n\
         effects = [ { kind = \"return_field\", param = 0, field = \"run\" } ]\n",
    )
    .expect("parse toml");
    let (pag, analysis) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            models: Arc::new(models),
            ..Default::default()
        },
    );
    for result in ["v", "f"] {
        let var = local_variable(&program, "use_shapes", result);
        let points = analysis
            .points_to
            .get(&pag.var_node[&var])
            .cloned()
            .unwrap_or_default();
        assert!(
            points.is_empty(),
            "{result} must not receive a projected member"
        );
    }
}

#[test]
fn epoll_registration_reaches_wait_dispatch() {
    let root = fixture("epoll");
    let program = build_program(&root, &default_opts(&root)).expect("build epoll");
    let (pag, with) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    assert!(with.solve.converged);
    let overload_targets: Vec<_> = with
        .call_edges
        .iter()
        .filter(|edge| fn_name(&program, edge.caller) == "other::wait_overload")
        .map(|edge| program.symbols.function(edge.callee))
        .collect();
    assert_eq!(overload_targets.len(), 1);
    assert_eq!(overload_targets[0].name, "epoll_pwait");
    assert!(overload_targets[0].is_cpp && !overload_targets[0].c_linkage);
    let registration = program
        .symbols
        .call_sites
        .iter()
        .find(|cs| {
            fn_name(&program, cs.caller) == "Observer::monitor"
                && cs.callee_name == "register_event"
        })
        .expect("nested registration");
    let payload = registration
        .var_args
        .iter()
        .find(|(index, _)| *index == 2)
        .unwrap()
        .1;
    assert!(with.points_to[&pag.var_node[&payload]].iter().any(|loc| {
        let ty = pag.locations[loc.0 as usize].type_id;
        matches!(program.types.get(ty).desc.as_ref(), trace_ir::TypeDesc::Struct { name, .. } if name == "LevelHandler")
    }), "the registration passes the handler subobject's address");
    for caller in [
        "HandleEpollEvent",
        "pwait_events",
        "pwait2_events",
        "Observer::handle",
    ] {
        assert!(
            indirect_targets(&program, &with, caller).contains(&"HandleLevelReport".into()),
            "{caller} must receive the registered handler"
        );
    }
    assert!(
        indirect_targets(&program, &with, "Observer::handle").contains(&"NestedReport".into()),
        "nested handler must flow"
    );
    assert!(indirect_targets(&program, &with, "only_wait").is_empty());
    let variable = |name: &str| {
        program
            .symbols
            .variables
            .iter()
            .find(|v| v.name == name)
            .unwrap()
            .id
    };
    let received = pag.var_node[&variable("late_events")];
    for source in [
        "late_registration",
        "array_registration",
        "mutable_registration",
    ] {
        let source_loc = pag.var_location[&variable(source)];
        assert!(
            with.points_to
                .get(&received)
                .is_some_and(|pts| pts.contains(&source_loc)),
            "late-discovered endpoints must propagate {source}'s address"
        );
    }
    // Literal null registrations must not appear as sources of wait buffers.
    let wait_nodes: Vec<_> = program
        .symbols
        .call_sites
        .iter()
        .filter(|cs| {
            matches!(
                cs.callee_name.as_str(),
                "epoll_wait" | "epoll_pwait" | "epoll_pwait2" | "wait_api"
            )
        })
        .filter_map(|cs| pag.argument_var(cs, 1))
        .map(|var| pag.var_node[&var])
        .collect();
    for flow in &program.flow {
        if let trace_ir::FlowConstraint::NullPointer { dst } = flow {
            if program.symbols.variable(*dst).temp.is_none() {
                continue; // a real pointer variable can receive a value later
            }
            assert!(
                !pag.constraints.iter().any(|c| c.src == pag.var_node[dst]
                    && c.kind == trace_analysis::ConstraintKind::Copy
                    && wait_nodes.contains(&c.dst)),
                "literal-null temporary {} must not copy into a wait buffer",
                program.symbols.variable(*dst).name
            );
            assert!(with
                .points_to
                .get(&pag.var_node[dst])
                .is_none_or(|pts| pts.is_empty()));
        }
    }
    // FieldSummary may already resolve the dispatch without models. Pin
    // the actual new bridge in the exported graph, including its direction.
    let db = export_program(&program, &pag, &with);
    let conn = open_db(&db).expect("open epoll db");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM flow_edges e
         JOIN flow_edges a ON a.dst_node=e.src_node AND a.kind='addr_of'
         JOIN flow_nodes_text s ON s.id=a.src_node
         JOIN flow_nodes_text d ON d.id=e.dst_node
         WHERE e.kind='copy' AND s.label='registration of registration' AND d.label='events'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 5,
        "only the five libc wait buffers receive registrations"
    );
    let late: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM flow_edges e
         JOIN flow_edges a ON a.dst_node=e.src_node AND a.kind='addr_of'
         JOIN flow_nodes_text s ON s.id=a.src_node
         JOIN flow_nodes_text d ON d.id=e.dst_node
         WHERE e.kind='copy' AND s.label='late_registration of late_registration' AND d.label='late_events'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(late, 1, "indirect API calls must wire the epoll flow once");
    let (base_pag, base) = analyze_with_options(
        &program,
        AnalyzeOptions {
            models: Arc::new(trace_analysis::FnModelSet::default()),
            ..Default::default()
        },
    );
    let base_db = export_program(&program, &base_pag, &base);
    let base_conn = open_db(&base_db).unwrap();
    let count: i64 = base_conn
        .query_row(
            "SELECT COUNT(*) FROM flow_edges e
         JOIN flow_edges a ON a.dst_node=e.src_node AND a.kind='addr_of'
         JOIN flow_nodes_text s ON s.id=a.src_node
         JOIN flow_nodes_text d ON d.id=e.dst_node
         WHERE e.kind='copy' AND s.label='registration of registration' AND d.label='events'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "epoll flow requires the models");
}

#[test]
fn alias_models_accept_precise_members_but_reject_container_fallbacks() {
    let root = fixture("member_alias");
    let program = build_program(&root, &default_opts(&root)).unwrap();
    let models = trace_analysis::FnModelSet::from_toml_str(
        "version = 1\n[[model]]\nname = \"Alias\"\neffects = [{kind = \"alias\", dst = 0, src = 1}]\n",
    ).unwrap();
    for enabled in [false, true] {
        let (pag, analysis) = analyze_with_options(
            &program,
            AnalyzeOptions {
                retain_points_to: true,
                models: Arc::new(if enabled {
                    models.clone()
                } else {
                    trace_analysis::FnModelSet::default()
                }),
                ..Default::default()
            },
        );
        let payload = program
            .symbols
            .variables
            .iter()
            .find(|v| v.name == "payload")
            .unwrap()
            .id;
        let loc = pag.var_location[&payload];
        for cs in program
            .symbols
            .call_sites
            .iter()
            .filter(|cs| cs.callee_name == "Alias")
        {
            let precise = fn_name(&program, cs.caller) == "precise";
            assert_eq!(cs.addr_of_member_args().is_empty(), precise);
            let dst = cs.var_args.iter().find(|(index, _)| *index == 0).unwrap().1;
            let reached = analysis
                .points_to
                .get(&pag.var_node[&dst])
                .is_some_and(|pts| pts.contains(&loc));
            assert_eq!(
                reached,
                enabled && precise,
                "models={enabled}, precise={precise}"
            );
        }
    }
}

#[test]
fn adl_overloads_do_not_take_builtin_libc_models() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
using size_t = decltype(sizeof(0));
struct Tag {};
void memcpy(Tag*, Tag*) {}
extern "C" void *memcpy(void *, const void *, size_t);
namespace app {
void memcpy(int);
Tag tag;
Tag* destination;
Tag* source = &tag;
void copy() { memcpy(destination, source); }
}
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    let targets: Vec<_> = result
        .call_edges
        .iter()
        .filter(|edge| fn_name(&program, edge.caller) == "app::copy")
        .map(|edge| program.symbols.function(edge.callee))
        .collect();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].name, "memcpy");
    assert!(targets[0].is_cpp && !targets[0].c_linkage);
    let destination = program
        .symbols
        .variables
        .iter()
        .find(|v| v.name == "destination")
        .unwrap();
    assert!(result
        .points_to
        .get(&pag.var_node[&destination.id])
        .is_none_or(|pts| pts.is_empty()));
}

#[test]
fn libc_allocation_models_follow_the_selected_call_target() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
struct Tag {};
Tag* malloc(Tag*) { return nullptr; }
namespace std { extern "C" void* malloc(unsigned long); }
namespace app {
void malloc(int, int);
Tag tag;
void unqualified() { Tag* p = malloc(&tag); }
void qualified() { Tag* p = ::malloc(&tag); }
void genuine() { void* p = std::malloc(sizeof(Tag)); }
}
namespace ordinary {
void unqualified() { Tag* p = malloc(&app::tag); }
Tag* forwarded() { return malloc(&app::tag); }
void through_return() { Tag* p = forwarded(); }
}
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for caller in [
        "app::unqualified",
        "app::qualified",
        "ordinary::unqualified",
        "ordinary::forwarded",
        "app::genuine",
    ] {
        let targets: Vec<_> = result
            .call_edges
            .iter()
            .filter(|edge| fn_name(&program, edge.caller) == caller)
            .map(|edge| program.symbols.function(edge.callee))
            .collect();
        assert_eq!(targets.len(), 1, "{caller}");
        assert_eq!(
            targets[0].name,
            if caller == "app::genuine" {
                "std::malloc"
            } else {
                "malloc"
            },
            "{caller}"
        );
        assert_eq!(targets[0].c_linkage, caller == "app::genuine", "{caller}");
    }
    for (caller, allocates) in [
        ("app::unqualified", false),
        ("app::qualified", false),
        ("ordinary::unqualified", false),
        ("ordinary::through_return", false),
        ("app::genuine", true),
    ] {
        let var = local_variable(&program, caller, "p");
        let heap = result
            .points_to
            .get(&pag.var_node[&var])
            .is_some_and(|pts| {
                pts.iter()
                    .any(|l| pag.locations[l.0 as usize].kind == trace_analysis::LocKind::Heap)
            });
        assert_eq!(heap, allocates, "{caller}");
    }
}

#[test]
fn allocation_models_keep_calls_to_the_same_destination_separate() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
struct Tag {} tag;
Tag* malloc(Tag*) { return nullptr; }
namespace std { extern "C" void* malloc(unsigned long); }
#define ASSIGN(dst, callee, arg) dst = callee(arg)
void run() {
    void* p;
    ASSIGN(p, std::malloc, 8UL);
    ASSIGN(p, ::malloc, &tag);
}
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    let dst = local_variable(&program, "run", "p");
    let pointees = &result.points_to[&pag.var_node[&dst]];
    assert_eq!(pointees.len(), 1, "{pointees:?}");
    let loc = &pag.locations[pointees.iter().next().unwrap().0 as usize];
    assert_eq!(loc.kind, trace_analysis::LocKind::Heap);
}

#[test]
fn unselected_overload_returns_cannot_suppress_libc_allocation() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
struct Tag {} tag;
extern "C" void* malloc(unsigned long);
Tag* malloc(Tag*, int) { return &tag; }
void run() { void* p = malloc(8UL); }
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    let allocators: Vec<_> = result
        .call_edges
        .iter()
        .filter(|edge| fn_name(&program, edge.caller) == "run")
        .map(|edge| program.symbols.function(edge.callee))
        .collect();
    assert_eq!(allocators.len(), 1);
    assert_eq!(allocators[0].name, "malloc");
    assert!(allocators[0].c_linkage);
    let dst = local_variable(&program, "run", "p");
    let pointees = &result.points_to[&pag.var_node[&dst]];
    assert_eq!(pointees.len(), 1, "{pointees:?}");
    let loc = &pag.locations[pointees.iter().next().unwrap().0 as usize];
    assert_eq!(loc.kind, trace_analysis::LocKind::Heap);
}

#[test]
fn namespaced_adl_allocator_overload_controls_return_flow() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
extern "C" void* malloc(unsigned long);
namespace app {
struct Tag {};
Tag* malloc(Tag*) { return nullptr; }
}

void adl(app::Tag* tag) { void* p = malloc(tag); }
void* forward(app::Tag* tag) { return malloc(tag); }
void forwarded(app::Tag* tag) { void* p = forward(tag); }
void libc() { void* p = malloc(8UL); }
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for caller in ["adl", "forward"] {
        let targets: Vec<_> = result
            .call_edges
            .iter()
            .filter(|edge| fn_name(&program, edge.caller) == caller)
            .map(|edge| fn_name(&program, edge.callee))
            .collect();
        assert_eq!(targets, ["app::malloc"], "{caller}");
    }
    for (caller, allocates) in [("adl", false), ("forwarded", false), ("libc", true)] {
        let dst = local_variable(&program, caller, "p");
        let has_heap = result
            .points_to
            .get(&pag.var_node[&dst])
            .is_some_and(|pts| {
                pts.iter()
                    .any(|loc| pag.locations[loc.0 as usize].kind == trace_analysis::LocKind::Heap)
            });
        assert_eq!(has_heap, allocates, "{caller}");
    }
}

#[test]
fn adl_selected_c_allocator_supplies_return_flow() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
extern "C" void* malloc(unsigned long);
struct Size { operator unsigned long() const { return 8; } };
namespace app {
void* malloc(int, int);
void direct(Size size) { void* p = malloc(size); }
void* forward(Size size) { return malloc(size); }
void forwarded(Size size) { void* p = forward(size); }
}
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for caller in ["app::direct", "app::forward"] {
        let targets: Vec<_> = result
            .call_edges
            .iter()
            .filter(|edge| fn_name(&program, edge.caller) == caller)
            .map(|edge| fn_name(&program, edge.callee))
            .collect();
        assert_eq!(targets, ["malloc"], "{caller}");
    }
    for caller in ["app::direct", "app::forwarded"] {
        let dst = local_variable(&program, caller, "p");
        let pointees = &result.points_to[&pag.var_node[&dst]];
        assert_eq!(pointees.len(), 1, "{caller}: {pointees:?}");
        let loc = &pag.locations[pointees.iter().next().unwrap().0 as usize];
        assert_eq!(loc.kind, trace_analysis::LocKind::Heap, "{caller}");
    }
}

#[test]
fn file_scope_allocator_returns_follow_adl_selection() {
    let dir = scratch(&[
        (
            "main.cpp",
            r#"
extern "C" void* malloc(unsigned long);
namespace app {
struct Tag {};
Tag tag;
Tag* malloc(Tag*);
}
namespace address {
struct Tag {};
Tag address_tag;
Tag* malloc(Tag*) { return &address_tag; }
}
namespace identity {
struct Tag {};
Tag identity_tag;
Tag* malloc(Tag*);
}
struct Size { operator unsigned long() const { return 8; } };
void* no_allocation = malloc(&app::tag);
void* adl_address = malloc(&address::address_tag);
void* libc_allocation = malloc(8UL);
void* adl_argument = malloc(&identity::identity_tag);
namespace inner {
void* malloc(int, int);
Size size;
void* adl_allocation = malloc(size);
}
"#,
        ),
        (
            "casts.cpp",
            r#"
extern "C" void* malloc(unsigned long);
struct CastTag {};
CastTag cast_tag;
CastTag* malloc(CastTag*) { return &cast_tag; }
void* cast_allocation = malloc(((unsigned long)8));
void* cast_address = malloc((CastTag*)&cast_tag);
"#,
        ),
        (
            "overload.cpp",
            r#"
namespace app { struct Tag {}; Tag* malloc(Tag*) { return nullptr; } }
namespace identity { struct Tag {}; Tag* malloc(Tag* value) { return value; } }
"#,
        ),
    ]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    let pointees = |name| {
        let dst = only_variable(&program, name);
        result
            .points_to
            .get(&pag.var_node[&dst])
            .into_iter()
            .flatten()
    };
    assert_eq!(pointees("no_allocation").count(), 0);
    for (result, object) in [
        ("adl_address", "address_tag"),
        ("cast_address", "cast_tag"),
        ("adl_argument", "identity_tag"),
    ] {
        let tag = only_variable(&program, object);
        assert_eq!(
            pointees(result).copied().collect::<Vec<_>>(),
            [pag.var_location[&tag]],
            "{result}",
        );
    }
    for name in ["libc_allocation", "cast_allocation", "adl_allocation"] {
        let locations: Vec<_> = pointees(name).collect();
        assert_eq!(locations.len(), 1, "{name}");
        assert_eq!(
            pag.locations[locations[0].0 as usize].kind,
            trace_analysis::LocKind::Heap,
            "{name}",
        );
    }
}

#[test]
fn file_scope_allocator_returns_bind_formals_after_packs() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
extern "C" void* malloc(unsigned long);
namespace leading {
struct Tag {};
Tag leading_tag;
template<class... P> Tag* malloc(P... ignored, Tag* value) { return value; }
}
namespace prefixed {
struct Tag {};
Tag prefix_tag;
Tag tail_tag;
template<class... P>
Tag* malloc(Tag* prefix, P... ignored, Tag* value) { return value; }
}
void* leading_result = malloc(&leading::leading_tag);
void* prefixed_result = malloc(&prefixed::prefix_tag, &prefixed::tail_tag);
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for (dst, tag) in [
        ("leading_result", "leading_tag"),
        ("prefixed_result", "tail_tag"),
    ] {
        let var = only_variable(&program, dst);
        let expected = pag.var_location[&only_variable(&program, tag)];
        let pointees: Vec<_> = result
            .points_to
            .get(&pag.var_node[&var])
            .into_iter()
            .flatten()
            .copied()
            .collect();
        assert_eq!(pointees, [expected], "{dst}");
    }
}

#[test]
fn file_scope_allocator_formals_after_packs_get_addresses_and_only_omitted_defaults() {
    let dir = scratch(&[
        (
            "main.cpp",
            r#"
namespace reference {
struct Tag {};
Tag referent;
template<class... P> Tag* malloc(P... ignored, Tag& value);
}
namespace supplied_default {
struct Tag {};
Tag supplied, unused_fallback;
template<class... P>
Tag* malloc(P... ignored, Tag* value = &unused_fallback) { return value; }
}
namespace omitted_default {
struct Tag {};
Tag fallback;
template<class... P>
Tag* malloc(P... ignored, Tag* value = &fallback) { return value; }
}
void* address = reference::malloc(reference::referent);
void* supplied_result = supplied_default::malloc(&supplied_default::supplied);
void* omitted = omitted_default::malloc();
"#,
        ),
        (
            "definition.cpp",
            r#"
namespace reference {
struct Tag {};
template<class... P> Tag* malloc(P... ignored, Tag& value) { return &value; }
template Tag* malloc<>(Tag&);
}
"#,
        ),
    ]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for (dst, tag) in [
        ("address", "referent"),
        ("supplied_result", "supplied"),
        ("omitted", "fallback"),
    ] {
        let var = only_variable(&program, dst);
        let expected = pag.var_location[&only_variable(&program, tag)];
        let pointees: Vec<_> = result
            .points_to
            .get(&pag.var_node[&var])
            .into_iter()
            .flatten()
            .copied()
            .collect();
        assert_eq!(pointees, [expected], "{dst}");
    }
}

#[test]
fn file_scope_allocator_empty_packs_align_each_fixed_argument() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace ordered {
struct Tag {};
Tag a, b;
template<class... P>
Tag* malloc(P... ignored, Tag* first, Tag* second) { return first; }
}
namespace empty {
struct Tag {};
Tag supplied, unused_fallback;
template<class... P>
Tag* malloc(P... ignored, Tag* value = &unused_fallback) { return value; }
}
namespace explicit_pack {
struct Tag {};
Tag fallback;
template<class... P>
Tag* malloc(P... ignored, Tag* value = &fallback) { return value; }
}
namespace deduced_pack {
struct Tag {};
Tag deduced_fallback;
template<class... P> struct Types {};
Types<int> types;
template<class... P>
Tag* malloc(Types<P...>, P... ignored, Tag* value = &deduced_fallback) { return value; }
}
void* deduced_result = deduced_pack::malloc(deduced_pack::types, 42);
void* first_result = ordered::malloc(&ordered::a, &ordered::b);
void* supplied_result = empty::malloc<>(&empty::supplied);
void* explicit_result = explicit_pack::malloc<int>(42);
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for (dst, tag) in [
        ("first_result", "a"),
        ("supplied_result", "supplied"),
        ("explicit_result", "fallback"),
        ("deduced_result", "deduced_fallback"),
    ] {
        let var = only_variable(&program, dst);
        let expected = pag.var_location[&only_variable(&program, tag)];
        let pointees: Vec<_> = result
            .points_to
            .get(&pag.var_node[&var])
            .into_iter()
            .flatten()
            .copied()
            .collect();
        assert_eq!(pointees, [expected], "{dst}");
    }
}

#[test]
fn file_scope_allocator_template_id_excludes_non_templates_before_ranking() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace app {
struct Tag {};
Tag ordinary, expected;
Tag* malloc(int) { return &ordinary; }
template<class... P>
Tag* malloc(P... ignored, int value) { return &expected; }
}
void* empty_id = app::malloc<>(7);
void* explicit_id = app::malloc<int>(6, 7);
void* bare_name = app::malloc(7);
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for (dst, tag) in [
        ("empty_id", "expected"),
        ("explicit_id", "expected"),
        ("bare_name", "ordinary"),
    ] {
        let var = only_variable(&program, dst);
        let expected = pag.var_location[&only_variable(&program, tag)];
        let pointees: Vec<_> = result
            .points_to
            .get(&pag.var_node[&var])
            .into_iter()
            .flatten()
            .copied()
            .collect();
        assert_eq!(pointees, [expected], "{dst}");
    }
}

#[test]
fn body_allocator_template_id_excludes_the_ordinary_overload() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace app {
struct Tag {};
Tag wrong, expected;
Tag* malloc(int) { return &wrong; }
template<class... P>
Tag* malloc(P... ignored, int value) { return &expected; }
}
auto* use_template() {
    auto* p = app::malloc<>(7);
    return p;
}
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    let caller = only_function(&program, "use_template");
    let targets: Vec<_> = result
        .call_edges
        .iter()
        .filter(|edge| edge.caller == caller)
        .map(|edge| program.symbols.function(edge.callee))
        .collect();
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert!(targets[0].is_free_function_template, "{targets:?}");
    let var = local_variable(&program, "use_template", "p");
    let expected = pag.var_location[&only_variable(&program, "expected")];
    let pointees: Vec<_> = result
        .points_to
        .get(&pag.var_node[&var])
        .into_iter()
        .flatten()
        .copied()
        .collect();
    assert_eq!(pointees, [expected]);
}

#[test]
fn file_scope_allocator_template_id_retains_the_explicit_specialization() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace app {
struct Tag {};
Tag ordinary, primary, specialized;
Tag* malloc(int) { return &ordinary; }
template<class... P>
Tag* malloc(P... ignored, int value) { return &primary; }
template<> Tag* malloc(int value) { return &specialized; }
}
auto* empty_id = app::malloc<>(7);
auto* nonempty_id = app::malloc<int>(6, 7);
auto* bare_name = app::malloc(7);
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for (dst, tag) in [
        ("empty_id", "specialized"),
        ("nonempty_id", "primary"),
        ("bare_name", "ordinary"),
    ] {
        let var = only_variable(&program, dst);
        let expected = pag.var_location[&only_variable(&program, tag)];
        let pointees: Vec<_> = result
            .points_to
            .get(&pag.var_node[&var])
            .into_iter()
            .flatten()
            .copied()
            .collect();
        assert_eq!(pointees, [expected], "{dst}");
    }
}

#[test]
fn allocator_template_ids_respect_specializations_written_arguments() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace app {
struct Tag {};
Tag primary, int_result, short_result;
template<class U, class... P>
Tag* malloc(P... ignored, int value) { return &primary; }
template<> Tag* malloc<int>(int value);
template<> Tag* malloc<short>(int value) { return &short_result; }
}
template<> app::Tag* app::malloc<int>(int value) { return &int_result; }
auto* global_long = app::malloc<long>(7);
auto* global_int = app::malloc<signed int>(7);
auto* global_short = app::malloc<short>(7);
auto* local_long() {
    auto* p = app::malloc<long>(7);
    return p;
}
auto* local_alias() {
    using Selected = int;
    auto* p = app::malloc<Selected>(7);
    return p;
}
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for (var, tag) in [
        (only_variable(&program, "global_long"), "primary"),
        (only_variable(&program, "global_int"), "int_result"),
        (only_variable(&program, "global_short"), "short_result"),
        (local_variable(&program, "local_long", "p"), "primary"),
        (local_variable(&program, "local_alias", "p"), "int_result"),
    ] {
        let expected = pag.var_location[&only_variable(&program, tag)];
        let pointees: Vec<_> = result
            .points_to
            .get(&pag.var_node[&var])
            .into_iter()
            .flatten()
            .copied()
            .collect();
        assert_eq!(pointees, [expected], "expected {tag}");
    }
}

#[test]
fn allocator_template_ids_check_pack_length_and_integral_values() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace lengths {
struct Tag {};
Tag primary_length, empty_pack;
template<class U, class... P>
Tag* malloc(P... args, int value = 0) { return &primary_length; }
template<> Tag* malloc<int>(int value) { return &empty_pack; }
}
namespace packed {
struct Tag {};
Tag primary_packed, nonempty_pack;
template<class U, class... P>
Tag* malloc(P... args, int value) { return &primary_packed; }
template<> Tag* malloc<int, long>(long arg, int value) { return &nonempty_pack; }
}
namespace values {
struct Tag {};
Tag primary_value, one, three;
template<int N, class... P>
Tag* malloc(P... args, int value) { return &primary_value; }
template<> Tag* malloc<1>(int value) { return &one; }
template<> Tag* malloc<3>(int value) { return &three; }
}
auto* global_longer = lengths::malloc<int, long>(7);
auto* global_shorter = lengths::malloc<int>(7);
auto* global_packed = packed::malloc<int, long>(6, 7);
auto* global_other = values::malloc<2>(7);
auto* global_same = values::malloc<0x1>(7);
auto* global_three = values::malloc<3>(7);
auto* local_longer() {
    auto* p = lengths::malloc<int, long>(7);
    return p;
}
auto* local_other() {
    auto* p = values::malloc<2>(7);
    return p;
}
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for (var, tag) in [
        (only_variable(&program, "global_longer"), "primary_length"),
        (only_variable(&program, "global_shorter"), "empty_pack"),
        (only_variable(&program, "global_packed"), "nonempty_pack"),
        (only_variable(&program, "global_other"), "primary_value"),
        (only_variable(&program, "global_same"), "one"),
        (only_variable(&program, "global_three"), "three"),
        (
            local_variable(&program, "local_longer", "p"),
            "primary_length",
        ),
        (
            local_variable(&program, "local_other", "p"),
            "primary_value",
        ),
    ] {
        let expected = pag.var_location[&only_variable(&program, tag)];
        let pointees: Vec<_> = result
            .points_to
            .get(&pag.var_node[&var])
            .into_iter()
            .flatten()
            .copied()
            .collect();
        assert_eq!(pointees, [expected], "expected {tag}");
    }
}

#[test]
fn allocator_specializations_distinguish_deduced_integral_types() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace deduced {
struct Tag {};
Tag primary, unsigned_result;
template<auto N, class... P>
Tag* malloc(P... args, int value) { return &primary; }
template<> Tag* malloc<1u>(int value) { return &unsigned_result; }
}
namespace converted {
struct Tag {};
Tag converted_primary, converted_result;
template<int N, class... P>
Tag* malloc(P... args, int value) { return &converted_primary; }
template<> Tag* malloc<1u>(int value) { return &converted_result; }
}
auto* signed_call = deduced::malloc<1>(7);
auto* unsigned_call = deduced::malloc<0x1u>(7);
auto* converted_call = converted::malloc<1>(7);
auto* local_signed() {
    auto* p = deduced::malloc<1>(7);
    return p;
}
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for (var, tag) in [
        (only_variable(&program, "signed_call"), "primary"),
        (only_variable(&program, "unsigned_call"), "unsigned_result"),
        (
            only_variable(&program, "converted_call"),
            "converted_result",
        ),
        (local_variable(&program, "local_signed", "p"), "primary"),
    ] {
        let expected = pag.var_location[&only_variable(&program, tag)];
        let pointees: Vec<_> = result
            .points_to
            .get(&pag.var_node[&var])
            .into_iter()
            .flatten()
            .copied()
            .collect();
        assert_eq!(pointees, [expected], "expected {tag}");
    }
}

#[test]
fn uncertain_allocator_specialization_keeps_primary_return_flow() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace app {
struct Tag {};
Tag primary, specialized;
template<int N, class... P>
Tag* malloc(P... args, int value) { return &primary; }
template<> Tag* malloc<1>(int value) { return &specialized; }
}
constexpr int n = 2;
auto* global = app::malloc<n>(7);
auto* local() {
    auto* p = app::malloc<n>(7);
    return p;
}
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    let primary = pag.var_location[&only_variable(&program, "primary")];
    let specialization = pag.var_location[&only_variable(&program, "specialized")];
    for var in [
        only_variable(&program, "global"),
        local_variable(&program, "local", "p"),
    ] {
        let pointees = &result.points_to[&pag.var_node[&var]];
        assert_eq!(pointees.len(), 2, "{pointees:?}");
        assert!(pointees.contains(&primary), "{pointees:?}");
        assert!(pointees.contains(&specialization), "{pointees:?}");
    }
}

#[test]
fn allocator_specializations_without_template_ids_keep_the_primary() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace implicit_args {
struct Tag {};
Tag default_primary, wrong_default;
template<int N = 2, class... P>
Tag* malloc(P... args, int value) { return &default_primary; }
template<> Tag* malloc<1>(int value) { return &wrong_default; }
}
auto* global = implicit_args::malloc(7);
auto* local() {
    auto* p = implicit_args::malloc(7);
    return p;
}
"#,
    )]);
    assert_allocator_return_candidates(dir.path(), &["default_primary", "wrong_default"]);
}

#[test]
fn uncertain_allocator_specializations_are_ranked_independently() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace dependent {
struct Tag {};
Tag dependent_primary, wrong_int, selected_long;
template<auto N, class... P>
Tag* malloc(P... args, decltype(N) value) { return &dependent_primary; }
template<> Tag* malloc<1>(int value) { return &wrong_int; }
template<> Tag* malloc<2L>(long value) { return &selected_long; }
}
constexpr long n = 2;
auto* global = dependent::malloc<n>(7);
auto* local() {
    auto* p = dependent::malloc<n>(7);
    return p;
}
"#,
    )]);
    assert_allocator_return_candidates(
        dir.path(),
        &["dependent_primary", "wrong_int", "selected_long"],
    );
}

#[test]
fn allocator_template_ids_accept_abbreviated_templates() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
namespace app {
struct Tag {};
Tag expected, ordinary;
Tag* malloc(int) { return &ordinary; }
Tag* malloc(auto value);
}
auto* global = app::malloc<int>(7);
auto* local() {
    auto* p = app::malloc<int>(7);
    return p;
}
namespace app {
Tag* malloc(auto value) { return &expected; }
}
"#,
    )]);
    assert_allocator_return_candidates(dir.path(), &["expected"]);
}

#[test]
fn allocator_template_ids_accept_abbreviated_static_members() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
struct Tag {};
Tag expected, ordinary;
struct Alloc {
    static Tag* malloc(int) { return &ordinary; }
    static Tag* malloc(auto value) { return &expected; }
};
struct DeferredAlloc {
    static Tag* malloc(auto value);
};
Tag* global = Alloc::malloc<int>(7);
auto* local() {
    auto* p = DeferredAlloc::malloc<int>(7);
    return p;
}
Tag* DeferredAlloc::malloc(auto value) { return &expected; }
"#,
    )]);
    assert_allocator_return_candidates(dir.path(), &["expected"]);
}

#[test]
fn allocator_template_ids_select_static_member_specializations() {
    let dir = scratch(&[(
        "main.cpp",
        r#"
struct Tag {};
Tag primary, specialized;
struct Alloc {
    template<class... P>
    static Tag* malloc(P... args, int value) { return &primary; }
};
template<> Tag* Alloc::malloc(int value) { return &specialized; }
auto* empty_id = Alloc::malloc<>(7);
auto* nonempty_id = Alloc::malloc<long>(6L, 7);
auto* local() {
    auto* p = Alloc::malloc<>(7);
    return p;
}
"#,
    )]);
    let program = build_program(dir.path(), &default_opts(dir.path())).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for (var, tag) in [
        (only_variable(&program, "empty_id"), "specialized"),
        (only_variable(&program, "nonempty_id"), "primary"),
        (local_variable(&program, "local", "p"), "specialized"),
    ] {
        let expected = pag.var_location[&only_variable(&program, tag)];
        let pointees: Vec<_> = result
            .points_to
            .get(&pag.var_node[&var])
            .into_iter()
            .flatten()
            .copied()
            .collect();
        assert_eq!(pointees, [expected], "expected {tag}");
    }
}

fn assert_allocator_return_candidates(root: &std::path::Path, tags: &[&str]) {
    let program = build_program(root, &default_opts(root)).unwrap();
    let (pag, result) = analyze_with_options(
        &program,
        AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    for var in [
        only_variable(&program, "global"),
        local_variable(&program, "local", "p"),
    ] {
        let pointees = &result.points_to[&pag.var_node[&var]];
        assert_eq!(pointees.len(), tags.len(), "{pointees:?}");
        for tag in tags {
            let expected = pag.var_location[&only_variable(&program, tag)];
            assert!(pointees.contains(&expected), "missing {tag}: {pointees:?}");
        }
    }
}
