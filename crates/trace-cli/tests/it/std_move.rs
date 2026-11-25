use crate::common::{analysis_rows, analyze_source, cli_analyze, has_any_edge, scratch};
use trace_analysis::AnalysisResult;
use trace_ir::{FlowConstraint, Program};

fn analyze(source: &str) -> (Program, AnalysisResult) {
    analyze_source(&[("move.cpp", source)])
}

fn assert_edge(program: &Program, result: &AnalysisResult, caller: &str, callee: &str) {
    assert!(
        has_any_edge(program, result, caller, callee),
        "missing edge from {caller} to {callee}"
    );
}

#[test]
fn move_and_forward_spellings_preserve_flow_and_dispatch() {
    let (p, a) = analyze(
        r#"
namespace std {
    template<class T> T&& move(T& t);
    template<class T> T&& forward(T& t);
}
namespace s = std;
#define MOVED(x) std::move(x)
struct C { void f() {} };
int *g;
int *qualified, *global, *parenthesized, *macro, *explicit_arg;
int *imported, *directive, *alias, *forwarded, *using_forward, *qualified_alias;
void spellings(C& c) {
    qualified = std :: move(g);
    global = ::std::move(g);
    parenthesized = (std::move)(g);
    macro = MOVED(g);
    explicit_arg = std::move<int*>(g);
    s::move(c).f();
}
void imports(C& c) {
    using std::move;
    using s::forward;
    imported = move(g);
    using_forward = forward<int*>(g);
    move(c).f();
}
void directives(C& c) {
    using namespace s;
    directive = move(g);
    forwarded = forward<int*>(g);
    forward<C>(c).f();
}
namespace outer { namespace reexport = ::s; }
void qualified_aliases() { qualified_alias = ::outer::reexport::move(g); }
void aliases(C& c) {
    alias = s::move(g);
    s::forward<C>(c).f();
}
int *other;
namespace custom { int *move(int *p) { return other; } }
int *shadowed;
void ordinary() {
    using namespace std;
    using custom::move;
    shadowed = move(g);
}
"#,
    );
    for dst in [
        "qualified",
        "global",
        "parenthesized",
        "macro",
        "explicit_arg",
        "imported",
        "directive",
        "alias",
        "forwarded",
        "using_forward",
        "qualified_alias",
    ] {
        assert!(
            p.flow
                .iter()
                .any(|flow| matches!(flow, FlowConstraint::Copy { dst: d, src }
                if p.symbols.variable(*d).name == dst && p.symbols.variable(*src).name == "g")),
            "missing g -> {dst}"
        );
    }
    for caller in ["spellings", "imports", "directives", "aliases"] {
        assert_edge(&p, &a, caller, "C::f");
    }
    assert_edge(&p, &a, "ordinary", "custom::move");
    assert!(!p
        .flow
        .iter()
        .any(|flow| matches!(flow, FlowConstraint::Copy { dst, src }
        if p.symbols.variable(*dst).name == "shadowed" && p.symbols.variable(*src).name == "g")));
}

#[test]
fn explicit_template_arguments_type_the_receiver() {
    let (p, a) = analyze(
        r#"
struct B { void m() {} };
struct D : B { void m() {} };
void forwarded(D& d) { std::forward<B>(d).m(); }
void moved(D& d) { std::move<B>(d).m(); }
void cast(D& d) { std::move(static_cast<B&>(d)).m(); }
"#,
    );
    for caller in ["forwarded", "moved", "cast"] {
        assert_edge(&p, &a, caller, "B::m");
        assert!(!has_any_edge(&p, &a, caller, "D::m"), "{caller}");
    }
}

#[test]
fn ref_qualified_member_edges_agree_with_return_flow() {
    let (p, a) = analyze(
        r#"
void lvalue() {}
void rvalue() {}
void first() {}
void second() {}
struct R { void (*visit)(); };
struct S { void (*visit)(); };
R r;
S s;
struct C {
    R* f() & { lvalue(); return &r; }
    S* f() && { rvalue(); return &s; }
};
void prepare() { r.visit = first; s.visit = second; }
void moved(C& c) { auto x = std::move(c).f(); x->visit(); }
void forwarded(C& c) { auto x = std::forward<C>(c).f(); x->visit(); }
void original(C& c) { auto x = c.f(); x->visit(); }
void forwarded_lvalue(C& c) { auto x = std::forward<C&>(c).f(); x->visit(); }
void array_lvalue(C (&c)[2]) { auto x = (*c).f(); x->visit(); }
void smart_lvalue(std::shared_ptr<C> c) { auto x = (*c).f(); x->visit(); }
void comma_lvalue(C& c) { auto x = (c, c).f(); x->visit(); }
void assigned_lvalue(C& c) { auto x = (c = c).f(); x->visit(); }
void conditional_rvalue(C& c, bool b) { auto x = (b ? c : C{}).f(); x->visit(); }
C& uncertain(int);
C&& uncertain(long);
void unknown() { uncertain(untyped()).f(); }
"#,
    );
    for (caller, qualifier, target, excluded) in [
        ("moved", true, "second", "first"),
        ("forwarded", true, "second", "first"),
        ("original", false, "first", "second"),
        ("forwarded_lvalue", false, "first", "second"),
        ("array_lvalue", false, "first", "second"),
        ("smart_lvalue", false, "first", "second"),
        ("comma_lvalue", false, "first", "second"),
        ("assigned_lvalue", false, "first", "second"),
        ("conditional_rvalue", true, "second", "first"),
    ] {
        let overloads: Vec<_> = a
            .call_edges
            .iter()
            .filter_map(|edge| {
                let target = p.symbols.function_by_id(edge.callee)?;
                (p.symbols.function(edge.caller).name == caller && target.name == "C::f")
                    .then_some(target.ref_qualifier)
            })
            .collect();
        assert_eq!(overloads, [Some(qualifier)], "{caller}");
        assert_edge(&p, &a, caller, target);
        assert!(!has_any_edge(&p, &a, caller, excluded), "{caller}");
    }
    assert_eq!(
        a.call_edges
            .iter()
            .filter(|edge| {
                p.symbols.function(edge.caller).name == "unknown"
                    && p.symbols
                        .function_by_id(edge.callee)
                        .is_some_and(|f| f.name == "C::f")
            })
            .count(),
        2
    );
}

