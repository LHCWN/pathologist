//! Arguments passed through a variadic template's parameter pack reach the
//! formals the pack is forwarded to (docs/POINTERS.md, "Variadic parameter
//! packs").

use crate::common;

use common::*;
use trace_analysis::{AnalysisResult, ResolutionKind};
use trace_ir::Program;

analyzed_fixture!(cpp_variadic_packs);

#[test]
fn every_declared_pack_receives_all_of_its_possible_arguments() {
    for (body, call, expected) in [
        (
            "(a(), ...);",
            "invoke<F, F>(alpha, beta, gamma);",
            &["alpha", "beta"][..],
        ),
        (
            "(b(), ...);",
            "invoke(alpha, beta);",
            &["alpha", "beta"][..],
        ),
        ("(a(), ...); (b(), ...);", "invoke();", &[][..]),
    ] {
        let source = format!(
            r#"
using F = void (*)();
void alpha() {{}}
void beta() {{}}
void gamma() {{}}
template<class... A, class... B> void invoke(A... a, B... b) {{ {body} }}
void caller() {{ {call} }}
"#
        );
        let (p, a) = analyze_source(&[("main.cpp", &source)]);
        assert!(p.diagnostics.is_empty(), "{call}: {:?}", p.diagnostics);
        let invoke = only_function(&p, "invoke");
        let caller = only_function(&p, "caller");
        assert!(p.symbols.function(invoke).is_defined);
        assert!(
            a.call_edges
                .iter()
                .any(|edge| edge.caller == caller && edge.callee == invoke),
            "{call}"
        );
        for target in expected {
            assert!(indirect(&p, &a, "invoke", target), "{call}: {target}");
        }
    }
}

#[test]
fn multiple_packs_preserve_fixed_parameters_and_member_signatures() {
    for (declaration, definition, call, owner) in [
        (
            "",
            "template<class... A, class... B> void invoke(F head, A... a, B... b, F tail) { head(); (a(), ...); tail(); }",
            "invoke<F, F>(head, alpha, beta, tail);",
            "invoke",
        ),
        (
            "struct C { template<class... A, class... B> void invoke(F head, A... a, B... b, F tail); };",
            "template<class... A, class... B> void C::invoke(F head, A... a, B... b, F tail) { head(); (a(), ...); tail(); }",
            "C c; c.invoke<F, F>(head, alpha, beta, tail);",
            "C::invoke",
        ),
    ] {
        let source = format!(
            r#"
using F = void (*)();
void head() {{}}
void alpha() {{}}
void beta() {{}}
void tail() {{}}
{declaration}
{definition}
void caller() {{ {call} }}
"#
        );
        let (p, a) = analyze_source(&[("main.cpp", &source)]);
        assert!(p.diagnostics.is_empty(), "{call}: {:?}", p.diagnostics);
        for target in ["head", "alpha", "beta", "tail"] {
            assert!(indirect(&p, &a, owner, target), "{call}: {target}");
        }
        let function = p.symbols.function(only_function(&p, owner));
        let params = p.symbols.explicit_params(function).unwrap();
        assert_eq!(params.len(), 2);
        for i in 0..params.len() {
            assert!(matches!(
                p.types.get(params.get(i).unwrap()).desc.innermost().0,
                trace_ir::TypeDesc::FnPtr { .. }
            ));
        }
        let this = u32::from(p.symbols.has_this_param(function.id));
        assert_eq!(arrivals(&p, &a, owner, "head"), [this]);
        assert!(!arrivals(&p, &a, owner, "tail").contains(&this));
    }
}

#[test]
fn a_fixed_callback_after_a_pack_receives_empty_and_nonempty_pack_arguments() {
    for call in [
        "run(target);",
        "run<int>(1, target);",
        "F value = target; run(value);",
        "F value = target; run<int>(1, value);",
    ] {
        let source = format!(
            r#"
using F = void (*)();
void target() {{}}
template<class... T> void run(T... xs, F cb) {{ cb(); }}
void caller() {{ {call} }}
"#
        );
        let (p, a) = analyze_source(&[("main.cpp", &source)]);
        assert!(p.diagnostics.is_empty(), "{call}: {:?}", p.diagnostics);
        assert!(indirect(&p, &a, "run", "target"), "{call}");
        assert!(has_any_edge(&p, &a, "caller", "run"), "{call}");
    }
}

#[test]
fn a_nontrailing_pack_survives_static_template_addresses_and_thread_models() {
    for call in [
        "void (*f)(int, F) = &C::run<int>; f(1, target);",
        "std::thread t(static_cast<void (*)(int, F)>(&C::run<int>), 1, target);",
    ] {
        let source = format!(
            r#"
namespace std {{ struct thread {{ template<class G, class... A> thread(G, A...); }}; }}
using F = void (*)();
void target() {{}}
struct C {{ template<class... T> static void run(T... xs, F cb) {{ cb(); }} }};
void caller() {{ {call} }}
"#
        );
        let (p, a) = analyze_source(&[("main.cpp", &source)]);
        assert!(p.diagnostics.is_empty(), "{call}: {:?}", p.diagnostics);
        assert!(indirect(&p, &a, "C::run", "target"), "{call}");
        assert!(has_any_edge(&p, &a, "caller", "C::run"), "{call}");
    }
}

