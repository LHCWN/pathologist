use super::cpp_review_257::verify;
use crate::common::*;

#[test]
fn virtual_member_pointer_keeps_selected_base_family() {
    let (p, a) = verify(
        "struct B { virtual void f() {} }; struct A { virtual void f() {} }; struct D final : B, A {}; void caller(D &d) { (d.*&A::f)(); }",
        &[("caller", "A::f")],
    );
    assert!(!has_any_edge(&p, &a, "caller", "B::f"));
    let (p, a) = verify(
        "struct A { virtual void f() {} }; struct D : A { void f(int) {} }; struct E final : D { void f() override {} }; void caller(D &d) { (d.*&A::f)(); }",
        &[("caller", "A::f"), ("caller", "E::f")],
    );
    assert!(!has_any_edge(&p, &a, "caller", "D::f"));
}

#[test]
fn free_arrow_star_returns_only_selected_overload_value() {
    let (p, a) = verify(
        "using F=void(*)(); void alpha(){} void beta(){} struct P{}; struct X{}; struct Y{}; F operator->*(P,X){return alpha;} F operator->*(P,Y){return beta;} void caller(P p){(p->*X{})();}",
        &[("caller", "alpha")],
    );
    assert!(!has_any_edge(&p, &a, "caller", "beta"));
}

#[test]
fn selected_free_arrow_star_returns_survive_cross_tu_merge() {
    let root = scratch(&[
        ("api.hpp", "using F=void(*)(); struct P{}; struct X{}; struct Y{}; F operator->*(P,X); F operator->*(P,Y);"),
        ("impl.cpp", "#include \"api.hpp\"\nvoid alpha(){} void beta(){} F operator->*(P,X){return alpha;} F operator->*(P,Y){return beta;}"),
        ("main.cpp", "#include \"api.hpp\"\nvoid caller(P p){(p->*X{})();}"),
    ]);
    for jobs in [1, 4] {
        let p = trace_parse::build_program_with_jobs(root.path(), &default_opts(root.path()), jobs)
            .unwrap();
        let (_, a) = trace_analysis::analyze(&p);
        assert!(has_any_edge(&p, &a, "caller", "alpha"));
        assert!(!has_any_edge(&p, &a, "caller", "beta"));
    }
}

#[test]
fn member_pointer_qualifiers_survive_typedef_and_header_merge() {
    let root = scratch(&[
        (
            "api.hpp",
            "struct C{void f();void f()const;}; typedef void(C::*M)()const; struct H{M pm;};",
        ),
        (
            "impl.cpp",
            "#include \"api.hpp\"\nvoid C::f(){} void C::f()const{}",
        ),
        (
            "main.cpp",
            "#include \"api.hpp\"\nvoid caller(const C &c){H h; h.pm=&C::f; (c.*h.pm)();}",
        ),
    ]);
    let p = trace_parse::build_program(root.path(), &default_opts(root.path())).unwrap();
    assert!(
        !p.diagnostics.iter().any(|d| d.stage == "parse"),
        "{:?}",
        p.diagnostics
    );
    let (_, a) = trace_analysis::analyze(&p);
    let targets: Vec<_> = a
        .call_edges
        .iter()
        .filter(|e| {
            p.symbols.function(e.caller).name == "caller"
                && p.symbols.function(e.callee).name == "C::f"
        })
        .map(|e| p.symbols.function(e.callee))
        .collect();
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert!(targets[0].is_const_member);
}