fn assert_reference_target(
    p: &Program,
    a: &AnalysisResult,
    caller: &str,
    callee: &str,
    rvalue: bool,
) {
    let bindings: Vec<_> = a
        .call_edges
        .iter()
        .filter_map(|edge| {
            let f = p.symbols.function_by_id(edge.callee)?;
            (p.symbols.function(edge.caller).name == caller && f.name == callee).then(|| {
                f.parameter_references
                    .first()
                    .copied()
                    .flatten()
                    .map(|r| r.rvalue)
            })
        })
        .collect();
    assert_eq!(bindings, [Some(rvalue)], "{caller} -> {callee}");
}

#[test]
fn rvalue_arguments_select_free_functions_and_constructors() {
    let (p, a) = analyze(
        r#"
struct D {};
void f(D&) {}
void f(D&&) {}
void free_function(D d) { f(std::move(d)); }
struct T { T(const T&); T(T&&); };
void braced(T t) { T x{std::move(t)}; }
void direct(T t) { T x((std::move(t))); }
void heap(T t) { T* x = new T(std::move(t)); }
"#,
    );
    assert_reference_target(&p, &a, "free_function", "f", true);
    for caller in ["braced", "direct", "heap"] {
        assert_reference_target(&p, &a, caller, "T::T", true);
    }
}

#[test]
fn copy_initialization_and_assignment_have_constructor_and_operator_edges() {
    let (p, a) = analyze(
        r#"
struct T {
    T(const T&) {}
    T(T&&) {}
    T& operator=(const T&) { return *this; }
    T& operator=(T&&) { return *this; }
};
void copy(T t) { T x = t; }
void move(T t) { T x = std::move(t); }
void copy_assign(T a, T b) { a = b; }
void move_assign(T a, T b) { a = std::move(b); }
"#,
    );
    for (caller, target, rvalue) in [
        ("copy", "T::T", false),
        ("move", "T::T", true),
        ("copy_assign", "T::operator=", false),
        ("move_assign", "T::operator=", true),
    ] {
        assert_reference_target(&p, &a, caller, target, rvalue);
    }
}

#[test]
fn moved_parenthesized_and_cast_functors_dispatch() {
    let (p, a) = analyze(
        r#"
void target() {}
struct F { void operator()() { target(); } };
void plain(F& f) { f(); }
void moved(F& f) { std::move(f)(); }
void enclosed(F& f) { (f)(); }
void cast(F& f) { static_cast<F&&>(f)(); }
void pointer() { void (*cb)() = target; std::move(cb)(); }
void closure() { auto l = [] { target(); }; std::move(l)(); }
"#,
    );
    for caller in ["plain", "moved", "enclosed", "cast"] {
        assert_edge(&p, &a, caller, "F::operator()");
    }
    assert_edge(&p, &a, "pointer", "target");
    assert!(a
        .call_edges
        .iter()
        .any(|edge| p.symbols.function(edge.caller).name == "closure"
            && p.symbols
                .function_by_id(edge.callee)
                .is_some_and(|f| f.name.contains("$lambda"))));
}

#[test]
fn moved_from_values_remain_in_the_may_analysis() {
    let (p, a) = analyze(
        r#"
struct Obj { void ok() {} };
void ownership() {
    Obj object;
    std::unique_ptr<Obj> a = &object, b = std::move(a);
    a->ok();
}
void target() {}
void other() {}
struct H { void (*cb)(); };
void fields() {
    H a;
    a.cb = target;
    H b = std::move(a);
    a.cb = other;
    b.cb();
}
"#,
    );
    assert_edge(&p, &a, "ownership", "Obj::ok");
    assert_edge(&p, &a, "fields", "target");
    assert_edge(&p, &a, "fields", "other");
}

#[test]
fn move_and_forward_export_ordinary_edges_deterministically() {
    let root = scratch(&[(
        "move.cpp",
        r#"
namespace std { template<class T> T&& forward(T& t) { return static_cast<T&&>(t); } }
namespace s = std;
int *input, *output;
void run() {
    using std::forward;
    output = s::move(input);
    output = forward<int*>(input);
}
"#,
    )]);
    let first = cli_analyze(root.path(), &["--jobs", "1"]);
    let repeated = cli_analyze(root.path(), &["--jobs", "2"]);
    let conn = rusqlite::Connection::open(first.path()).unwrap();
    let mut query = conn
        .prepare(
            "SELECT cs.callee_text, e.resolution
         FROM raw_call_edges e
         JOIN raw_call_sites cs ON cs.id = e.call_site_id
         JOIN raw_functions f ON f.id = e.caller_fn_id
         WHERE f.name = 'run'
         ORDER BY cs.callee_text",
        )
        .unwrap();
    let rows: Vec<(String, String)> = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        [
            ("std::forward".to_owned(), "direct".to_owned()),
            ("std::move".to_owned(), "external".to_owned()),
        ]
    );
    assert_eq!(analysis_rows(&first), analysis_rows(&repeated));
}

#[test]
fn deeply_nested_imported_moves_preserve_receiver_type() {
    let nested = format!("{}object{}", "move(".repeat(32), ")".repeat(32));
    let (p, a) = analyze(&format!(
        "struct C {{ void f() {{}} }}; void nested(C& object) {{ using std::move; {nested}.f(); }}"
    ));
    assert_edge(&p, &a, "nested", "C::f");
}

#[test]
fn receiver_viability_precedes_argument_binding_preference() {
    let (p, a) = analyze(
        r#"
struct T {};
struct L { void visit_left() {} };
struct R { void visit_right() {} };
struct C {
    R* f(T&&) &&;
    L* f(const T&) const &;
    void implicit(T& t) const { f(std::move(t))->visit_left(); }
};
struct Derived : C {
    void qualified(T& t) const { C::f(std::move(t))->visit_left(); }
    void global_qualified(T& t) const { ::C::f(std::move(t))->visit_left(); }
};
void run(const C& c, T& t) { c.f(std::move(t))->visit_left(); }
namespace n {
    struct Base {
        R* f(T&&) &&;
        L* f(const T&) const &;
    };
    struct Derived : Base {
        void qualified(T& t) const { Base::f(std::move(t))->visit_left(); }
    };
}
"#,
    );
    assert_reference_target(&p, &a, "run", "C::f", false);
    assert_edge(&p, &a, "run", "L::visit_left");
    assert_reference_target(&p, &a, "C::implicit", "C::f", false);
    assert_edge(&p, &a, "C::implicit", "L::visit_left");
    assert_reference_target(&p, &a, "Derived::qualified", "C::f", false);
    assert_edge(&p, &a, "Derived::qualified", "L::visit_left");
    assert_reference_target(&p, &a, "Derived::global_qualified", "C::f", false);
    assert_edge(&p, &a, "Derived::global_qualified", "L::visit_left");
    assert_reference_target(&p, &a, "n::Derived::qualified", "n::Base::f", false);
    assert_edge(&p, &a, "n::Derived::qualified", "L::visit_left");
}