#[test]
fn fixed_parameters_on_both_sides_of_a_pack_keep_their_bindings() {
    for call in [
        "run(first, second, third);",
        "run<int>(first, 1, second, third);",
    ] {
        let source = format!(
            r#"
using F = void (*)();
void first() {{}}
void second() {{}}
void third() {{}}
template<class... T> void run(F head, T... xs, F cb, F tail) {{
    head(); cb(); tail();
}}
void caller() {{ {call} }}
"#
        );
        let (p, a) = analyze_source(&[("main.cpp", &source)]);
        assert!(p.diagnostics.is_empty(), "{call}: {:?}", p.diagnostics);
        for target in ["first", "second", "third"] {
            assert!(indirect(&p, &a, "run", target), "{call}: {target}");
        }
        assert_eq!(arrivals(&p, &a, "run", "head"), [0]);
        assert!(!arrivals(&p, &a, "run", "cb").contains(&0));
        assert!(!arrivals(&p, &a, "run", "tail").contains(&0));
    }
}

#[test]
fn a_nontrailing_member_pack_preserves_declaration_signature_and_receiver() {
    for (call, position) in [
        ("c->run(target);", 1),
        (
            "void (C::*m)(int, F) = &C::run<int>; (c->*m)(1, target);",
            2,
        ),
    ] {
        let source = format!(
            r#"
using F = void (*)();
void target() {{}}
struct C {{ template<class... T> void run(T... xs, F cb); }};
template<class... T> void C::run(T... xs, F cb) {{ cb(); }}
void caller(C *c) {{ {call} }}
"#
        );
        let (p, a) = analyze_source(&[("main.cpp", &source)]);
        assert!(p.diagnostics.is_empty(), "{call}: {:?}", p.diagnostics);
        assert!(indirect(&p, &a, "C::run", "target"), "{call}");
        let run = p.symbols.function(only_function(&p, "C::run"));
        let params = p.symbols.explicit_params(run).unwrap();
        assert_eq!(params.len(), 1);
        assert!(matches!(
            p.types.get(params.get(0).unwrap()).desc.innermost().0,
            trace_ir::TypeDesc::FnPtr { .. }
        ));
        assert_eq!(arrivals(&p, &a, "C::run", "cb"), [position]);
    }
}

