use crate::common;
use common::*;
use trace_analysis::{AnalysisResult, ContextKind, ResolutionKind};
use trace_ir::{CallSiteId, Program};

analyzed_fixture!(std_bind);

fn indirect_at(p: &Program, a: &AnalysisResult, caller: &str, spelling: &str) -> Vec<String> {
    a.call_edges
        .iter()
        .filter(|edge| {
            edge.resolution == ResolutionKind::Indirect
                && p.symbols.function(edge.caller).name == caller
                && p.symbols
                    .call_site_by_id(edge.call_site)
                    .unwrap()
                    .callee_name
                    .as_str()
                    == spelling
        })
        .map(|edge| p.symbols.function(edge.callee).name.clone())
        .collect()
}

fn site(p: &Program, caller: &str, spelling: &str) -> CallSiteId {
    p.symbols
        .call_sites
        .iter()
        .find(|c| p.symbols.function(c.caller).name == caller && c.callee_name.as_str() == spelling)
        .unwrap_or_else(|| panic!("missing {caller} -> {spelling}"))
        .id
}

fn check_args(
    p: &Program,
    a: &AnalysisResult,
    caller: &str,
    spelling: &str,
    target: &str,
    expected: &[(&str, &str, u32)],
) {
    let site = site(p, caller, spelling);
    let mut actual: Vec<_> = a
        .arg_flow_edges
        .iter()
        .filter(|row| row.call_site == site)
        .filter_map(|row| {
            let formal = p.symbols.variable(row.formal);
            if p.symbols.function(formal.fn_id?).name != target {
                return None;
            }
            let actual = if let Some(var) = row.actual_var {
                &p.symbols.variable(var).name
            } else {
                &p.symbols.function(row.actual_fn?).name
            };
            Some((formal.name.as_str(), actual.as_str(), row.arg_index))
        })
        .collect();
    actual.sort_unstable();
    assert_eq!(actual, expected, "{caller} -> {target}");
}

#[test]
fn exact_targets_and_receivers_at_invocation() {
    let (p, a) = std_bind();
    for (caller, target) in [
        ("control", "alpha"),
        ("bound", "alpha"),
        ("member", "Worker::run"),
        ("member_placeholder", "Worker::run"),
    ] {
        assert_eq!(indirect_at(p, a, caller, "f"), [target]);
    }
    assert!(has_edge(
        p,
        a,
        "bound",
        "std::bind",
        ResolutionKind::External
    ));
    assert_eq!(indirect_at(p, a, "via_directive", "copy"), ["alpha"]);
    assert!(has_edge(
        p,
        a,
        "immediate",
        "alpha",
        ResolutionKind::Indirect
    ));
    assert!(indirect_at(p, a, "immediate", "std::bind").is_empty());
    assert!(indirect_at(p, a, "bound", "std::bind").is_empty());
    assert!(indirect_at(p, a, "dropped", "std::bind").is_empty());
    assert!(a
        .execution_contexts
        .iter()
        .all(|ctx| ctx.start.is_none_or(|start| {
            let call = p.symbols.call_site_by_id(start.call_site).unwrap();
            !call.callee_name.as_str().contains("bind")
        })));
    check_args(p, a, "member", "f", "Worker::run", &[("this", "w", 0)]);
    check_args(
        p,
        a,
        "member_placeholder",
        "f",
        "Worker::run",
        &[("this", "later", 0)],
    );
}

#[test]
fn named_captures_and_reordered_repeated_placeholders() {
    let (p, a) = std_bind();
    assert_eq!(indirect_at(p, a, "order", "f"), ["Reorder"]);
    check_args(
        p,
        a,
        "order",
        "f",
        "Reorder",
        &[
            ("first", "Right", 1),
            ("second", "Left", 0),
            ("stored", "Captured", 3),
            ("third", "Right", 1),
        ],
    );
    for (arg, target) in [
        ("first", "Right"),
        ("second", "Left"),
        ("third", "Right"),
        ("stored", "Captured"),
    ] {
        assert_eq!(indirect_at(p, a, "Reorder", arg), [target]);
    }
}