#[test]
fn unresolved_forwarded_receiver_keeps_overloads_for_both_categories() {
    let (p, a) = analyze(
        r#"
struct A {};
struct C {
    void f(const A&) &;
    void f(A&&) &&;
};
template<class T>
void run(C& c, A& a) { std::forward<T>(c).f(std::move(a)); }
"#,
    );
    let mut categories: Vec<_> = a
        .call_edges
        .iter()
        .filter_map(|edge| {
            let target = p.symbols.function(edge.callee);
            (p.symbols.function(edge.caller).name == "run" && target.name == "C::f")
                .then_some(target.ref_qualifier)
                .flatten()
        })
        .collect();
    categories.sort_unstable();
    assert_eq!(categories, [false, true]);
}

#[test]
fn dependent_receiver_keeps_candidates_for_each_cv_and_reference_category() {
    let (p, a) = analyze(
        r#"
struct B {};
struct D : B {};
struct A {};
struct C {
    void f(const B&) &;
    void f(D&&) &&;
    void g(A&&) &;
    void g(const A&) const &;
};
template<class R>
void category(C& c, D& d) { std::forward<R>(c).f(std::move(d)); }
template<class R>
void qualification(C& c, A& a) { std::forward<R>(c).g(std::move(a)); }
"#,
    );
    for (caller, method) in [("category", "C::f"), ("qualification", "C::g")] {
        let mut bindings: Vec<_> = a
            .call_edges
            .iter()
            .filter_map(|edge| {
                let target = p.symbols.function(edge.callee);
                (p.symbols.function(edge.caller).name == caller && target.name == method)
                    .then(|| target.parameter_references[0].unwrap().rvalue)
            })
            .collect();
        bindings.sort_unstable();
        assert_eq!(bindings, [false, true], "{caller}");
    }
}

#[test]
fn virtual_move_calls_rank_the_static_overload_set() {
    let (p, a) = analyze(
        r#"
struct A {};
struct B : A {};
struct Base {
    virtual void f(const A&) & {}
};
struct Derived : Base {
    void f(const A&) & override {}
    void f(B&&) & {}
};
void run(Base& base, B& b) { base.f(std::move(b)); }
"#,
    );
    let mut owners: Vec<_> = a
        .call_edges
        .iter()
        .filter_map(|edge| {
            let target = p.symbols.function(edge.callee);
            if p.symbols.function(edge.caller).name == "run"
                && matches!(target.name.as_str(), "Base::f" | "Derived::f")
            {
                assert!(!target.parameter_references[0].unwrap().rvalue);
                Some(target.name.as_str())
            } else {
                None
            }
        })
        .collect();
    owners.sort_unstable();
    assert_eq!(owners, ["Base::f", "Derived::f"]);
}

#[test]
fn reference_ranking_applies_to_base_subobjects() {
    let (p, a) = analyze(
        r#"
struct A {};
struct B : A {};
void take(A&);
void take(A&&);
void run(B& b) { take(std::move(b)); }
"#,
    );
    assert_reference_target(&p, &a, "run", "take", true);
}

#[test]
fn forwarded_receiver_alternatives_survive_ordinary_arguments() {
    let (p, a) = analyze(
        r#"
struct B {};
struct D : B {};
struct C {
    void f(const B&) &;
    void f(D&) &&;
};
struct Wrapper { C field; };
template<class R>
void conditional(bool flag, C& lhs, C& rhs, D& d) {
    (flag ? std::forward<R>(lhs) : std::forward<R>(rhs)).f(d);
}
template<class R>
void member(Wrapper& wrapper, D& d) { std::forward<R>(wrapper).field.f(d); }
template<class R>
void run(C& c, D& d) { std::forward<R>(c).f(d); }
template<class R>
void comma(C& c, D& d) { (static_cast<void>(0), std::forward<R>(c)).f(d); }
"#,
    );
    for caller in ["run", "comma", "member", "conditional"] {
        let mut categories: Vec<_> = a
            .call_edges
            .iter()
            .filter_map(|edge| {
                let target = p.symbols.function(edge.callee);
                (p.symbols.function(edge.caller).name == caller && target.name == "C::f")
                    .then_some(target.ref_qualifier)
                    .flatten()
            })
            .collect();
        categories.sort_unstable();
        assert_eq!(categories, [false, true], "{caller}");
    }
}

#[test]
fn default_arguments_follow_the_receiver_selected_overload() {
    let (p, a) = analyze(
        r#"
struct B {};
struct D : B {};
int left_default();
int right_default();
struct C {
    void f(const B&, int = left_default()) &;
    void f(D&&, int = right_default()) &&;
};
void run(C& c, D& d) { c.f(std::move(d)); }
"#,
    );
    assert_reference_target(&p, &a, "run", "C::f", false);
    assert_edge(&p, &a, "run", "left_default");
    assert!(!has_any_edge(&p, &a, "run", "right_default"));
}

#[test]
fn global_adl_candidate_keeps_its_ordinary_call_and_return() {
    let (p, a) = analyze(
        r#"
struct T {};
T* other;
T* move(T*) { return other; }
T* out;
void run(T* in) {
    using std::move;
    out = move(in);
}
"#,
    );
    assert_edge(&p, &a, "run", "move");
    assert!(!has_any_edge(&p, &a, "run", "std::move"));
    assert!(!p
        .flow
        .iter()
        .any(|flow| matches!(flow, FlowConstraint::Copy { dst, src }
        if p.symbols.variable(*dst).name == "out" && p.symbols.variable(*src).name == "in")));
}