#[test]
fn pack_positions_keep_member_template_overloads_distinct() {
    let (p, _) = analyze_source(&[(
        "main.cpp",
        r#"
using F = void (*)();
struct C {
    template<class... T> void run(T... xs, F cb);
    template<class... T> void run(F cb, T... xs);
};
"#,
    )]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert_eq!(p.symbols.functions_named("C::run").len(), 2);
}

fn indirect(program: &Program, analysis: &AnalysisResult, caller: &str, callee: &str) -> bool {
    has_edge(program, analysis, caller, callee, ResolutionKind::Indirect)
}

/// The argument positions the arg-flow rows into `function`'s parameter
/// `name` came from.
fn arrivals(program: &Program, analysis: &AnalysisResult, function: &str, name: &str) -> Vec<u32> {
    let formal = local_variable(program, function, name);
    let mut positions: Vec<u32> = analysis
        .arg_flow_edges
        .iter()
        .filter(|row| row.formal == formal)
        .map(|row| row.arg_index)
        .collect();
    positions.sort_unstable();
    positions.dedup();
    positions
}

#[test]
fn a_pack_forwarded_with_std_forward_reaches_the_callee_formal() {
    let (p, a) = cpp_variadic_packs();
    assert!(
        indirect(p, a, "Target", "HitForwarded"),
        "`HitForwarded` goes into `Forward`'s pack and out to `Target`'s `cb`"
    );
}

#[test]
fn a_pack_expanded_bare_reaches_the_callee_formal() {
    let (p, a) = cpp_variadic_packs();
    assert!(indirect(p, a, "TargetPlain", "HitPlain"));
}

#[test]
fn a_pack_after_a_fixed_parameter_reaches_an_indirect_callee() {
    let (p, a) = cpp_variadic_packs();
    assert!(indirect(p, a, "Call", "TargetAfter"));
    assert!(
        indirect(p, a, "TargetAfter", "HitAfter"),
        "the pack's argument is passed on by `f(std::forward<Args>(args)...)`"
    );
}

#[test]
fn a_pack_handed_to_a_thread_model_reaches_the_threads_callable() {
    let (p, a) = cpp_variadic_packs();
    assert!(indirect(p, a, "Spawn", "ThreadBody"));
    assert!(
        indirect(p, a, "ThreadBody", "HitThread"),
        "`std::thread(f, args...)` runs `f` with the pack's arguments"
    );
}

#[test]
fn an_expansion_covers_the_positions_from_its_own_on() {
    let (p, a) = cpp_variadic_packs();
    assert!(indirect(p, a, "Pick", "HitRest"));
    assert_eq!(
        arrivals(p, a, "Pick", "first"),
        vec![0],
        "`first` is the fixed argument's; the expansion starts at position 1"
    );
    assert_eq!(
        arrivals(p, a, "Pick", "second"),
        vec![1],
        "`second` takes the expansion"
    );
}

#[test]
fn a_pack_formal_receives_every_argument_from_its_position_on() {
    let (p, a) = cpp_variadic_packs();
    assert_eq!(
        arrivals(p, a, "Spawn", "args"),
        vec![1],
        "`HitThread` is the argument after the callable"
    );
    assert_eq!(arrivals(p, a, "Spawn", "f"), vec![0]);
}

#[test]
fn a_pack_takes_every_argument_passed_up_to_the_last() {
    let (p, a) = cpp_variadic_packs();
    assert_eq!(
        arrivals(p, a, "Collect", "all"),
        vec![0, 1, 2],
        "three functions are passed: one row for each position"
    );
    assert_eq!(arrivals(p, a, "Gather", "head"), vec![0]);
    assert_eq!(
        arrivals(p, a, "Gather", "tail"),
        vec![1, 2],
        "the pack follows a fixed parameter: positions 1 and 2, not 0"
    );
}

#[test]
fn a_formal_after_the_expansions_position_takes_the_expansion() {
    let (p, a) = cpp_variadic_packs();
    assert_eq!(
        arrivals(p, a, "ForwardTwo", "args"),
        vec![0, 1],
        "both functions reach the pack"
    );
    assert!(indirect(p, a, "TargetTwo", "HitOne"));
    assert!(indirect(p, a, "TargetTwo", "HitTwo"));
    assert_eq!(arrivals(p, a, "TargetTwo", "a"), vec![0]);
    assert_eq!(
        arrivals(p, a, "TargetTwo", "b"),
        vec![0],
        "`b` is at position 1, where nothing is written: it takes the expansion, \
         recorded at position 0"
    );
}

#[test]
fn an_empty_expansion_still_reaches_a_callable_that_takes_nothing() {
    let (p, a) = cpp_variadic_packs();
    assert!(
        indirect(p, a, "Pool::Run", "Idle"),
        "`pool.Run(Idle)` passes no argument for `args...`; `Idle` takes none"
    );
}

#[test]
fn a_call_passing_nothing_for_the_pack_still_reaches_the_function() {
    let (p, a) = cpp_variadic_packs();
    assert!(
        has_edge(p, a, "CallEmpty", "Emit", ResolutionKind::Direct),
        "the pack is no declared parameter: `Emit()` fits `Emit(Args &&...args)`"
    );
    assert!(
        has_edge(p, a, "MakeEmpty", "Holder::Holder", ResolutionKind::Direct),
        "and so does `Holder holder;` the pack constructor"
    );
}

#[test]
fn a_member_template_with_a_pack_merges_its_declaration_and_definition() {
    let (p, _) = cpp_variadic_packs();
    let add = p.symbols.function(only_function(p, "Widget::Add"));
    // The signature a definition records is by parameter, the pack's own last:
    // an offset that counted the pack as declared would rewrite the wrong one.
    if !add.param_type_ids.is_empty() {
        assert_eq!(add.param_type_ids.len(), add.params.len());
        for (&declared, &formal) in add.param_type_ids.iter().zip(&add.params) {
            let held = p.symbols.variable(formal).type_id;
            let unknown = matches!(
                p.types.get(declared).desc.as_ref(),
                trace_ir::TypeDesc::Unknown
            );
            assert!(
                declared == held || unknown,
                "{}: recorded {:?}, held {:?}",
                p.symbols.variable(formal).name,
                p.types.get(declared).desc,
                p.types.get(held).desc
            );
        }
        let pack = *add.params.last().unwrap();
        assert_eq!(
            *add.param_type_ids.last().unwrap(),
            p.symbols.variable(pack).type_id,
            "the pack's own type is never a stand-in"
        );
    }
}

#[test]
fn an_argument_after_an_expansion_still_reaches_the_callee() {
    let (p, a) = cpp_variadic_packs();
    assert!(
        indirect(p, a, "TargetTrail", "HitTrail"),
        "`HitTrail` follows `std::forward<Args>(args)...`: at no known position, it may be any formal's"
    );
    assert!(indirect(p, a, "TargetTrail", "HitLead"));
}

#[test]
fn a_pack_nothing_forwards_carries_its_argument_nowhere() {
    let (p, a) = cpp_variadic_packs();
    assert!(must_not_have_edge(p, a, "Loose", "Unrelated"));
    assert!(must_not_have_edge(p, a, "Drop", "Unrelated"));
}
