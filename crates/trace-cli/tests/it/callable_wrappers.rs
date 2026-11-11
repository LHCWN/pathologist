//! Callables wrapped by `std::bind` and `std::packaged_task` keep the call
//! edge from the code that wraps them (docs/ANALYSIS.md, "Callable
//! wrappers").

use crate::common;

use common::*;
use trace_analysis::{AnalysisResult, ResolutionKind};
use trace_ir::Program;

analyzed_fixture!(cpp_bind_packaged_task);

fn indirect(program: &Program, analysis: &AnalysisResult, caller: &str, callee: &str) -> bool {
    has_edge(program, analysis, caller, callee, ResolutionKind::Indirect)
}

/// Whether `Worker::Step`'s `this` receives the variable `actual` of `caller`.
fn receives(program: &Program, analysis: &AnalysisResult, caller: &str, actual: &str) -> bool {
    let this = local_variable(program, "Worker::Step", "this");
    let actual = local_variable(program, caller, actual);
    analysis
        .arg_flow_edges
        .iter()
        .any(|row| row.formal == this && row.actual_var == Some(actual))
}

#[test]
fn submit_reaches_the_callable_it_binds() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(
        indirect(p, a, "Submit", "Work"),
        "`Work` is only named inside `std::bind(std::forward<F>(f), ...)`"
    );
}

/// Known imprecision (docs/ANALYSIS.md, "Callable wrappers"): a parameter
/// pack is not a formal, so `Hit` stops at `Submit`'s `Args&&... args`. When
/// packs are followed, this test and that section change together.
#[test]
fn a_parameter_pack_does_not_carry_a_bound_argument_to_the_callable() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(indirect(p, a, "Submit", "Work"));
    assert!(
        must_not_have_edge(p, a, "Submit", "Hit"),
        "`Hit` is only an argument; `Submit` never calls it"
    );
    assert!(
        must_not_have_edge(p, a, "Work", "Hit"),
        "`Hit` is passed to `Submit` through `Args&&... args`, which reaches no formal"
    );
}

#[test]
fn a_pool_forwards_a_named_bound_argument_to_the_callable() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(indirect(p, a, "SubmitFixed", "FixedWork"));
    assert!(
        indirect(p, a, "FixedWork", "FixedHit"),
        "`FixedHit` is the bound argument that reaches `FixedWork`'s `cb`"
    );
}

#[test]
fn a_plain_bind_reaches_the_callable_and_forwards_its_arguments() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(indirect(p, a, "Bind", "BoundWork"));
    assert!(indirect(p, a, "BoundWork", "BoundHit"));
}

#[test]
fn a_packaged_task_built_from_a_callable_reaches_it() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(indirect(p, a, "RunDirect", "Direct"));
}

#[test]
fn a_packaged_task_built_from_a_callable_with_parameters_reaches_it() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(
        indirect(p, a, "RunTyped", "Typed"),
        "`std::packaged_task<int(int, double)>` stores a callable that takes arguments"
    );
}

#[test]
fn a_bind_of_a_member_function_reaches_it_from_the_binding_member() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(indirect(p, a, "Worker::Start", "Worker::Step"));
    assert!(
        receives(p, a, "Worker::Start", "this"),
        "the bound `this` reaches `Worker::Step`'s `this`"
    );
}

#[test]
fn a_bind_of_a_member_function_on_a_named_receiver_reaches_it() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(indirect(p, a, "StartOn", "Worker::Step"));
    assert!(
        receives(p, a, "StartOn", "worker"),
        "the bound `&worker` reaches `Worker::Step`'s `this`"
    );
}

#[test]
fn a_std_function_needs_no_model() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(indirect(p, a, "CallHeld", "Held"));
}

#[test]
fn a_bound_argument_is_not_called_from_the_bind_site() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(
        must_not_have_edge(p, a, "Bind", "BoundHit"),
        "`BoundHit` is only an argument; `BoundWork` is what calls it"
    );
}

#[test]
fn a_bind_or_a_packaged_task_starts_no_execution_context() {
    let (_, a) = cpp_bind_packaged_task();
    assert!(
        a.execution_contexts.is_empty(),
        "nothing in the fixture starts a thread or a task, only wraps a callable: {:?}",
        a.execution_contexts
    );
}

const WRAP: &str = "\
void Target() {}
void Wrap(void (*f)());
void Caller() { Wrap(Target); }
";

fn wrap_with(context: &str) -> (Program, AnalysisResult) {
    let dir = scratch(&[("wrap.cpp", WRAP)]);
    let program = trace_parse::build_program(dir.path(), &default_opts(dir.path())).expect("build");
    let analysis = analyze_with_models(
        &program,
        &format!(
            "[[model]]\nname = \"Wrap\"\neffects = [{{ kind = \"invoke\", param = 0, context = \"{context}\" }}]\n"
        ),
    );
    (program, analysis)
}

#[test]
fn a_model_stating_no_context_keeps_the_edge_and_starts_nothing() {
    let (p, a) = wrap_with("none");
    assert!(
        indirect(&p, &a, "Caller", "Target"),
        "the wrapped callable stays reachable from the wrapping site"
    );
    assert!(
        a.execution_contexts.is_empty(),
        "{:?}",
        a.execution_contexts
    );
}

#[test]
fn a_model_stating_a_context_still_starts_it() {
    let (p, a) = wrap_with("thread");
    assert!(indirect(&p, &a, "Caller", "Target"));
    assert_eq!(
        a.execution_contexts.len(),
        1,
        "the control: the same model with a context"
    );
}
