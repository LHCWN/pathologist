use super::cpp_review_257::verify;
use crate::common::*;

#[test]
fn global_data_member_owners_preserve_declarations_and_value_flow() {
    let root = scratch(&[(
        "main.cpp",
        r#"
int value;
struct C { int *p; };
int * ::C::* field = &C::p;
using DM = int * ::C::*;
DM alias = &C::p;
void consume(int *p) {}
void caller(C *c) {
    c->*field = &value;
    consume(c->*alias);
}
"#,
    )]);
    let program = trace_parse::build_program(root.path(), &default_opts(root.path())).unwrap();
    assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
    let (pag, analysis) = trace_analysis::analyze_with_options(
        &program,
        trace_analysis::AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    assert!(has_any_edge(&program, &analysis, "caller", "consume"));
    for name in ["field", "alias"] {
        let variable = program
            .symbols
            .variables
            .iter()
            .find(|v| v.name == name)
            .unwrap();
        assert!(
            program
                .types
                .get(variable.type_id)
                .desc
                .member_pointer()
                .is_some(),
            "{name}"
        );
    }
    let consume = only_function(&program, "consume");
    let site = program
        .symbols
        .call_sites
        .iter()
        .find(|s| s.callee_fn_id == Some(consume))
        .unwrap();
    let actual = site.var_args[0].1;
    assert_eq!(
        points_to_names_of_var(&program, &pag, &analysis, actual),
        ["value"]
    );
}

#[test]
fn arrow_star_result_types_follow_receiver_cv_and_reference_qualifiers() {
    let (program, analysis) = verify(
        r#"
struct A { void go() {} }; struct B { void go() {} };
struct C { void go() {} }; struct D { void go() {} };
struct Proxy {
    A operator->*(int) & { return {}; }
    B operator->*(int) && { return {}; }
    C operator->*(int) const & { return {}; }
    D operator->*(int) const && { return {}; }
};
void lvalue(Proxy &p) { (p->*0).go(); }
void rvalue() { (Proxy{}->*0).go(); }
void const_lvalue(const Proxy &p) { (p->*0).go(); }
void const_rvalue(const Proxy &p) { (static_cast<const Proxy&&>(p)->*0).go(); }
"#,
        &[
            ("lvalue", "A::go"),
            ("rvalue", "B::go"),
            ("const_lvalue", "C::go"),
            ("const_rvalue", "D::go"),
        ],
    );
    for (caller, owner, is_const, ref_qualifier) in [
        ("lvalue", "A::go", false, false),
        ("rvalue", "B::go", false, true),
        ("const_lvalue", "C::go", true, false),
        ("const_rvalue", "D::go", true, true),
    ] {
        let targets: Vec<_> = analysis
            .call_edges
            .iter()
            .filter(|edge| program.symbols.function(edge.caller).name == caller)
            .map(|edge| program.symbols.function(edge.callee))
            .collect();
        assert!(
            targets.iter().any(|f| f.name == owner),
            "{caller}: {targets:?}"
        );
        let operators: Vec<_> = targets
            .iter()
            .filter(|f| f.name == "Proxy::operator->*")
            .collect();
        assert_eq!(operators.len(), 1, "{caller}: {operators:?}");
        assert_eq!(operators[0].is_const_member, is_const, "{caller}");
        assert_eq!(operators[0].ref_qualifier, Some(ref_qualifier), "{caller}");
        assert!(
            targets
                .iter()
                .filter(|f| f.name.ends_with("::go"))
                .all(|f| f.name == owner),
            "{caller}: {targets:?}"
        );
    }
}

#[test]
fn deferred_defaults_preserve_member_pointer_spelling_and_returns() {
    let header = r#"
int value;
struct C { int *get() { return &value; } };
C obj; C *ptr = &obj;
void take(int *p = (obj.*&C::get)()) {}
void arrow(int *p = (ptr->*&C::get)()) {}
"#;
    let root = scratch(&[
        ("api.hpp", header),
        ("main.cpp", "#include \"api.hpp\"\nvoid caller() { take(); arrow(); }\nvoid explicit_argument() { take((obj.*&C::get)()); }\n"),
    ]);
    for jobs in [1, 3] {
        let program =
            trace_parse::build_program_with_jobs(root.path(), &default_opts(root.path()), jobs)
                .unwrap();
        assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
        let (pag, analysis) = trace_analysis::analyze_with_options(
            &program,
            trace_analysis::AnalyzeOptions {
                retain_points_to: true,
                ..Default::default()
            },
        );
        for caller in ["caller", "explicit_argument"] {
            assert!(
                has_any_edge(&program, &analysis, caller, "C::get"),
                "jobs={jobs}: {caller}"
            );
        }
        for (name, spelling) in [("take", "(obj.*&C::get)()"), ("arrow", "(ptr->*&C::get)()")] {
            let fid = only_function(&program, name);
            let defaults = &program.default_arguments[&fid];
            assert!(
                defaults.iter().any(|(_, expr)| expr
                    .expression
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>()
                    == spelling),
                "{name}: {defaults:?}"
            );
            let caller = only_function(&program, "caller");
            let actual = program
                .symbols
                .call_sites
                .iter()
                .find(|site| site.caller == caller && site.callee_fn_id == Some(fid))
                .unwrap()
                .var_args[0]
                .1;
            assert_eq!(
                points_to_names_of_var(&program, &pag, &analysis, actual),
                ["value"],
                "jobs={jobs}: {name}"
            );
            let line = header
                .lines()
                .position(|line| line.starts_with(&format!("void {name}(")))
                .unwrap() as u32
                + 1;
            assert!(
                analysis.call_edges.iter().any(|edge| {
                    let site = &program.symbols.call_sites[edge.call_site.0 as usize];
                    program.symbols.function(edge.caller).name == "caller"
                        && program.symbols.function(edge.callee).name == "C::get"
                        && site.span.line == line
                        && program.symbols.files[site.span.file.0 as usize]
                            .path
                            .ends_with("api.hpp")
                }),
                "jobs={jobs}: {name} retains the default's source position"
            );
        }
    }
}

#[test]
fn inferred_static_member_addresses_are_ordinary_callbacks() {
    for declaration in [
        "struct C { static void f(int) {} static void zero() {} };",
        "struct C { static void f(int); static void zero(); }; void C::f(int) {} void C::zero() {}",
        "struct C { static void f(int); static void zero(); };",
    ] {
        verify(
            &format!(
                r#"
{declaration}
struct V {{ static void f(int, ...) {{}} }};
void caller() {{ auto p = &C::f; p(1); }}
void zero() {{ auto p = &C::zero; p(); }}
void typed() {{ void (*p)(int) = &C::f; p(1); }}
void variadic() {{ auto p = &V::f; p(1,2); }}
"#
            ),
            &[
                ("caller", "C::f"),
                ("zero", "C::zero"),
                ("typed", "C::f"),
                ("variadic", "V::f"),
            ],
        );
    }
}

#[test]
fn inferred_callable_signatures_restore_reference_returns() {
    verify(
        r#"
int value;
struct C {
    int& ref() { return value; }
    int&& move_ref() { return static_cast<int&&>(value); }
    static int& static_ref() { return value; }
    int& var_ref(int, ...) { return value; }
};
int& free_ref() { return value; }
void arrow(C *c) { auto p = &C::ref; (c->*p)(); }
void dot(C& c) { auto p = &C::ref; (c.*p)(); }
void rvalue(C *c) { auto p = &C::move_ref; (c->*p)(); }
void ordinary() { auto p = &C::static_ref; p(); }
void free_function() { auto p = &free_ref; p(); }
void variadic(C *c) { auto p = &C::var_ref; (c->*p)(1,2); }
"#,
        &[
            ("arrow", "C::ref"),
            ("dot", "C::ref"),
            ("rvalue", "C::move_ref"),
            ("ordinary", "C::static_ref"),
            ("free_function", "free_ref"),
            ("variadic", "C::var_ref"),
        ],
    );
}

#[test]
fn reference_returning_callable_signatures_keep_their_reference_layer() {
    let (program, analysis) = verify(
        r#"
int value;
struct C {
    static int& cb() { return value; }
    static int& cb(int) { return value; }
    int& member() { return value; }
    int& member(int) { return value; }
    int&& rvalue() { return static_cast<int&&>(value); }
};
using F = int&(*)(); using M = int&(C::*)();
struct Use { void call(F f) { f(); } };
void argument(Use& u) { u.call(&C::cb); }
void ordinary() { F f = &C::cb; f(); }
void member(C& c) { M f = &C::member; (c.*f)(); }
void stored() { F f; F *p = &f; *p = &C::cb; f(); }
void rvalue(C& c) { using R = int&&(C::*)(); R f = &C::rvalue; (c.*f)(); }
void cast_ordinary() { using V = int(*)(); V f = reinterpret_cast<V>(static_cast<F>(&C::cb)); reinterpret_cast<F>(f)(); }
void incompatible() { using V = int(*)(); V f = reinterpret_cast<int(*)()>(static_cast<F>(&C::cb)); f(); }
void incompatible_member(C& c) { using V = int(C::*)(); V f = reinterpret_cast<int(C::*)()>(static_cast<M>(&C::member)); (c.*f)(); }
void cast_member(C& c) { using V = int(C::*)(); V f = reinterpret_cast<int(C::*)()>(static_cast<M>(&C::member)); (c.*reinterpret_cast<M>(f))(); }
"#,
        &[
            ("ordinary", "C::cb"),
            ("member", "C::member"),
            ("stored", "C::cb"),
            ("rvalue", "C::rvalue"),
            ("cast_ordinary", "C::cb"),
            ("cast_member", "C::member"),
            ("Use::call", "C::cb"),
        ],
    );
    assert!(has_any_edge(&program, &analysis, "incompatible", "C::cb"));
    assert!(has_any_edge(
        &program,
        &analysis,
        "incompatible_member",
        "C::member"
    ));
    for caller in ["ordinary", "member", "stored"] {
        let targets: Vec<_> = analysis
            .call_edges
            .iter()
            .filter(|e| program.symbols.function(e.caller).name == caller)
            .collect();
        assert_eq!(targets.len(), 1, "{caller}: {targets:?}");
        assert!(program
            .symbols
            .explicit_params(program.symbols.function(targets[0].callee))
            .unwrap()
            .is_empty());
    }
}

#[test]
fn returned_reinterpreted_addresses_survive_callback_storage() {
    let (program, analysis) = verify(
        r#"
using F = void(*)(int,int); using G = void(*)(int);
struct C { static void target(int,int) {} };
void free_target(int,int) {}
G erase() { return reinterpret_cast<G>(&C::target); }
G erase_free() { return reinterpret_cast<G>(&free_target); }
G erase_cstyle() { return (void (*)(int))&C::target; }
void put(G *out) { *out = erase(); }
void put_free(G *out) { *out = erase_free(); }
void put_cstyle(G *out) { *out = erase_cstyle(); }
void restored() { G g; put(&g); F f = reinterpret_cast<F>(g); f(1,2); }
void restored_free() { G g; put_free(&g); F f = reinterpret_cast<F>(g); f(1,2); }
void restored_cstyle() { G g; put_cstyle(&g); F f = reinterpret_cast<F>(g); f(1,2); }
void incompatible() { G g; put(&g); g(1); }
"#,
        &[
            ("restored", "C::target"),
            ("restored_free", "free_target"),
            ("restored_cstyle", "C::target"),
        ],
    );
    assert!(!has_any_edge(
        &program,
        &analysis,
        "incompatible",
        "C::target"
    ));
}

#[test]
fn opaque_function_values_survive_local_and_forwarded_arguments() {
    for extension in ["c", "cpp"] {
        let source = r#"
typedef void (*F)(int); void good(int n) {}
void call(void *v) { ((F)v)(1); }
void forward(void *v) { call(v); }
void initialized(void) { void *a = (void *)good; forward(a); }
void assigned(void) { void *a; a = (void *)good; forward(a); }
void put(void **p, void *v) { *p = v; }
void stored(void) { F f; void *a = (void *)good; put((void **)&f, a); f(1); }
"#;
        let root = scratch(&[(&format!("main.{extension}"), source)]);
        let program = trace_parse::build_program(root.path(), &default_opts(root.path())).unwrap();
        let (_, analysis) = trace_analysis::analyze(&program);
        for caller in ["call", "stored"] {
            assert!(
                has_any_edge(&program, &analysis, caller, "good"),
                "{extension}: {caller}"
            );
        }
    }
}

#[test]
fn conversion_addresses_stop_at_template_argument_boundaries() {
    verify(
        r#"
struct C { explicit operator bool() const { return true; } bool ok() const { return true; } };
template<bool (C::*M)() const> struct Probe { void run() {} };
void conversion() { Probe<&C::operator bool> probe; probe.run(); }
void named() { Probe<&C::ok> probe; probe.run(); }
"#,
        &[("conversion", "Probe::run"), ("named", "Probe::run")],
    );
}

#[test]
fn static_callback_selection_covers_bare_names_stores_and_arguments() {
    for declarations in [
        "static void f() {} static void f(int) {}",
        "static void f(int) {} static void f() {}",
    ] {
        let source = format!(
            r#"
struct C {{ {declarations} }}; using F=void(*)(); struct H{{ F cb; }};
void bare(){{ F f=C::f; f(); }}
void field(H*h){{ h->cb=&C::f; h->cb(); }}
void take(F f){{ f(); }} void argument(){{ take(&C::f); }}
"#
        );
        let (p, a) = verify(
            &source,
            &[("bare", "C::f"), ("field", "C::f"), ("take", "C::f")],
        );
        for caller in ["bare", "field", "take"] {
            let edges: Vec<_> = a
                .call_edges
                .iter()
                .filter(|e| p.symbols.function(e.caller).name == caller)
                .collect();
            assert_eq!(edges.len(), 1, "{caller}: {edges:?}");
            assert_eq!(
                p.symbols
                    .explicit_params(p.symbols.function(edges[0].callee))
                    .unwrap()
                    .len(),
                0
            );
        }
    }
}

#[test]
fn free_arrow_star_uses_ordinary_scope_and_imports() {
    verify(
        r#"
using F=void(*)(); void alpha(){} namespace ns { struct P{}; }
namespace ops { F operator->*(ns::P,int){return alpha;} void local(ns::P p){(p->*0)();} }
void declared(ns::P p){using ops::operator->*; (p->*0)();}
void directive(ns::P p){using namespace ops; (p->*0)();}
"#,
        &[
            ("ops::local", "ops::operator->*"),
            ("ops::local", "alpha"),
            ("declared", "alpha"),
            ("directive", "alpha"),
        ],
    );
}

#[test]
fn non_type_template_arguments_compare_integral_values() {
    for (left, right) in [("2", "02"), ("2", "0x2u"), ("1", "true"), ("0", "false")] {
        verify(
            &format!(
                r#"
template<int N,class T=int> struct Box{{T*get();}};
struct C{{void hit(Box<{left}>*){{}}}}; using M=void(C::*)(Box<{right}>*);
M m=&C::hit; void caller(C*p){{(p->*m)(nullptr);}}
"#
            ),
            &[("caller", "C::hit")],
        );
    }
}

#[test]
fn returned_functors_resolve_their_call_operator() {
    verify(
        r#"
struct Fun { void operator()() {} }; struct P { Fun operator->*(int) { return {}; } };
void caller(P p){(p->*0)();} void control(P p){Fun f=p->*0; f();}
"#,
        &[
            ("caller", "P::operator->*"),
            ("caller", "Fun::operator()"),
            ("control", "Fun::operator()"),
        ],
    );
}

#[test]
fn returned_data_member_pointer_keeps_its_category() {
    verify(
        r#"
using F=void(*)(); void alpha(){} struct C{F f;}; F C::*get(){return &C::f;}
void caller(C*p){p->f=alpha;(p->*get())();}
"#,
        &[("caller", "get"), ("caller", "alpha")],
    );
}

#[test]
fn enum_operands_participate_in_arrow_star_lookup() {
    verify(
        r#"
using F=void(*)(); void alpha(){} struct C{}; enum class Tag{go};
F operator->*(C*,Tag){return alpha;} void caller(C*p){(p->*Tag::go)();}
"#,
        &[("caller", "operator->*"), ("caller", "alpha")],
    );
}

#[test]
fn lone_free_operator_must_accept_its_right_operand() {
    let (p, a) = verify(
        r#"
using F=void(*)(); void alpha(){} struct C{void a(){}}; using M=void(C::*)(); struct Tag{};
F operator->*(C*,Tag){return alpha;} struct Holder{operator M()const{return &C::a;}};
void tagged(C*p){(p->*Tag{})();} void held(C*p,Holder h){(p->*h)();}
"#,
        &[("tagged", "alpha")],
    );
    assert!(!has_any_edge(&p, &a, "held", "operator->*"));
    assert!(!has_any_edge(&p, &a, "held", "alpha"));
}

#[test]
fn member_pointer_slots_keep_persistent_forwarding_flow() {
    verify(
        r#"
struct C{void hit(){}}; using M=void(C::*)();
void set(M*out){*out=&C::hit;} void helper(M*out){set(out);}
void caller(C*p){M m;helper(&m);(p->*m)();}
"#,
        &[("caller", "C::hit")],
    );
}

#[test]
fn overloads_take_precedence_over_data_member_binding() {
    verify(
        r#"
using F=void(*)(); void alpha(){} void beta(){} struct S{F cb;}; F S::*pm=&S::cb;
struct Proxy{F operator->*(F S::*){return beta;}};
void init(Proxy p){F f=p->*pm;f();} void use(F f){f();} void argument(Proxy p){use(p->*pm);}
"#,
        &[("init", "beta"), ("use", "beta")],
    );
}

#[test]
fn implicit_arrow_star_in_sizeof_is_unevaluated() {
    let (p, a) = verify(
        r#"
struct C{int operator->*(int){return 0;}};
void never(C c){(void)sizeof(c->*1);} void runs(C c){(void)(c->*1);}
"#,
        &[("runs", "C::operator->*")],
    );
    assert!(!has_any_edge(&p, &a, "never", "C::operator->*"));
    let id = only_function(&p, "never");
    assert!(p
        .symbols
        .call_sites
        .iter()
        .filter(|s| s.caller == id)
        .all(|s| s.unevaluated));
}

#[test]
fn reinterpreted_return_types_retain_stored_passed_and_local_targets() {
    for extension in ["c", "cpp"] {
        let root = scratch(&[(
            &format!("main.{extension}"),
            r#"
typedef int (*Entry)(void *);
void thread_main(void *arg) { (void)arg; }
void thread_main2(void *arg) { (void)arg; }
void thread_main3(void *arg) { (void)arg; }
int thread_main_int(void *arg) { (void)arg; return 0; }
struct Wrapper { Entry entry; };
static struct Wrapper g;
void create(Entry e) { g.entry = e; }
void run(void) { g.entry(0); }
void call(Entry e) { e(0); }
void stored(void) { create((Entry)thread_main); }
void passed(void) { call((Entry)thread_main2); }
void local(void) { Entry e = (Entry)thread_main3; e(0); }
void control(void) { create(thread_main_int); }
"#,
        )]);
        let program = trace_parse::build_program(root.path(), &default_opts(root.path())).unwrap();
        assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
        let (_, analysis) = trace_analysis::analyze(&program);
        for (caller, target) in [
            ("run", "thread_main"),
            ("call", "thread_main2"),
            ("local", "thread_main3"),
            ("run", "thread_main_int"),
        ] {
            assert!(
                has_any_edge(&program, &analysis, caller, target),
                "{extension}: {caller} -> {target}"
            );
        }
    }
    verify(
        r#"
struct C { void hit() {} };
using M = int (C::*)();
struct Wrapper { M entry; };
void caller(C *p) { M m = reinterpret_cast<M>(&C::hit); (p->*m)(); }
void passed(C *p, M m) { (p->*m)(); }
void pass(C *p) { passed(p, reinterpret_cast<M>(&C::hit)); }
void stored(C *p, Wrapper *w) { w->entry = reinterpret_cast<M>(&C::hit); (p->*w->entry)(); }
"#,
        &[
            ("caller", "C::hit"),
            ("passed", "C::hit"),
            ("stored", "C::hit"),
        ],
    );
}

#[test]
fn zero_argument_pointer_slots_reject_known_nonzero_arity_stores() {
    let (program, analysis) = verify(
        r#"
void cb(int, int) {}
void zero() {}
struct C { static void zero() {} static void one(int) {} };
void staticZero(void (**pp)()) { *pp = C::zero; }
void staticOne(void (**pp)(int)) { *pp = C::one; }
void useStaticZero() { void (*p)(); staticZero(&p); p(); }
void useStaticOne() { void (*p)(int); staticOne(&p); p(1); }
void viaPtr0(void (**pp)()) { *pp = cb; }
void viaPtr1(void (**pp)(int)) { *pp = cb; }
void accepted(void (**pp)()) { *pp = zero; }
void caller0() { void (*p)(); viaPtr0(&p); p(); }
void caller1() { void (*p)(int); viaPtr1(&p); p(1); }
void control() { void (*p)(); accepted(&p); p(); }
"#,
        &[
            ("control", "zero"),
            ("useStaticZero", "C::zero"),
            ("useStaticOne", "C::one"),
        ],
    );
    for caller in ["caller0", "caller1"] {
        assert!(!has_any_edge(&program, &analysis, caller, "cb"), "{caller}");
    }
}

#[test]
fn coarse_callback_fields_admit_all_arities_in_gep_flow() {
    for extension in ["c", "cpp"] {
        let root = scratch(&[(
            &format!("main.{extension}"),
            r#"
struct S { void (*cb)(); };
struct S zero(void) { struct S s = {0}; return s; }
struct S one(int x) { struct S s = {0}; return s; }
struct S two(int x, int y) { struct S s = {0}; return s; }
void use(struct S *p) { p->cb(); }
void source(void) { use((struct S *)zero); use((struct S *)one); use((struct S *)two); }
"#,
        )]);
        let program = trace_parse::build_program(root.path(), &default_opts(root.path())).unwrap();
        assert!(
            !program.diagnostics.iter().any(|d| d.stage == "parse"),
            "{:?}",
            program.diagnostics
        );
        let (_, analysis) = trace_analysis::analyze(&program);
        for target in ["zero", "one", "two"] {
            assert_eq!(
                has_any_edge(&program, &analysis, "use", target),
                extension == "c" || target == "zero",
                "{extension}: {target}"
            );
        }
    }
}

#[test]
fn prototype_only_targets_use_declared_arity_in_gep_fields() {
    let root = scratch(&[(
        "main.cpp",
        r#"
struct S { void (*cb)(); };
struct S zero(void);
struct S one(int);
struct T { void (*cb)(int); };
struct T zero_t(void);
struct T one_t(int);
void use_zero(struct S *p) { p->cb(); }
void use_one(struct T *p) { p->cb(1); }
void source(void) { use_zero((struct S *)zero); use_zero((struct S *)one); use_one((struct T *)zero_t); use_one((struct T *)one_t); }
"#,
    )]);
    let mut program = trace_parse::build_program(root.path(), &default_opts(root.path())).unwrap();
    assert!(!program.diagnostics.iter().any(|d| d.stage == "parse"));
    // Model the declaration-only form retained without lowered parameter
    // variables: signature metadata still knows the explicit arity.
    for name in ["zero", "one", "zero_t", "one_t"] {
        let f = program
            .symbols
            .functions
            .iter()
            .find(|f| f.name == name)
            .unwrap();
        assert!(!f.is_defined);
        let id = f.id;
        let signature = program.symbols.explicit_params(f).unwrap();
        let types = (0..signature.len())
            .map(|i| signature.get(i).unwrap())
            .collect();
        let f = program
            .symbols
            .functions
            .iter_mut()
            .find(|f| f.id == id)
            .unwrap();
        f.param_type_ids = types;
        f.params.clear();
    }
    let (pag, analysis) = trace_analysis::analyze_with_options(
        &program,
        trace_analysis::AnalyzeOptions {
            retain_points_to: true,
            ..Default::default()
        },
    );
    assert!(has_any_edge(&program, &analysis, "use_zero", "zero"));
    assert!(has_any_edge(&program, &analysis, "use_one", "one_t"));
    assert!(!has_any_edge(&program, &analysis, "use_zero", "one"));
    assert!(!has_any_edge(&program, &analysis, "use_one", "zero_t"));
    // Check the GEP's propagated values too: invocation signature filtering
    // could otherwise hide an incorrectly admitted prototype in the graph.
    for (caller, expected) in [("use_zero", "zero"), ("use_one", "one_t")] {
        let site = program
            .symbols
            .call_sites
            .iter()
            .find(|site| program.symbols.function(site.caller).name == caller)
            .unwrap();
        let node = pag.var_node[&site.callee_var.unwrap()];
        let mut targets = analysis.points_to[&node]
            .iter()
            .filter_map(|loc| pag.locations[loc.0 as usize].fn_id)
            .map(|id| program.symbols.function(id).name.clone())
            .collect::<Vec<_>>();
        targets.sort();
        targets.dedup();
        assert_eq!(targets, [expected.to_owned()]);
    }
}

#[test]
fn class_valued_member_pointer_owners_keep_distinct_overloads() {
    let (program, analysis) = verify(
        r#"
struct S {};
struct C { S s; };
struct D { S s; };
void take(S C::*);
void take(S D::* p) { (void)p; }
void use(C &c, S C::*m) { take(m); }
"#,
        &[("use", "take")],
    );
    let edges = analysis
        .call_edges
        .iter()
        .filter(|edge| program.symbols.function(edge.caller).name == "use")
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 1);
    let target = program.symbols.function(edges[0].callee);
    assert!(
        !target.is_defined,
        "must bind the C::* prototype rather than the D::* definition"
    );
    let param = program
        .symbols
        .explicit_params(target)
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(
        program
            .types
            .get(param)
            .desc
            .member_pointer()
            .map(|(owner, _)| owner),
        Some("C")
    );
}

