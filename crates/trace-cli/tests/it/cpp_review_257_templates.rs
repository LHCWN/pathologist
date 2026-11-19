use super::cpp_review_257::verify;

#[test]
fn nested_callable_signatures_preserve_dependent_member_owners() {
    for outer in ["", "struct T {};"] {
        verify(
            &format!(
                r#"
{outer}
struct C {{}};
template<class T> struct Host {{
    static void consume(void (*cb)(void (T::*)())) {{}}
    static void deep(void (*cb)(void (*)(void (T::*)()))) {{}}
    using Callback = void (*)(void (T::*)());
    static Callback produce() {{ return nullptr; }}
}};
using Member = void (C::*)();
using Callback = void (*)(Member);
void caller() {{
    void (*p)(void (*)(void (C::*)())) = &Host<C>::consume;
    p(nullptr);
}}
void deep_call() {{
    void (*p)(void (*)(Callback)) = &Host<C>::deep;
    p(nullptr);
}}
void return_call() {{ Callback (*p)() = &Host<C>::produce; p(); }}
"#
            ),
            &[
                ("caller", "Host::consume"),
                ("deep_call", "Host::deep"),
                ("return_call", "Host::produce"),
            ],
        );
    }
}

#[test]
fn template_callback_parameters_shadow_outer_aliases() {
    for (parameter, outer, member_alias) in [
        ("T", "using T = int;", ""),
        ("T", "typedef int T;", ""),
        ("T", "using T = double;", ""),
        ("T", "", "using T = int;"),
        ("size_t", "using size_t = int;", ""),
        ("size_t", "", ""),
    ] {
        verify(
            &format!(
                r#"
{outer}
double worker(int) {{ return 1.0; }}
struct A {{
    {member_alias}
    template<class {parameter}> static void f({parameter}) {{}}
    template<class {parameter}> static void pointer({parameter} *) {{}}
    template<class {parameter}> static void reference({parameter} &) {{}}
    template<class {parameter}> static {parameter} value_return({parameter} v) {{ return v; }}
    template<class {parameter}> void member({parameter}) {{}}
    template<class {parameter}> static void callback(void (*cb)({parameter})) {{}}
    template<class {parameter}> static void callable_return() {{
        using F = {parameter} (*)(int); F p = &worker; p(1);
    }}
}};
void scalar_call() {{ void (*p)(double) = &A::f<double>; p(1.0); }}
void pointer_call() {{ void (*p)(double *) = &A::pointer<double>; double v; p(&v); }}
void reference_call() {{ void (*p)(double &) = &A::reference<double>; double v; p(v); }}
void return_call() {{ double (*p)(double) = &A::value_return<double>; p(1.0); }}
void member_call(A *a) {{ void (A::*p)(double) = &A::member<double>; (a->*p)(1.0); }}
void callback_call() {{ void (*p)(void (*)(double)) = &A::callback<double>; void (*cb)(double) = &A::f<double>; p(cb); }}
void callable_return_call() {{ A::callable_return<double>(); }}
"#
            ),
            &[
                ("scalar_call", "A::f"),
                ("pointer_call", "A::pointer"),
                ("reference_call", "A::reference"),
                ("return_call", "A::value_return"),
                ("member_call", "A::member"),
                ("callback_call", "A::callback"),
                ("A::callable_return", "worker"),
            ],
        );
    }
}

#[test]
fn qualified_aliases_in_templates_keep_their_concrete_signature() {
    let (program, analysis) = verify(
        r#"
namespace n { using T = int; }
using T = int;
struct A {
    template<class T> static void fixed(n::T) {}
    static void real(T) {}
    static void real(double) {}
};
void fixed_call() { void (*p)(int) = &A::fixed<double>; p(1); }
void real_call() { void (*p)(int) = &A::real; p(1); }
"#,
        &[("fixed_call", "A::fixed"), ("real_call", "A::real")],
    );
    for caller in ["fixed_call", "real_call"] {
        let targets: Vec<_> = analysis
            .call_edges
            .iter()
            .filter(|e| program.symbols.function(e.caller).name == caller)
            .collect();
        assert_eq!(targets.len(), 1, "{caller}: {targets:?}");
        let f = program.symbols.function(targets[0].callee);
        let ty = program.symbols.explicit_params(f).unwrap().get(0).unwrap();
        assert_eq!(
            program.types.get(ty).desc.as_ref(),
            &trace_ir::TypeDesc::Int
        );
    }
}