#[test]
fn virtual_member_pointer_results_stay_with_the_dispatching_site() {
    let root = scratch(&[("main.cpp", "int left,right; struct A { virtual int *get(){return &left;} }; struct L final:A { int *get() override{return &left;} }; struct R final:A { int *get() override{return &right;} }; void caller(L &l,R &r){auto pm=&A::get; int *one=(l.*pm)(); int *two=(r.*pm)();}")]);
    let p = trace_parse::build_program(root.path(), &default_opts(root.path())).unwrap();
    let (pag, a) = trace_analysis::analyze_with_options(
        &p,
        trace_analysis::AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    assert_points_to(&p, &pag, &a, "one", "left");
    assert_not_points_to(&p, &pag, &a, "one", "right");
    assert_points_to(&p, &pag, &a, "two", "right");
    assert_not_points_to(&p, &pag, &a, "two", "left");
}

#[test]
fn free_arrow_star_ranks_left_operand_conversion() {
    let (p,a)=verify("using F=void(*)(); void base(){} void derived(){} struct B{}; struct D:B{}; struct Tag{}; F operator->*(B&,Tag){return base;} F operator->*(D&,Tag){return derived;} void caller(D &d){(d->*Tag{})();}", &[("caller","derived")]);
    assert!(!has_any_edge(&p, &a, "caller", "base"));
}

#[test]
fn member_pointer_address_selection_preserves_const_and_ref_qualifiers() {
    for (definitions, signature, expected) in [
        ("void f() {} void f() const {}", "void (C::*)() const", true),
        ("void f() & {} void f() && {}", "void (C::*)() &", false),
        ("void f() & {} void f() && {}", "void (C::*)() &&", true),
    ] {
        let receiver = if signature.ends_with("&&") {
            "static_cast<C&&>(c)"
        } else {
            "c"
        };
        let source=format!("struct C{{{definitions}}}; using M={signature}; void caller(C &c){{M pm=&C::f; ({receiver}.*pm)();}}");
        let (p, a) = verify(&source, &[("caller", "C::f")]);
        let targets: Vec<_> = a
            .call_edges
            .iter()
            .filter(|e| {
                p.symbols.function(e.caller).name == "caller"
                    && p.symbols.function(e.callee).name == "C::f"
            })
            .map(|e| p.symbols.function(e.callee))
            .collect();
        assert_eq!(targets.len(), 1, "{signature}: {targets:?}");
        assert_eq!(
            targets[0].is_const_member || targets[0].ref_qualifier == Some(true),
            expected
        );
    }
}

#[test]
fn member_pointer_call_results_supply_chained_receivers() {
    verify("struct R{void run(){}}; struct C{R make(){return {};} R &ref(){static R r;return r;}}; void local(C &c){auto pm=&C::make;(c.*pm)().run();} void inlined(C &c){(c.*&C::make)().run();} void reference(C &c){auto pm=&C::ref;(c.*pm)().run();}",&[("local","R::run"),("inlined","R::run"),("reference","R::run")]);
}

#[test]
fn dependent_parameter_does_not_hide_later_known_parameters() {
    let (p,a)=verify("struct C{void f(int,int){} void f(int,double){}}; template<class T> void caller(C &c){using M=void(C::*)(T,int); M pm=&C::f;(c.*pm)(1,1);}",&[("caller","C::f")]);
    let targets: Vec<_> = a
        .call_edges
        .iter()
        .filter(|e| {
            p.symbols.function(e.caller).name == "caller"
                && p.symbols.function(e.callee).name == "C::f"
        })
        .collect();
    assert_eq!(targets.len(), 1, "{targets:?}");
}

#[test]
fn declared_variadic_pointer_requires_prefix_and_ellipsis() {
    let (p,a)=verify("struct C{void f(...){} void f(int){} void f(int,...){}}; using M=void(C::*)(int,...); void caller(C &c){M pm=&C::f;(c.*pm)(1,2);}",&[("caller","C::f")]);
    let targets: Vec<_> = a
        .call_edges
        .iter()
        .filter(|e| {
            p.symbols.function(e.caller).name == "caller"
                && p.symbols.function(e.callee).name == "C::f"
        })
        .map(|e| p.symbols.function(e.callee))
        .collect();
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert!(targets[0].variadic);
    assert_eq!(targets[0].explicit_arity, Some(1));
}

#[test]
fn union_arrow_star_operators_produce_callbacks() {
    verify("using F=void(*)(); void target(){} struct Tag{}; union U {int n; F operator->*(Tag){return target;}}; void caller(U &u){(u->*Tag{})();}",&[("caller","U::operator->*"),("caller","target")]);
}

#[test]
fn builtin_arrow_star_uses_pointer_conversion_instead_of_arrow_operator() {
    let (p,_) = verify("struct C{void f(){}}; struct Other{}; struct Proxy{operator C*(); Other *operator->();}; void caller(Proxy p){(p->*&C::f)();}",&[("caller","C::f")]);
    let caller = only_function(&p, "caller");
    let site = p
        .symbols
        .call_sites
        .iter()
        .find(|s| s.caller == caller && s.args_bound_past_this)
        .unwrap();
    assert_eq!(site.receiver_class.as_deref(), Some("C"));
}