#[test]
fn return_copy_parameter_and_field_flow_cross_translation_units() {
    let (p, a) = std_bind();
    assert_eq!(indirect_at(p, a, "returned", "f"), ["Echo"]);
    assert_eq!(indirect_at(p, a, "returned", "result"), ["Captured"]);
    assert_eq!(indirect_at(p, a, "Queue::Fire", "this->task"), ["Dispatch"]);
    check_args(
        p,
        a,
        "Queue::Fire",
        "this->task",
        "Dispatch",
        &[("callback", "captured", 0)],
    );
    assert_eq!(indirect_at(p, a, "Dispatch", "callback"), ["CrossTarget"]);
}

#[test]
fn two_bindings_of_the_same_function_both_wire_at_one_invocation() {
    let (p, a) = std_bind();
    assert_eq!(indirect_at(p, a, "alternatives", "f"), ["Dispatch"]);
    check_args(
        p,
        a,
        "alternatives",
        "f",
        "Dispatch",
        &[("callback", "first", 0), ("callback", "second", 0)],
    );
}

#[test]
fn unrelated_and_opaque_targets_remain_unresolved() {
    let (p, a) = std_bind();
    assert!(indirect_at(p, a, "foreign_bind", "f").is_empty());
    assert!(has_edge(
        p,
        a,
        "via_adl",
        "custom::Job::operator()",
        ResolutionKind::Direct,
    ));
    assert!(must_not_have_edge(p, a, "via_adl", "AdlTarget"));
    assert!(indirect_at(p, a, "opaque", "f").is_empty());
    assert!(indirect_at(p, a, "opaque", "std::bind").is_empty());
    assert_eq!(indirect_at(p, a, "maybe_opaque", "f"), ["alpha"]);
}

#[test]
fn modeled_thread_and_timer_invoke_bindings_at_the_submission_site() {
    let (p, _) = std_bind();
    let a = analyze_with_models(
        p,
        r#"
[[model]]
name = "AddTimerEvent"
effects = [{ kind = "invoke", param = 0, context = "serial_task" }]
"#,
    );
    let thread = site(p, "Launch", "std::thread");
    let timer = site(p, "Plugin::DelayProcessEvent", "AddTimerEvent");
    for (start_site, entry, kind) in [
        (thread, "StartWorker", ContextKind::Thread),
        (timer, "Plugin::OnEventProxy", ContextKind::SerialTask),
    ] {
        assert!(
            a.execution_contexts.iter().any(|ctx| {
                ctx.start.is_some_and(|start| start.call_site == start_site)
                    && p.symbols.function(ctx.entry).name == entry
                    && ctx.kind == kind
            }),
            "{entry}: {:?}",
            a.execution_contexts
        );
    }
    check_args(
        p,
        &a,
        "Launch",
        "std::thread",
        "StartWorker",
        &[("left", "right", 2), ("right", "left", 1)],
    );
    check_args(
        p,
        &a,
        "Plugin::DelayProcessEvent",
        "AddTimerEvent",
        "Plugin::OnEventProxy",
        &[("event", "event", 1), ("this", "this", 0)],
    );
    assert_eq!(
        indirect_at(p, &a, "Plugin::OnEventProxy", "event"),
        ["Captured"]
    );
    assert_eq!(indirect_at(p, &a, "StartWorker", "left"), ["Right"]);
    assert_eq!(indirect_at(p, &a, "StartWorker", "right"), ["Left"]);
}

#[test]
fn callable_casts_keep_the_selected_overload_and_leave_runtime_targets_opaque() {
    let (p, a) = std_bind();
    assert_eq!(indirect_at(p, a, "member_cast", "f"), ["CastWorker::run"]);
    check_args(
        p,
        a,
        "member_cast",
        "f",
        "CastWorker::run",
        &[("cb", "ReviewHit", 1), ("this", "worker", 0)],
    );
    assert!(indirect_at(p, a, "member_variable_cast", "f").is_empty());
    assert_eq!(indirect_at(p, a, "free_cast", "f"), ["FreeCast"]);
    check_args(
        p,
        a,
        "free_cast",
        "f",
        "FreeCast",
        &[("cb", "ReviewHit", 0)],
    );
}

#[test]
fn a_braced_callable_slot_keeps_the_bind_result() {
    let (p, a) = std_bind();
    assert_eq!(indirect_at(p, a, "brace_bind", "f"), ["BraceTarget"]);
    assert!(indirect_at(p, a, "brace_bind", "std::bind").is_empty());
}