#[test]
fn array_initializer_reinterpretations_preserve_callback_round_trips() {
    for cast in ["reinterpret_cast<F0>(&target)", "(void (*)())&target"] {
        verify(
            &format!(
                r#"
using F0 = void (*)(); using F1 = void (*)(int);
void target(int) {{}}
F0 global[] = {{{cast}}};
void global_call() {{ F1 r = reinterpret_cast<F1>(global[0]); r(1); }}
void local_call() {{ F0 a[] = {{{cast}}}; F1 r = reinterpret_cast<F1>(a[0]); r(1); }}
"#
            ),
            &[("global_call", "target"), ("local_call", "target")],
        );
    }
}

#[test]
fn cast_array_members_keep_their_declared_aggregate_fields() {
    verify(
        r#"
using F0 = void (*)(); using F1 = void (*)(int);
void target(int) {}
struct Erased { F0 cb; }; struct Typed { F1 cb; };
Erased erased[] = {{reinterpret_cast<F0>(&target)}};
Typed typed[] = {{static_cast<F1>(&target)}};
void global_call() { F1 r = reinterpret_cast<F1>(erased[0].cb); r(1); }
void local_call() { Erased a[] = {{reinterpret_cast<F0>(&target)}}; F1 r = reinterpret_cast<F1>(a[0].cb); r(1); }
void typed_call() { typed[0].cb(1); }
"#,
        &[
            ("global_call", "target"),
            ("local_call", "target"),
            ("typed_call", "target"),
        ],
    );
}

#[test]
fn nested_alias_template_arguments_retain_member_function_values() {
    verify(
        r#"
template<class T> using Same = T;
template<class T> struct Box {};
struct C { void hit(Box<Same<int>> *) {} };
using M = void (C::*)(Box<int> *);
void caller(C *p) { M m = &C::hit; (p->*m)(nullptr); }
"#,
        &[("caller", "C::hit")],
    );
}