#[test]
fn adl_uses_class_and_enum_template_argument_namespaces() {
    let (p, a) = analyze(
        r#"
template<class T> struct Box {};
namespace app {
    struct Tag {};
    Box<Tag>* move(Box<Tag>*);
}
namespace enums {
    enum class E {};
    Box<E>* move(Box<E>*);
}
Box<app::Tag>* out;
Box<enums::E>* enum_out;
void run(Box<app::Tag>* in) {
    using std::move;
    out = move(in);
}
void dereference(Box<app::Tag>** in) {
    using std::move;
    out = move(*in);
}
void address_of(Box<app::Tag>& in) {
    using std::move;
    out = move(&in);
}
void enum_argument(Box<enums::E>* in) {
    using std::move;
    enum_out = move(in);
}
"#,
    );
    for (caller, callee) in [
        ("run", "app::move"),
        ("dereference", "app::move"),
        ("address_of", "app::move"),
        ("enum_argument", "enums::move"),
    ] {
        assert_edge(&p, &a, caller, callee);
        assert!(!has_any_edge(&p, &a, caller, "std::move"));
    }
    assert!(!p
        .flow
        .iter()
        .any(|flow| matches!(flow, FlowConstraint::Copy { dst, src }
        if matches!(p.symbols.variable(*dst).name.as_str(), "out" | "enum_out")
            && p.symbols.variable(*src).name == "in")));
}

#[test]
fn mutable_lvalue_reference_adl_candidate_cannot_bind_an_xvalue_pointer() {
    let (p, a) = analyze(
        r#"
namespace app {
    struct T {};
    T* move(T*&);
}
app::T* out;
void run(app::T* in) {
    using std::move;
    out = move(static_cast<app::T*&&>(in));
}
void lvalue(app::T* in) {
    using std::move;
    move(in);
}
"#,
    );
    assert_edge(&p, &a, "run", "std::move");
    assert!(!has_any_edge(&p, &a, "run", "app::move"));
    assert!(p
        .flow
        .iter()
        .any(|flow| matches!(flow, FlowConstraint::Copy { dst, src }
        if p.symbols.variable(*dst).name == "out" && p.symbols.variable(*src).name == "in")));
    assert_edge(&p, &a, "lvalue", "app::move");
}

#[test]
fn rvalue_reference_adl_candidate_cannot_bind_a_named_pointer() {
    let (p, a) = analyze(
        r#"
namespace app {
    struct T {};
    T* move(T*&&);
}
app::T* out;
void run(app::T* in) {
    using std::move;
    out = move(in);
}
void rvalue(app::T* in) {
    using std::move;
    move(static_cast<app::T*&&>(in));
}
"#,
    );
    assert_edge(&p, &a, "run", "std::move");
    assert!(!has_any_edge(&p, &a, "run", "app::move"));
    assert!(p
        .flow
        .iter()
        .any(|flow| matches!(flow, FlowConstraint::Copy { dst, src }
        if p.symbols.variable(*dst).name == "out" && p.symbols.variable(*src).name == "in")));
    assert_edge(&p, &a, "rvalue", "app::move");
}

#[test]
fn conditional_base_glvalue_invokes_copy_constructor() {
    let (p, a) = analyze(
        r#"
struct B { B(); B(const B&); B(B&&); };
struct D : B {};
void run(bool flag, B& b, D& d) { B x = flag ? b : d; }
"#,
    );
    assert_reference_target(&p, &a, "run", "B::B", false);
}

#[test]
fn adl_candidates_prevent_an_imported_move_from_becoming_an_identity() {
    let (p, a) = analyze(
        r#"
namespace std { template<class T> T&& move(T&); }
namespace app {
    struct T {};
    T* other;
    T* move(T*) { return other; }
}
app::T* dst;
void run(app::T* src) {
    using std::move;
    dst = move(src);
}
"#,
    );
    assert_edge(&p, &a, "run", "app::move");
    assert!(!p
        .flow
        .iter()
        .any(|flow| matches!(flow, FlowConstraint::Copy { dst, src }
        if p.symbols.variable(*dst).name == "dst" && p.symbols.variable(*src).name == "src")));
}

#[test]
fn nearer_types_shadow_the_standard_namespace() {
    let (p, a) = analyze(
        r#"
namespace app {
    struct std { static int* move(int* p) { return p; } };
    int* shadowed;
    int* standard;
    void shadow(int* src) { shadowed = std::move(src); }
    void global(int* src) { standard = ::std::move(src); }
}
"#,
    );
    assert_edge(&p, &a, "app::shadow", "app::std::move");
    assert!(!has_any_edge(&p, &a, "app::shadow", "std::move"));
    let copies_from_src = |name| {
        p.flow.iter().any(|flow| {
            matches!(flow, FlowConstraint::Copy { dst, src }
            if p.symbols.variable(*dst).name == name && p.symbols.variable(*src).name == "src")
        })
    };
    assert!(!copies_from_src("shadowed"));
    assert!(copies_from_src("standard"));
}

#[test]
fn selected_move_overload_supplies_return_constness() {
    let (p, a) = analyze(
        r#"
struct T {};
struct C { void f() &; void f() const &; };
C& get(T&);
const C& get(T&&);
void run(T& t) { get(std::move(t)).f(); }
"#,
    );
    assert_reference_target(&p, &a, "run", "get", true);
    let constness: Vec<_> = a
        .call_edges
        .iter()
        .filter_map(|edge| {
            let f = p.symbols.function_by_id(edge.callee)?;
            (p.symbols.function(edge.caller).name == "run" && f.name == "C::f")
                .then_some(f.is_const_member)
        })
        .collect();
    assert_eq!(constness, [true]);
}

#[test]
fn explicit_move_constructors_are_eligible_only_for_direct_initialization() {
    let (p, a) = analyze(
        r#"
struct T {
    T(const T&) {}
    explicit T(T&&);
};
T::T(T&&) {}
struct U {
    U(const U&) {}
    explicit(false) U(U&&) {}
};
struct V {
    V(const V&) {}
    explicit(true) V(V&&) {}
};
void copy(T& t) { T x = std::move(t); }
void direct(T& t) { T x((std::move(t))); }
void implicit_condition(U& u) { U x = std::move(u); }
void explicit_condition(V& v) { V x = std::move(v); }
"#,
    );
    for (caller, name, rvalue) in [
        ("copy", "T::T", false),
        ("direct", "T::T", true),
        ("implicit_condition", "U::U", true),
        ("explicit_condition", "V::V", false),
    ] {
        assert_reference_target(&p, &a, caller, name, rvalue);
    }
}