#[test]
fn global_placeholders_work_without_a_standard_declaration() {
    let (p, a) = std_bind();
    check_args(
        p,
        a,
        "global_placeholder",
        "f",
        "ReviewTarget",
        &[("cb", "ReviewHit", 0)],
    );
    assert_eq!(indirect_at(p, a, "ReviewTarget", "cb"), ["ReviewHit"]);
}

#[test]
fn a_model_on_the_bound_target_reads_composed_arguments() {
    let (p, _) = std_bind();
    let a = analyze_with_models(
        p,
        r#"
[[model]]
name = "ModeledLaunch"
effects = [{ kind = "invoke", param = 0, args = [1], context = "thread" }]
"#,
    );
    for (caller, position) in [("mode_with_capture", 1), ("mode_with_placeholders", 0)] {
        let mut targets = indirect_at(p, &a, caller, "job");
        targets.sort_unstable();
        assert_eq!(targets, ["ModelActual", "ModeledLaunch"]);
        check_args(
            p,
            &a,
            caller,
            "job",
            "ModelActual",
            &[("next", "ReviewHit", position)],
        );
        let start = site(p, caller, "job");
        let contexts: Vec<_> = a
            .execution_contexts
            .iter()
            .filter(|ctx| ctx.start.is_some_and(|loc| loc.call_site == start))
            .map(|ctx| (p.symbols.function(ctx.entry).name.as_str(), ctx.kind))
            .collect();
        assert_eq!(contexts, [("ModelActual", ContextKind::Thread)]);
    }
    assert_eq!(indirect_at(p, &a, "ModelActual", "next"), ["ReviewHit"]);
}

#[test]
fn jthread_reserves_the_first_placeholder_for_its_stop_token() {
    let (p, a) = std_bind();
    assert_eq!(
        indirect_at(p, a, "with_stop_token", "std::jthread"),
        ["StopWork"]
    );
    check_args(
        p,
        a,
        "with_stop_token",
        "std::jthread",
        "StopWork",
        &[("cb", "ReviewHit", 1)],
    );
    assert_eq!(indirect_at(p, a, "StopWork", "cb"), ["ReviewHit"]);

    let root = fixture("std_bind");
    let mut program = trace_parse::build_program(&root, &default_opts(&root)).expect("build");
    let token = local_variable(&program, "StopWork", "token");
    program.symbols.variable_mut(token).type_id = program.types.unknown();
    let (_, analysis) = trace_analysis::analyze(&program);
    check_args(
        &program,
        &analysis,
        "with_stop_token",
        "std::jthread",
        "StopWork",
        &[("cb", "ReviewHit", 1), ("token", "ReviewHit", 1)],
    );
}

#[test]
fn equal_captured_and_placeholder_values_export_once() {
    let (p, a) = std_bind();
    check_args(
        p,
        a,
        "duplicate_flow",
        "f",
        "ReviewTarget",
        &[("cb", "x", 0)],
    );
    let db = cli_analyze(&fixture("std_bind"), &[]);
    let rows: i64 = trace_db::open_db(&db)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM flow_calls f JOIN call_sites s ON s.id = f.call_site_id
         JOIN functions c ON c.id = s.caller_fn_id
         WHERE c.name = 'duplicate_flow' AND s.callee_text = 'f'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn explored_bindings_keep_provenance_for_each_target() {
    let dir = scratch(&[
        ("BUILD.gn", "defines = [ \"CHOOSE_FIRST\" ]\n"),
        (
            "main.cpp",
            r#"
void First() {}
void Second() {}
#ifdef CHOOSE_FIRST
#define CALLBACK First
#else
#define CALLBACK Second
#endif
void run() {
    auto f = std::bind(CALLBACK);
    f();
}
"#,
        ),
    ]);
    let db = cli_analyze(dir.path(), &["--explore"]);
    let conn = trace_db::open_db(&db).unwrap();
    let mut query = conn
        .prepare(
            "SELECT n.label, o.dst_node, o.line FROM flow_origins o
         JOIN flow_nodes n ON n.id = o.src_node
         WHERE o.kind = 'addr_of' AND n.detail = 'bound'
         ORDER BY n.label",
        )
        .unwrap();
    let rows: Vec<(String, i64, i64)> = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!((&*rows[0].0, &*rows[1].0), ("bind:First", "bind:Second"));
    assert_eq!((rows[0].1, rows[0].2), (rows[1].1, rows[1].2));
}