#[test]
fn character_template_arguments_retain_possible_numeric_equivalents() {
    verify(
        r#"
template<int N> struct Box {};
struct C {
    void digit(Box<'1'> *) {}
    void letter(Box<'a'> *) {}
    void separator(Box<1'000> *) {}
};
using D = void (C::*)(Box<49> *);
using L = void (C::*)(Box<97> *);
using S = void (C::*)(Box<1000> *);
void caller(C *p) {
    D d = &C::digit; (p->*d)(nullptr);
    L l = &C::letter; (p->*l)(nullptr);
    S s = &C::separator; (p->*s)(nullptr);
}
"#,
        &[
            ("caller", "C::digit"),
            ("caller", "C::letter"),
            ("caller", "C::separator"),
        ],
    );
}

#[test]
fn distinct_character_arguments_select_distinct_member_overloads() {
    let (program, analysis) = verify(
        r#"
template<int N> struct Box;
struct C {
    void hit(Box<'1'> *) {}
    void hit(Box<'a'> *) {}
};
using M = void (C::*)(Box<'1'> *);
void caller(C *p) { M m = &C::hit; (p->*m)(nullptr); }
"#,
        &[("caller", "C::hit")],
    );
    let targets: Vec<_> = analysis
        .call_edges
        .iter()
        .filter(|e| program.symbols.function(e.caller).name == "caller")
        .collect();
    assert_eq!(targets.len(), 1, "{targets:?}");
    let params = program
        .symbols
        .explicit_params(program.symbols.function(targets[0].callee))
        .unwrap();
    assert!(format!("{:?}", program.types.get(params.get(0).unwrap()).desc).contains("'1'"));
}

#[test]
fn reference_callback_parameters_preserve_named_and_abstract_shapes() {
    verify(
        r#"
void worker(int) {}
struct C {
    static void array(int (&a)[3]) {}
    static void function(void (&cb)(int)) { cb(1); }
    void array_member(int (&a)[3]) {}
    void function_member(void (&cb)(int)) { cb(2); }
};
void array_call() { int a[3]; void (*m)(int (&)[3]) = &C::array; m(a); }
void function_call() { void (*m)(void (&)(int)) = &C::function; m(worker); }
void array_member_call(C *p) { int a[3]; void (C::*m)(int (&)[3]) = &C::array_member; (p->*m)(a); }
void function_member_call(C *p) { void (C::*m)(void (&)(int)) = &C::function_member; (p->*m)(worker); }
"#,
        &[
            ("array_call", "C::array"),
            ("function_call", "C::function"),
            ("array_member_call", "C::array_member"),
            ("function_member_call", "C::function_member"),
        ],
    );
}

#[test]
fn reference_callback_shapes_work_with_names_on_either_side() {
    for (array, function) in [
        ("int (&)[3]", "void (&)(int)"),
        ("int (&named)[3]", "void (&named)(int)"),
        ("int (&&named)[3]", "void (&&named)(int)"),
        ("int (&named)[2][3]", "void (&named)(int)"),
    ] {
        let array_argument = if array.contains("[2]") {
            "int a[2][3];"
        } else {
            "int a[3];"
        };
        let array_value = if array.contains("&&") {
            "static_cast<int (&&)[3]>(a)"
        } else {
            "a"
        };
        let declared_array = array.replace("named", "");
        let declared_function = function.replace("named", "");
        verify(
            &format!(
                r#"
void worker(int) {{}}
struct C {{
    static void array({declared_array}) {{}}
    static void function({declared_function}) {{}}
}};
using A = void (*)({array}); using F = void (*)({function});
void array_call() {{ {array_argument} A m = &C::array; m({array_value}); }}
void function_call() {{ F m = &C::function; m(worker); }}
"#
            ),
            &[("array_call", "C::array"), ("function_call", "C::function")],
        );
    }
}

#[test]
fn reference_callback_selection_still_rejects_distinct_parameter_types() {
    let (program, analysis) = verify(
        r#"
struct C {
    static void array(int (&a)[3]) {}
    static void array(double (&a)[3]) {}
    static void function(void (&cb)(int)) {}
    static void function(void (&cb)(double)) {}
};
void worker(int) {}
void array_call() { int a[3]; void (*m)(int (&)[3]) = &C::array; m(a); }
void function_call() { void (*m)(void (&)(int)) = &C::function; m(worker); }
"#,
        &[("array_call", "C::array"), ("function_call", "C::function")],
    );
    for caller in ["array_call", "function_call"] {
        let targets: Vec<_> = analysis
            .call_edges
            .iter()
            .filter(|e| program.symbols.function(e.caller).name == caller)
            .collect();
        assert_eq!(targets.len(), 1, "{caller}: {targets:?}");
        let params = program
            .symbols
            .explicit_params(program.symbols.function(targets[0].callee))
            .unwrap();
        let desc = program.types.get(params.get(0).unwrap()).desc.as_ref();
        assert!(!format!("{desc:?}").contains("Double"), "{desc:?}");
    }
}

#[test]
fn concrete_callback_signatures_retain_static_template_instantiations() {
    verify(
        r#"
struct C { int data; };
struct A {
    template<class... T> static void pack(T... args) {}
    template<class... T> static void prefix(int first, T... args) {}
    template<class T> static void member(int T::*p) {}
};
void pack_call() { void (*p)(int) = &A::pack<int>; p(1); }
void empty_pack_call() { void (*p)() = &A::pack<>; p(); }
void multi_pack_call() { void (*p)(int, double) = &A::pack<int, double>; p(1, 2.0); }
void prefix_call() { void (*p)(int, double) = &A::prefix<double>; p(1, 2.0); }
void member_call() { void (*cb)(int C::*) = &A::member<C>; cb(&C::data); }
"#,
        &[
            ("pack_call", "A::pack"),
            ("empty_pack_call", "A::pack"),
            ("multi_pack_call", "A::pack"),
            ("prefix_call", "A::prefix"),
            ("member_call", "A::member"),
        ],
    );
}