#[test]
fn forwarding_lvalues_and_preserving_mutable_referents_narrows_overloads() {
    let (p, a) = analyze(
        r#"
struct T {};
void choose(T&);
void choose(T&&);
void cv_lvalue(T&);
void cv_lvalue(const T&);
void cv_rvalue(T&&);
void cv_rvalue(const T&&);
void forwarded(T& t) { choose(std::forward<T&>(t)); }
void forwarded_cv(T& t) { cv_lvalue(std::forward<T&>(t)); }
void moved(T& t) { cv_rvalue(std::move(t)); }
void moved_const(const T& t) { cv_rvalue(std::move(t)); }
"#,
    );
    assert_reference_target(&p, &a, "forwarded", "choose", false);
    for (caller, callee, rvalue, is_const) in [
        ("forwarded_cv", "cv_lvalue", false, false),
        ("moved", "cv_rvalue", true, false),
        ("moved_const", "cv_rvalue", true, true),
    ] {
        assert_reference_target(&p, &a, caller, callee, rvalue);
        let target = a
            .call_edges
            .iter()
            .find_map(|edge| {
                let f = p.symbols.function_by_id(edge.callee)?;
                (p.symbols.function(edge.caller).name == caller && f.name == callee).then_some(f)
            })
            .unwrap();
        assert_eq!(
            target.parameter_references[0].unwrap().is_const,
            Some(is_const),
            "{caller}"
        );
    }
}

#[test]
fn reference_initialization_does_not_construct_an_object() {
    let (p, a) = analyze(
        r#"
struct T { T(const T&) {} T(T&&) {} };
using Ref = T&;
void bind(T& t) {
    T& lvalue = t;
    T&& rvalue = std::move(t);
    Ref collapsed = t;
}
"#,
    );
    assert!(!has_any_edge(&p, &a, "bind", "T::T"));
}

#[test]
fn copy_initialization_uses_glvalue_category() {
    let (p, a) = analyze(
        r#"
struct T { T(); T(const T&) {} T(T&&) {} };
struct Box { T field; };
T& get_ref();
T&& get_rvalue_ref();
T produce();
void field(Box& box) { T copy = box.field; }
void reference() { T copy = get_ref(); }
void rvalue_reference() { T copy = get_rvalue_ref(); }
void dereference(T* p) { T copy = *p; }
void conditional(T& a, T& b, bool c) { T copy = c ? a : b; }
void value() { T copy = produce(); }
void mixed(T& a, bool c) { T copy = c ? a : produce(); }
"#,
    );
    for caller in ["field", "reference", "dereference", "conditional"] {
        assert_reference_target(&p, &a, caller, "T::T", false);
    }
    assert_reference_target(&p, &a, "rvalue_reference", "T::T", true);
    for caller in ["value", "mixed"] {
        assert!(!has_any_edge(&p, &a, caller, "T::T"), "{caller}");
    }
}

#[test]
fn move_and_forward_imports_use_the_lexical_namespace_alias() {
    let (p, a) = analyze(
        r#"
struct Input { void run() {} };
struct Output { void run() {} };
namespace custom {
    Output move(Input&);
    Output forward(Input&);
}
namespace app {
    namespace std = custom;
    void local_move(Input& x) { using std::move; move(x).run(); }
    void local_forward(Input& x) { using std::forward; forward(x).run(); }
    void directive(Input& x) { using namespace std; move(x).run(); }
    void standard(Input& x) { using ::std::move; move(x).run(); }
}
"#,
    );
    for (caller, callee) in [
        ("app::local_move", "custom::move"),
        ("app::local_forward", "custom::forward"),
        ("app::directive", "custom::move"),
    ] {
        assert_edge(&p, &a, caller, callee);
        assert_edge(&p, &a, caller, "Output::run");
        assert!(!has_any_edge(&p, &a, caller, "Input::run"));
        assert!(!has_any_edge(&p, &a, caller, "std::move"));
        assert!(!has_any_edge(&p, &a, caller, "std::forward"));
    }
    assert_edge(&p, &a, "app::standard", "Input::run");
}

#[test]
fn parenthesized_move_and_forward_suppress_adl() {
    let (p, a) = analyze(
        r#"
namespace std {
    template<class T> T&& move(T&);
    template<class T> T&& forward(T&);
}
namespace app {
    struct T { void run() {} };
    struct Other { void run() {} };
    Other move(T&);
    template<class U> Other forward(T&);
}
void moved(app::T& x) { using std::move; (move)(x).run(); }
void forwarded(app::T& x) { using std::forward; (forward<app::T&>)(x).run(); }
"#,
    );
    for (caller, callee) in [("moved", "app::move"), ("forwarded", "app::forward")] {
        assert_edge(&p, &a, caller, "app::T::run");
        assert!(!has_any_edge(&p, &a, caller, callee));
        assert!(!has_any_edge(&p, &a, caller, "app::Other::run"));
    }
}

#[test]
fn forward_recovers_the_referent_of_a_reference_alias() {
    let (p, a) = analyze(
        r#"
struct T {};
using Ref = T&;
void choose(T&);
void choose(T&&);
void test(T& obj) { choose(std::forward<Ref>(obj)); }
"#,
    );
    assert_reference_target(&p, &a, "test", "choose", false);
}

#[test]
fn overload_parameters_use_collapsed_reference_categories() {
    let (p, a) = analyze(
        r#"
struct T {};
using Ref = T&;
void choose(Ref&&);
void choose(const T&);
void test(T& obj) { choose(std::forward<T&>(obj)); }
"#,
    );
    assert_reference_target(&p, &a, "test", "choose", false);
    let target = a
        .call_edges
        .iter()
        .find_map(|edge| {
            let target = p.symbols.function_by_id(edge.callee)?;
            (p.symbols.function(edge.caller).name == "test" && target.name == "choose")
                .then_some(target)
        })
        .unwrap();
    assert_eq!(
        target.parameter_references[0].unwrap().is_const,
        Some(false)
    );
}

#[test]
fn class_assignment_skips_unrelated_overloads() {
    let (p, a) = analyze(
        r#"
struct A {};
struct Handle { Handle& operator=(A*); };
struct Mixed {
    Mixed& operator=(A*);
    Mixed& operator=(const Mixed&);
};
struct H {
    H& operator=(const H&) = default;
    H& operator=(H&& value) { return *this; }
};
void defaulted_copy(H& a, H& b) { a = b; }
void explicit_move(H& a, H& b) { a = std::move(b); }
void implicit_copy(Handle& a, Handle& b) { a = b; }
void explicit_copy(Mixed& a, Mixed& b) { a = b; }
"#,
    );
    assert!(!has_any_edge(&p, &a, "implicit_copy", "Handle::operator="));
    assert_reference_target(&p, &a, "explicit_copy", "Mixed::operator=", false);
    assert!(!has_any_edge(&p, &a, "defaulted_copy", "H::operator="));
    assert_reference_target(&p, &a, "explicit_move", "H::operator=", true);
}

