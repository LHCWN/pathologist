use super::cpp_review_257::verify;

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
