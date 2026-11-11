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

/// A parameter pack is followed (docs/ANALYSIS.md, "Variadic parameter
/// packs"): `Hit` goes into `Submit`'s `Args&&... args`, out through
/// `std::forward<Args>(args)...` into the bind, and so to `Work`'s `cb`.
#[test]
fn a_parameter_pack_carries_a_bound_argument_to_the_callable() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(indirect(p, a, "Submit", "Work"));
    assert!(
        must_not_have_edge(p, a, "Submit", "Hit"),
        "`Hit` is only an argument; `Submit` never calls it"
    );
    assert!(
        indirect(p, a, "Work", "Hit"),
        "`Hit` is passed to `Submit` through `Args&&... args` and bound to `Work`'s `cb`"
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

/// Whether `caller` has an indirect edge to the lambda it contains.
fn reaches_its_lambda(program: &Program, analysis: &AnalysisResult, caller: &str) -> bool {
    let lambda = lambda_in(program, caller).name.clone();
    indirect(program, analysis, caller, &lambda)
}

#[test]
fn a_make_shared_packaged_task_built_from_a_named_function_reaches_it() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(
        indirect(p, a, "MakeSharedFromNamed", "SharedNamed"),
        "the factory's constructor site passes `SharedNamed` to the `invoke` model"
    );
}

#[test]
fn a_make_shared_packaged_task_built_from_a_lambda_reaches_it() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(reaches_its_lambda(p, a, "MakeSharedFromLambda"));
}

#[test]
fn a_make_unique_packaged_task_built_from_a_named_function_reaches_it() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(indirect(p, a, "MakeUniqueFromNamed", "UniqueNamed"));
}

#[test]
fn a_make_unique_packaged_task_built_from_a_lambda_reaches_it() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(reaches_its_lambda(p, a, "MakeUniqueFromLambda"));
}

#[test]
fn a_globally_qualified_standard_class_is_the_same_class() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(indirect(p, a, "MakeSharedGlobal", "GlobalQualified"));
}

#[test]
fn a_standard_type_that_is_no_class_gets_no_constructor_site() {
    let (p, a) = cpp_bind_packaged_task();
    for caller in [
        "MakeCounter",
        "MakeBuffer",
        "MakePointer",
        "MakeSpacedPointer",
        "MakeNothing",
    ] {
        let constructors: Vec<_> = callees_of(p, a, caller)
            .into_iter()
            .map(|(callee, _)| callee)
            .filter(|callee| {
                [
                    "std::size_t",
                    "std::uint8_t",
                    "std::thread",
                    "std::nullptr_t",
                ]
                .iter()
                .any(|name| callee.starts_with(name))
            })
            .collect();
        assert!(
            constructors.is_empty(),
            "`{caller}` builds a number, not a class: {constructors:?}"
        );
    }
}

#[test]
fn a_task_naming_a_template_parameter_still_reaches_its_callable() {
    let (p, a) = cpp_bind_packaged_task();
    assert!(
        indirect(p, a, "MakeTyped", "TypedTarget"),
        "`std::packaged_task<R()>` is a standard class whatever `R` is, as for `new`"
    );
}