#[test]
fn forwarding_resolves_decltype_and_keeps_unknown_categories() {
    let (p, a) = analyze(
        r#"
struct Q { void f() &; void f() &&; };
void declared_reference(Q& q) { std::forward<decltype(q)>(q).f(); }
void declared_rvalue_reference(Q&& q) { std::forward<decltype(q)>(q).f(); }
void parenthesized_reference(Q&& q) { std::forward<decltype((q))>(q).f(); }
void local_reference(Q& obj) { Q& q = obj; std::forward<decltype(q)>(q).f(); }
void local_rvalue_reference(Q&& obj) {
    Q&& q = std::move(obj); std::forward<decltype(q)>(q).f();
}
void unresolved(Q& q) { std::forward<AnUnresolvedType>(q).f(); }
template<class T>
void dependent(Q& q) { std::forward<T&&>(q).f(); }
"#,
    );
    for (caller, expected) in [
        ("declared_reference", vec![false]),
        ("declared_rvalue_reference", vec![true]),
        ("parenthesized_reference", vec![false]),
        ("local_reference", vec![false]),
        ("local_rvalue_reference", vec![true]),
        ("unresolved", vec![false, true]),
        ("dependent", vec![false, true]),
    ] {
        let mut categories: Vec<_> = a
            .call_edges
            .iter()
            .filter_map(|edge| {
                let function = p.symbols.function_by_id(edge.callee)?;
                (function.name == "Q::f" && p.symbols.function(edge.caller).name == caller)
                    .then_some(function.ref_qualifier)
                    .flatten()
            })
            .collect();
        categories.sort_unstable();
        assert_eq!(categories, expected, "{caller}");
    }
}

#[test]
fn forwarding_through_decltype_preserves_constness() {
    let (p, a) = analyze(
        r#"
struct Q { void f() &; void f() const &; };
void declared(const Q& q) { std::forward<decltype(q)>(q).f(); }
void parenthesized(const Q& q) { std::forward<decltype((q))>(q).f(); }
void qualified(Q q) { std::forward<const decltype(q)&>(q).f(); }
struct D : Q { void f() &; };
void base(Q& q, D& d) { std::forward<decltype(q)>(d).f(); }
"#,
    );
    for caller in ["declared", "parenthesized", "qualified"] {
        let targets: Vec<_> = a
            .call_edges
            .iter()
            .filter_map(|edge| {
                let target = p.symbols.function(edge.callee);
                (p.symbols.function(edge.caller).name == caller && target.name == "Q::f")
                    .then_some(target.is_const_member)
            })
            .collect();
        assert_eq!(targets, [true], "{caller}");
    }
    assert_edge(&p, &a, "base", "Q::f");
    assert!(!has_any_edge(&p, &a, "base", "D::f"));
}

#[test]
fn explicit_forward_ignores_non_template_adl_overloads() {
    let (p, a) = analyze(
        r#"
namespace app {
    struct T { void f() {}; };
    int forward(T&);
}
void non_template(app::T& x) {
    using std::forward;
    forward<app::T&>(x).f();
}
namespace other {
    struct T {};
    struct U {};
    template<class Arg> U forward(T&);
}
void template_competitor(other::T& x) {
    using std::forward;
    forward<other::T&>(x);
}
"#,
    );
    assert_edge(&p, &a, "non_template", "std::forward");
    assert_edge(&p, &a, "non_template", "app::T::f");
    assert!(!has_any_edge(&p, &a, "non_template", "app::forward"));
    assert_edge(&p, &a, "template_competitor", "other::forward");
}

#[test]
fn pointer_reference_parameters_keep_their_overload_identity() {
    let (p, a) = analyze(
        r#"
struct Q {};
void take(Q*&);
void take(Q*&&);
void lvalue(Q* p) { take(std::forward<Q*&>(p)); }
void rvalue(Q* p) { take(std::move(p)); }
void take(Q*& p) {}
void take(Q*&& p) {}
"#,
    );
    assert_reference_target(&p, &a, "lvalue", "take", false);
    assert_reference_target(&p, &a, "rvalue", "take", true);
}

#[test]
fn pointer_reference_overloads_keep_pointee_constness() {
    let (p, a) = analyze(
        r#"
struct Q {};
void take(Q*&&) {}
void take(const Q*&&) {}
void mutable_pointee(Q* p) { take(std::move(p)); }
void const_pointee(const Q* p) { take(std::move(p)); }
"#,
    );
    for (caller, is_const) in [("mutable_pointee", false), ("const_pointee", true)] {
        let targets: Vec<_> = a
            .call_edges
            .iter()
            .filter(|edge| {
                p.symbols.function(edge.caller).name == caller
                    && p.symbols.function(edge.callee).name == "take"
            })
            .map(|edge| {
                p.symbols.function(edge.callee).parameter_references[0]
                    .unwrap()
                    .pointee_const
            })
            .collect();
        assert_eq!(targets, [Some(is_const)], "{caller}");
    }
}

#[test]
fn local_move_import_hides_a_global_variable() {
    let (p, a) = analyze(
        r#"
int move;
int* out;
void run(int* p) {
    using std::move;
    out = move(p);
}
"#,
    );
    assert_edge(&p, &a, "run", "std::move");
    assert!(p
        .flow
        .iter()
        .any(|flow| matches!(flow, FlowConstraint::Copy { dst, src }
        if p.symbols.variable(*dst).name == "out" && p.symbols.variable(*src).name == "p")));
}

#[test]
fn wrong_arity_adl_overloads_keep_the_standard_identity() {
    let (p, a) = analyze(
        r#"
namespace app {
    struct T {};
    T* move(T*&, int);
    T* forward(T*&, int);
}
app::T* moved;
app::T* forwarded;
void run(app::T* src) {
    using std::move;
    using std::forward;
    moved = move(src);
    forwarded = forward<app::T*>(src);
}
"#,
    );
    assert_edge(&p, &a, "run", "std::move");
    assert_edge(&p, &a, "run", "std::forward");
    for name in ["moved", "forwarded"] {
        assert!(p
            .flow
            .iter()
            .any(|flow| matches!(flow, FlowConstraint::Copy { dst, src }
            if p.symbols.variable(*dst).name == name && p.symbols.variable(*src).name == "src")));
    }
}