#[test]
fn standard_integer_template_arguments_preserve_member_callback_targets() {
    verify(
        r#"
using size_t = unsigned long;
template<class T> struct Box {};
struct C { void hit(Box<unsigned long>) {} };
using M = void (C::*)(Box<size_t>);
void caller(C &c, Box<size_t> value) { M m = &C::hit; (c.*m)(value); }
"#,
        &[("caller", "C::hit")],
    );
}

#[test]
fn declaration_macro_recovery_preserves_member_pointer_operator_edges() {
    let root = scratch(&[
        ("good.cpp", "struct V { int operator->*(int); }; int ov(V v) { return 1 + v->*0; }"),
        ("withmacro.cpp", "struct W { int operator->*(int); }; FFI_EXPORT int ov2(W w) { return 1 + w->*0; } struct S { DECLARE_SINGLETON(S) public: int ov3(W w) { return 1 + w->*0; } };"),
    ]);
    let program = trace_parse::build_program(root.path(), &default_opts(root.path())).unwrap();
    assert_eq!(
        program
            .diagnostics
            .iter()
            .filter(|d| d.stage == "parse")
            .count(),
        2,
        "{:?}",
        program.diagnostics
    );
    assert!(
        program
            .diagnostics
            .iter()
            .all(|d| !d.message.contains("parse errors")),
        "{:?}",
        program.diagnostics
    );
    let (_, analysis) = trace_analysis::analyze(&program);
    for (caller, target) in [
        ("ov", "V::operator->*"),
        ("ov2", "W::operator->*"),
        ("S::ov3", "W::operator->*"),
    ] {
        assert!(
            has_any_edge(&program, &analysis, caller, target),
            "{caller} -> {target}"
        );
    }
}

