use super::cpp_review_257::verify;
use crate::common::has_any_edge;

#[test]
fn ambiguous_data_member_cast_does_not_replace_an_arrow_star_overload_result() {
    let (program, analysis) = verify(
        r#"
using F = void (*)();
void alpha() {} void beta() {}
struct C { F f; };
using M = F C::*;
struct Proxy { F operator->*(M) { return beta; } };
void ambiguous() { C c; c.f=alpha; Proxy proxy; (proxy->*(M)&C::f)(); }
void named() { C c; c.f=alpha; Proxy proxy; (proxy->*static_cast<M>(&C::f))(); }
"#,
        &[
            ("ambiguous", "Proxy::operator->*"),
            ("named", "Proxy::operator->*"),
            ("named", "beta"),
        ],
    );
    for caller in ["ambiguous", "named"] {
        assert!(!has_any_edge(&program, &analysis, caller, "alpha"));
    }
}

#[test]
fn selected_data_member_arrays_and_subobjects_remain_callable() {
    verify(
        r#"
using F = void (*)();
void alpha() {}
struct I { F f; };
struct C { F a[2]; I i; I *p; I v[2]; };
using A = F[2];
using V = I[2];
A C::*pa = &C::a;
I C::*pi = &C::i;
I *C::*pp = &C::p;
V C::*pv = &C::v;
void array(C *p) { p->a[0] = alpha; (p->*pa)[0](); }
void object(C *p) { p->i.f = alpha; (p->*pi).f(); }
void pointer(C *p) { p->p->f = alpha; (p->*pp)->f(); }
void nested(C *p) { p->v[0].f = alpha; (p->*pv)[0].f(); }
void write(C *p) { (p->*pa)[0] = alpha; p->a[0](); }
F get(C *p) { return (p->*pi).f; }
void returned(C *p) { p->i.f = alpha; F f = get(p); f(); }
"#,
        &[
            ("array", "alpha"),
            ("object", "alpha"),
            ("pointer", "alpha"),
            ("nested", "alpha"),
            ("write", "alpha"),
            ("returned", "alpha"),
        ],
    );
}

#[test]
fn selected_callable_objects_retain_their_declared_type() {
    verify(
        r#"
struct Functor { void operator()() {} };
struct C { Functor f; };
Functor C::*pm = &C::f;
void caller(C *p) { (p->*pm)(); }
void inlined(C *p) { (p->*&C::f)(); }
"#,
        &[
            ("caller", "Functor::operator()"),
            ("inlined", "Functor::operator()"),
        ],
    );
}

#[test]
fn cast_data_member_operands_keep_data_binding_semantics() {
    verify(
        r#"
using F = void (*)();
void alpha() {}
struct C { F f; };
using M = F C::*;
M pm = &C::f;
void named(C *p) { p->f = alpha; (p->*static_cast<M>(pm))(); }
void cstyle(C *p) { p->f = alpha; (p->*(M)pm)(); }
void functional(C *p) { p->f = alpha; (p->*M(pm))(); }
void address(C *p) { p->f = alpha; (p->*static_cast<M>(&C::f))(); }
void caddress(C *p) { p->f = alpha; (p->*(M)&C::f)(); }
"#,
        &[
            ("named", "alpha"),
            ("cstyle", "alpha"),
            ("functional", "alpha"),
            ("address", "alpha"),
            ("caddress", "alpha"),
        ],
    );
}