#[test]
fn unrelated_adl_pointer_parameter_keeps_the_standard_move_identity() {
    let (p, a) = analyze(
        r#"
namespace app {
    struct T {};
    struct U {};
    T* move(U*);
}
app::T* out;
void run(app::T* in) {
    using std::move;
    out = move(in);
}
"#,
    );
    assert_edge(&p, &a, "run", "std::move");
    assert!(!has_any_edge(&p, &a, "run", "app::move"));
    assert!(p
        .flow
        .iter()
        .any(|flow| matches!(flow, FlowConstraint::Copy { dst, src }
        if p.symbols.variable(*dst).name == "out" && p.symbols.variable(*src).name == "in")));
}

#[test]
fn namespace_aliases_expand_each_qualified_component() {
    let (p, a) = analyze(
        r#"
struct Q { void f() {} };
namespace outer { namespace alias = std; }
namespace other = outer;
void qualified(Q& q) { other::alias::move(q).f(); }
void imported(Q& q) { using ::other::alias::forward; forward<Q&>(q).f(); }
"#,
    );
    assert_edge(&p, &a, "qualified", "std::move");
    assert_edge(&p, &a, "imported", "std::forward");
    for caller in ["qualified", "imported"] {
        assert_edge(&p, &a, caller, "Q::f");
    }
}

#[test]
fn moving_scalar_literals_preserves_their_argument_type() {
    let (p, a) = analyze(
        r#"
void take(int&);
void take(int&&);
void qualified() { take(std::move(1)); }
void imported() { using std::move; take(move(1)); }
"#,
    );
    for caller in ["qualified", "imported"] {
        assert_reference_target(&p, &a, caller, "take", true);
    }
}

#[test]
fn pointer_values_and_references_have_distinct_declarations() {
    let (p, _) = analyze(
        r#"
struct T {};
void one(T*);
void one(T*&&);
void two(T*&, T*);
void two(T*&, T*&&);
void one(T*&&) {}
void two(T*&, T*&&) {}
"#,
    );
    for (name, slot) in [("one", 0), ("two", 1)] {
        let functions: Vec<_> = p
            .symbols
            .functions
            .iter()
            .filter(|f| f.name == name)
            .collect();
        assert_eq!(functions.len(), 2, "{functions:?}");
        let defined = functions.iter().find(|f| f.is_defined).unwrap();
        assert!(defined.parameter_references[slot].unwrap().rvalue);
        let declared = functions.iter().find(|f| !f.is_defined).unwrap();
        assert!(declared.parameter_references.get(slot).is_none());
    }
}

#[test]
fn member_edges_rank_known_arguments_alongside_unknown_arguments() {
    let (p, a) = analyze(
        r#"
struct T {};
struct C {
    void f(T&&, int, int) & {}
    void f(T&&, int, double) & {}
    void implicit(T& t) { f(std::move(t), opaque(), 5); }
    void qualified(T& t) { C::f(std::move(t), opaque(), 5); }
};
void explicit_receiver(C& c, T& t) { c.f(std::move(t), opaque(), 5); }
"#,
    );
    for caller in ["explicit_receiver", "C::implicit", "C::qualified"] {
        let lines: Vec<_> = a
            .call_edges
            .iter()
            .filter_map(|edge| {
                let f = p.symbols.function_by_id(edge.callee)?;
                (p.symbols.function(edge.caller).name == caller && f.name == "C::f")
                    .then_some(f.span.line)
            })
            .collect();
        assert_eq!(lines, [4], "{caller}");
    }
}

#[test]
fn copy_initialization_does_not_emit_bodyless_constructor_calls() {
    let (p, a) = analyze(
        r#"
struct T {
    T(const T&) {}
    T(T&&) = default;
};
struct U {
    U(const U&) = default;
    U(U&&) {}
};
void defaulted_move(T& x) { T y = std::move(x); }
void written_copy(T& x) { T y = x; }
void defaulted_copy(U& x) { U y = x; }
void written_move(U& x) { U y = std::move(x); }
"#,
    );
    for (caller, ctor) in [("defaulted_move", "T::T"), ("defaulted_copy", "U::U")] {
        assert!(!has_any_edge(&p, &a, caller, ctor));
        assert!(!p
            .symbols
            .call_sites
            .iter()
            .any(|s| { p.symbols.function(s.caller).name == caller && s.callee_name == ctor }));
    }
    assert_reference_target(&p, &a, "written_copy", "T::T", false);
    assert_reference_target(&p, &a, "written_move", "U::U", true);
}

#[test]
fn reference_parameters_preserve_alias_qualification() {
    let (p, a) = analyze(
        r#"
using I = int;
using CI = const int;
using Ptr = int*;
using CPtr = int* const;
void take(I&) {}
void take(const I&) {}
void same(I&);
void same(CI&) {}
void pointer(Ptr&) {}
void pointer(CPtr&) {}
void nested(Ptr*&) {}
void run(I& x, const I& cx, Ptr p, CPtr cp) {
    take(std::forward<I&>(x));
    take(std::forward<const I&>(cx));
    pointer(std::forward<Ptr&>(p));
    pointer(std::forward<CPtr&>(cp));
}
"#,
    );
    for name in ["take", "same", "pointer"] {
        let mut qualifiers: Vec<_> = p
            .symbols
            .functions
            .iter()
            .filter(|f| f.name == name)
            .map(|f| f.parameter_references[0].unwrap().is_const)
            .collect();
        qualifiers.sort();
        assert_eq!(qualifiers, [Some(false), Some(true)], "{name}");
    }
    for (line, expected) in [(14, false), (15, true), (16, false), (17, true)] {
        let form: Vec<_> = a
            .call_edges
            .iter()
            .filter_map(|edge| {
                let site = p.symbols.call_site_by_id(edge.call_site)?;
                let f = p.symbols.function_by_id(edge.callee)?;
                (site.span.line == line && matches!(f.name.as_str(), "take" | "pointer"))
                    .then(|| f.parameter_references[0].unwrap().is_const)
            })
            .collect();
        assert_eq!(form, [Some(expected)], "line {line}");
    }
    let nested = p
        .symbols
        .functions
        .iter()
        .find(|f| f.name == "nested")
        .unwrap();
    assert_eq!(nested.parameter_references[0].unwrap().pointee_const, None);
}