#[test]
fn member_pointer_array_fields_keep_bounds_layout_and_export_names() {
    let root = scratch(&[(
        "main.cpp",
        r#"
struct C { int x; };
using M = int (C::*)(int);
constexpr int count = 2;
struct Holder { M arr[count]; int y; };
struct DataHolder { int C::* darr[2]; int y; };
struct ExtraHolder { M *arr[2]; int y; };
struct DirectHolder { void (C::*arr[2])(int); int y; };
struct MatrixHolder { M arr[2][3]; int y; };
struct PointerArrayHolder { M (*p)[2]; int y; };
struct PlainHolder { int *plain[2]; int y; };
struct StrideHolder { Holder stride[3]; };
"#,
    )]);
    let program = trace_parse::build_program(root.path(), &default_opts(root.path())).unwrap();
    assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
    let (pag, analysis) = trace_analysis::analyze(&program);
    let db = root.path().join("layout.db");
    let mut options = trace_db::ExportOptions::minimal(db.clone());
    options.full_detail = true;
    trace_db::export_to_sqlite(&program, &pag, &analysis, &options).unwrap();
    let conn = rusqlite::Connection::open(db).unwrap();
    for (class, field_name, display, field_size, class_size) in [
        ("Holder", "arr", "fn_ptr C::*[2]", 16, 24),
        ("DataHolder", "darr", "int C::*[2]", 16, 24),
        ("ExtraHolder", "arr", "fn_ptr C::**[2]", 16, 24),
        ("DirectHolder", "arr", "fn_ptr C::*[2]", 16, 24),
        ("MatrixHolder", "arr", "fn_ptr C::*[3][2]", 48, 56),
        ("PointerArrayHolder", "p", "fn_ptr C::*[2]*", 8, 16),
        ("PlainHolder", "plain", "int*[2]", 16, 24),
    ] {
        let (size, layout): (u64, String) = conn
            .query_row(
                "SELECT size,layout_json FROM types WHERE name=?1 ORDER BY size DESC LIMIT 1",
                [class],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(size, class_size, "{class}");
        let layout: serde_json::Value = serde_json::from_str(&layout).unwrap();
        let first = &layout["fields"]["0"];
        assert_eq!(first["name"], field_name, "{class}");
        assert_eq!(first["offset"], 0, "{class}");
        assert_eq!(first["size"], field_size, "{class}");
        assert_eq!(layout["fields"]["1"]["offset"], field_size, "{class}");
        let name: String = conn
            .query_row(
                "SELECT name FROM types WHERE id=?1",
                [first["type_id"].as_u64().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, display, "{class}");
    }
    let stride: u64 = conn
        .query_row("SELECT size FROM types WHERE name='Holder[3]'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stride, 72);
}

#[test]
fn reinterpretation_qualifies_loaded_function_values_before_storage() {
    verify(
        r#"
using F1 = void(*)(int); using F0 = void(*)();
void target(int) {}
struct Box { F1 slot; };
void loaded() { F1 f = target; F1 *p = &f; F0 e = reinterpret_cast<F0>(*p); F0 *q = &e; F1 r = reinterpret_cast<F1>(*q); r(1); }
void cstyle() { F1 f = target; F1 *p = &f; F0 e = (void(*)())*p; F0 *q = &e; F1 r = (void(*)(int))*q; r(1); }
void field() { Box b; b.slot = target; F0 e = reinterpret_cast<F0>(b.slot); F0 *q = &e; F1 r = reinterpret_cast<F1>(*q); r(1); }
void element() { F1 a[1] = {target}; F0 e = reinterpret_cast<F0>(a[0]); F0 *q = &e; F1 r = reinterpret_cast<F1>(*q); r(1); }
"#,
        &[
            ("loaded", "target"),
            ("cstyle", "target"),
            ("field", "target"),
            ("element", "target"),
        ],
    );
}

#[test]
fn inherited_data_member_addresses_infer_the_declaring_owner() {
    let (program, analysis) = verify(
        r#"
struct X {};
struct B { X field; };
struct D : B {};
struct E : D {};
struct H : B { X field; };
void pick(X B::*) {} void pick(X D::*) {} void pick(X H::*) {}
void inherited() { auto pm = &D::field; pick(pm); }
void deep() { auto pm = &E::field; pick(pm); }
void explicit_owner() { X D::* pm = &D::field; pick(pm); }
void hidden() { auto pm = &H::field; pick(pm); }
"#,
        &[
            ("inherited", "pick"),
            ("deep", "pick"),
            ("explicit_owner", "pick"),
            ("hidden", "pick"),
        ],
    );
    for (caller, owner) in [
        ("inherited", "B"),
        ("deep", "B"),
        ("hidden", "H"),
        ("explicit_owner", "D"),
    ] {
        let fid = only_function(&program, caller);
        let pm = program
            .symbols
            .variables
            .iter()
            .find(|v| v.fn_id == Some(fid) && v.name == "pm")
            .unwrap();
        assert_eq!(
            program
                .types
                .get(pm.type_id)
                .desc
                .member_pointer()
                .unwrap()
                .0,
            owner
        );
        let selected: Vec<_> = analysis
            .call_edges
            .iter()
            .filter(|e| e.caller == fid && program.symbols.function(e.callee).name == "pick")
            .map(|e| {
                let formals = program
                    .symbols
                    .explicit_params(program.symbols.function(e.callee))
                    .unwrap();
                program
                    .types
                    .get(formals.get(0).unwrap())
                    .desc
                    .member_pointer()
                    .unwrap()
                    .0
                    .to_owned()
            })
            .collect();
        assert_eq!(selected, [owner], "{caller}");
    }
}

#[test]
fn member_pointer_owner_dependency_survives_a_shadowed_class() {
    let (p, _) = verify(
        r#"
struct T {}; struct C { int data; };
struct A { template<class T> static void member(int T::*p) {} static void known(int T::*) {} };
void caller() { void (*cb)(int C::*) = &A::member<C>; cb(&C::data); }
"#,
        &[("caller", "A::member")],
    );
    let signature = p
        .symbols
        .variables
        .iter()
        .find(|v| v.name == "cb")
        .unwrap()
        .type_id;
    let known = only_function(&p, "A::known");
    assert!(!p.symbols.function_pointer_signature_admits(
        known,
        &p.types.get(signature).desc,
        &p.types,
        false
    ));
    let f = p.symbols.function(only_function(&p, "A::member"));
    let ty = p.symbols.explicit_params(f).unwrap().get(0).unwrap();
    assert!(matches!(
        p.types.get(ty).desc.as_ref(),
        trace_ir::TypeDesc::MemberPointer {
            owner_dependent: true,
            ..
        }
    ));
    let facts = trace_ir::LinkingFacts::collect(&p, false);
    let saved = facts
        .functions
        .iter()
        .find(|entry| entry.id == f.id)
        .unwrap()
        .params
        .as_ref()
        .unwrap();
    assert_eq!(&saved[0], p.types.get(ty).desc.as_ref());
    let mut json = serde_json::to_value(&saved[0]).unwrap();
    assert_eq!(json["MemberPointer"]["owner_dependent"], true);
    json["MemberPointer"]
        .as_object_mut()
        .unwrap()
        .remove("owner_dependent");
    let legacy: trace_ir::TypeDesc = serde_json::from_value(json).unwrap();
    assert!(matches!(
        legacy,
        trace_ir::TypeDesc::MemberPointer {
            owner_dependent: false,
            ..
        }
    ));
}

#[test]
fn reference_alias_callable_results_keep_their_referent_shape() {
    verify(
        r#"
int value; int *pointer;
using Ref = int&; using PtrRef = int*&;
using F = Ref(*)(); using G = PtrRef(*)();
struct A { static Ref get(); static PtrRef getptr(); };
Ref A::get() { return value; } PtrRef A::getptr() { return pointer; }
void caller() { F f = &A::get; G g = &A::getptr; f(); g(); }
"#,
        &[("caller", "A::get"), ("caller", "A::getptr")],
    );
}

#[test]
fn operator_arrow_star_results_preserve_cv_and_reference_category() {
    let (p, a) = verify(
        r#"
struct L { void go() {} }; struct R { void go() {} }; struct K { void go() {} };
struct S { L f() & { return {}; } R f() && { return {}; } K f() const & { return {}; } };
struct P { S&& operator->*(int); }; struct Q { const S& operator->*(int); };
void rvalue(P p) { (p->*0).f().go(); }
void constant(Q p) { (p->*0).f().go(); }
"#,
        &[("rvalue", "R::go"), ("constant", "K::go")],
    );
    for (caller, rejected) in [
        ("rvalue", "L::go"),
        ("constant", "L::go"),
        ("constant", "R::go"),
    ] {
        assert!(!has_any_edge(&p, &a, caller, rejected));
    }
}

#[test]
fn subscript_receiver_qualification_follows_its_element() {
    let (p, a) = verify(
        r#"
struct L { void go() {} }; struct K { void go() {} };
struct S { L f() { return {}; } K f() const { return {}; } };
void constant(const S& s) { (&s)[0].f().go(); }
void mutable_receiver(S& s) { (&s)[0].f().go(); }
void array_receiver(const S a[2]) { a[0].f().go(); }
"#,
        &[
            ("constant", "K::go"),
            ("mutable_receiver", "L::go"),
            ("array_receiver", "K::go"),
        ],
    );
    assert!(!has_any_edge(&p, &a, "constant", "L::go"));
    assert!(!has_any_edge(&p, &a, "mutable_receiver", "K::go"));
}