#[test]
fn forwarding_with_decltype_of_auto_rvalue_reference_uses_collapsed_binding() {
    let (p, a) = analyze(
        r#"
struct T {
    T(const T&) {}
    T(T&&) {}
    void f() & {}
    void f() && {}
};
void sink(T&) {}
void sink(T&&) {}
void from_lvalue(T& t) {
    auto&& r = t;
    std::forward<decltype(r)>(r).f();
    sink(std::forward<decltype(r)>(r));
    T copy = std::forward<decltype(r)>(r);
}
void from_rvalue(T& t) {
    auto&& r = std::move(t);
    std::forward<decltype(r)>(r).f();
    sink(std::forward<decltype(r)>(r));
    T moved = std::forward<decltype(r)>(r);
}
"#,
    );
    for (caller, rvalue) in [("from_lvalue", false), ("from_rvalue", true)] {
        assert_reference_target(&p, &a, caller, "sink", rvalue);
        assert_reference_target(&p, &a, caller, "T::T", rvalue);
        let categories: Vec<_> = a
            .call_edges
            .iter()
            .filter_map(|edge| {
                let f = p.symbols.function_by_id(edge.callee)?;
                (p.symbols.function(edge.caller).name == caller && f.name == "T::f")
                    .then_some(f.ref_qualifier)
            })
            .collect();
        assert_eq!(categories, [Some(rvalue)], "{caller}");
    }
}

#[test]
fn respelled_internal_declarations_only_match_their_own_reference_overload() {
    let (p, a) = analyze(
        r#"
struct T {};
static void take(const T&);
static void take(T&&) {}
void run(const T& t) { take(std::forward<const T&>(t)); }
"#,
    );
    let overloads: Vec<_> = p
        .symbols
        .functions
        .iter()
        .filter(|f| f.name == "take")
        .collect();
    assert_eq!(overloads.len(), 2, "{overloads:?}");
    assert_reference_target(&p, &a, "run", "take", false);
}

#[test]
fn template_reference_parameters_shadow_outer_type_aliases() {
    let (p, _) = analyze(
        r#"
using P = const int;
using R = int&;
template<class P> void same(P&);
template<class Q> void same(Q&) {}
template<class R> void move_only(R&&);
template<class Q> void move_only(Q&&) {}
"#,
    );
    for (name, rvalue) in [("same", false), ("move_only", true)] {
        let overloads: Vec<_> = p
            .symbols
            .functions
            .iter()
            .filter(|f| f.name == name)
            .collect();
        assert_eq!(overloads.len(), 1, "{overloads:?}");
        assert!(overloads[0].is_defined);
        assert_eq!(overloads[0].parameter_references[0].unwrap().rvalue, rvalue);
    }
}

#[test]
fn implicitly_deleted_defaulted_move_preserves_copy_edge() {
    let (p, a) = analyze(
        r#"
struct Member {
    Member(const Member&) {}
    Member(Member&&) = delete;
};
struct Owner {
    Member items[2];
    Owner(const Owner& other) : items{other.items[0], other.items[1]} {}
    Owner(Owner&&) = default;
};
struct Derived : Member {
    Derived(const Derived& other) : Member(other) {}
    Derived(Derived&&) = default;
};
struct Simple { int value; };
struct Movable {
    Simple member;
    Movable(const Movable&) {}
    Movable(Movable&&) = default;
};
struct Deleted {
    Member member;
    Deleted(const Deleted& other) : member(other.member) {}
    Deleted(Deleted&&) = delete;
};
void copy_member(Owner& owner) { Owner dst = std::move(owner); }
void copy_base(Derived& derived) { Derived dst = std::move(derived); }
void simple_move(Movable& movable) { Movable dst = std::move(movable); }
// Keep the explicitly deleted overload in selection for this ill-formed call.
void explicit_delete(Deleted& deleted) { Deleted dst = std::move(deleted); }
"#,
    );
    assert_reference_target(&p, &a, "copy_member", "Owner::Owner", false);
    assert_reference_target(&p, &a, "copy_base", "Derived::Derived", false);
    assert!(!has_any_edge(&p, &a, "simple_move", "Movable::Movable"));
    assert!(!has_any_edge(&p, &a, "explicit_delete", "Deleted::Deleted"));
}

#[test]
fn move_assignment_can_delete_a_subobjects_implicit_constructors() {
    let (p, a) = analyze(
        r#"
struct Member { Member& operator=(Member&&) = default; };
struct Owner {
    Member member;
    Owner(const Owner&) {}
    Owner(Owner&&) = default;
};
void copy(Owner& owner) { Owner dst = std::move(owner); }
"#,
    );
    assert_reference_target(&p, &a, "copy", "Owner::Owner", false);
}

#[test]
fn defaulted_move_checks_each_shared_subobject_layout_once() {
    let mut source = String::from("struct N0 { char data; };\n");
    for depth in 1..=28 {
        let child = depth - 1;
        source.push_str(&format!("struct N{depth} {{ N{child} left, right; }};\n"));
    }
    source.push_str(
        "struct Owner {
            N28 member;
            Owner(const Owner&) {}
            Owner(Owner&&) = default;
        };
        void run(Owner& owner) { Owner dst = std::move(owner); }",
    );
    let started = std::time::Instant::now();
    let (p, a) = analyze(&source);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "repeated subobject layout checks took {:?}",
        started.elapsed()
    );
    assert_edge(&p, &a, "run", "std::move");
    assert!(!has_any_edge(&p, &a, "run", "Owner::Owner"));
}

#[test]
fn const_subobjects_need_a_viable_copy_to_use_a_defaulted_move() {
    let (p, a) = analyze(
        r#"
int value;
struct Member { int&& reference; };
struct Owner {
    const Member member;
    Owner(const Owner&) : member{static_cast<int&&>(value)} {}
    Owner(Owner&&) = default;
};
struct Movable {
    Member member;
    Movable(const Movable&) : member{static_cast<int&&>(value)} {}
    Movable(Movable&&) = default;
};
void copy(Owner& owner) { Owner dst = std::move(owner); }
void move(Movable& owner) { Movable dst = std::move(owner); }
"#,
    );
    assert_reference_target(&p, &a, "copy", "Owner::Owner", false);
    assert!(!has_any_edge(&p, &a, "move", "Movable::Movable"));
}
