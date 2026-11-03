#!/usr/bin/env python3
"""Differential check of member-call result typing against a real C++ compiler.

Every case calls an overloaded member `f` whose overloads return distinct
pointer types (`R*`, `S*`, `T*`, `U*`), each with a `Good()` method. clang
reports which type `auto x = <call>` has; the analyzer's call graph must then
contain exactly the edge into that type's `Good`. Cases clang rejects are
dropped, so only valid C++ is compared.

Not covered: `decltype(expr)` declarations, whose type is deliberately left
unknown (docs/ANALYSIS.md, "C++ parse-input normalization").

Axes: overload set (cv / ref-qualifier / template / arguments) x object
declaration (parameter, alias, derived `auto`, local, field, smart pointer,
lambda capture, call result) x access expression (`.`, `->`, move, casts).

    scripts/cpp_overload_diff.py [--trace target/release/trace] [--keep DIR]
        [--filter SUBSTR] [--show N] [--volatile] [--strict]

A case is "exact" when the call graph holds only the compiler's target, "extra"
when it also holds others, "wrong" when it holds only others, and "missed" when
the receiver is left untyped and no edge is recorded. Exit status is 1 when any
case is wrong or extra; --strict also fails on missed ones (receiver forms the
analyzer does not type: ternaries, comma and assignment expressions,
lambda-call results, `C{}`, `d.Base::f()`).
"""
import argparse
import bisect
import collections
import os
import re
import sqlite3
import subprocess
import sys
import tempfile

PRELUDE = """\
namespace std {
template<class T> struct remove_reference { typedef T type; };
template<class T> struct remove_reference<T&> { typedef T type; };
template<class T> struct remove_reference<T&&> { typedef T type; };
template<class T> typename remove_reference<T>::type&& move(T&& t) noexcept;
template<class T> T&& forward(typename remove_reference<T>::type& t) noexcept;
template<class T> T&& forward(typename remove_reference<T>::type&& t) noexcept;
template<class T> struct shared_ptr {
    T* get() const; T* operator->() const; T& operator*() const;
};
template<class T> struct unique_ptr {
    T* get() const; T* operator->() const; T& operator*() const;
};
}
struct R { void Good(); }; struct S { void Good(); };
struct T { void Good(); }; struct U { void Good(); };
void R::Good() {} void S::Good() {} void T::Good() {} void U::Good() {}
template<class X> struct Show;
extern int gi;
"""

# name -> (member declarations, explicit call arguments)
SETS = {
    "cv": ("R* f(); S* f() const;", ""),
    "ref": ("R* f() &; S* f() &&;", ""),
    "cref": ("R* f() const &; S* f() &&;", ""),
    "rr": ("R* f() &&; S* f() const &&;", ""),
    "all4": ("R* f() &; S* f() const &; T* f() &&; U* f() const &&;", ""),
    "tmpl": ("template<class V = void> R* f(int, int = 0); S* f(int);", "1"),
    "args": ("R* f(int) const &; S* f(double) &&;", "1"),
    "refparam": ("template<class V = void> R* f(int&); S* f(int*);", "gi"),
}

# Overloads that differ in `volatile`: the analyzer tracks only `const`, so
# these still disagree with the compiler. Run with --volatile to see them.
VOLATILE_SETS = {
    "cv4": ("R* f(); S* f() const; T* f() volatile; U* f() const volatile;", ""),
    "vol": ("R* f() volatile; S* f() const volatile;", ""),
}

ALIASES = (
    "using A@N@ = const @C@; using V@N@ = volatile @C@;"
    " using RA@N@ = const @C@&; using P@N@ = const @C@*; using CP@N@ = P@N@ const;\n"
)

OBJ_PARAMS = [
    "@C@ c", "const @C@ c", "volatile @C@ c", "const volatile @C@ c",
    "@C@& c", "const @C@& c", "volatile @C@& c", "@C@&& c", "const @C@&& c",
    "A@N@ c", "V@N@ c", "RA@N@ c",
]
PTR_PARAMS = [
    "@C@* c", "const @C@* c", "@C@* const c", "const @C@* const c",
    "volatile @C@* c", "const volatile @C@* c", "P@N@ c", "CP@N@ c",
]
OBJ_ACC = [
    "c.f(@a@)", "std::move(c).f(@a@)", "static_cast<@C@&&>(c).f(@a@)",
    "((const @C@&)c).f(@a@)", "((volatile @C@&)c).f(@a@)",
    "std::forward<@C@&>(c).f(@a@)", "std::forward<@C@>(c).f(@a@)",
    "(c).f(@a@)", "((const volatile @C@&)c).f(@a@)",
]
PTR_ACC = ["c->f(@a@)", "(*c).f(@a@)", "std::move(*c).f(@a@)", "(c)->f(@a@)"]
OBJ_DERIVE = [
    "auto c = p;", "auto& c = p;", "auto&& c = p;", "const auto c = p;",
    "decltype(auto) c = p;",
]
PTR_DERIVE = [
    "auto c = p;", "auto* c = p;", "const auto* c = p;", "auto const c = p;",
    "auto& c = p;",
]
# (declaration of c, is_pointer)
LOCALS = [
    ("@C@ c;", False), ("const @C@ c;", False), ("constexpr @C@ c{};", False),
    ("constexpr auto c = @C@();", False), ("auto c = @C@();", False),
    ("const auto c = @C@();", False), ("@C@ o; auto&& c = o;", False),
    ("@C@ o; const auto& c = o;", False), ("@C@ o; @C@* const c = &o;", True),
    ("@C@ o; constexpr @C@* c = &o;", True), ("constexpr @C@& c = @G@;", False),
    ("@C@ o; const @C@* c = &o;", True),
]
SMART = [
    "std::shared_ptr<@C@> c", "std::shared_ptr<const @C@> c",
    "const std::shared_ptr<@C@> c", "std::unique_ptr<const @C@> c",
    "std::shared_ptr<const @C@>& c", "std::shared_ptr<const volatile @C@> c",
]
SMART_ACC = ["c->f(@a@)", "(*c).f(@a@)", "c.get()->f(@a@)", "std::move(c)->f(@a@)"]
CALLS = [
    "@C@ Make();", "const @C@ Make();", "@C@& Make();", "const @C@& Make();",
    "@C@&& Make();", "const @C@&& Make();", "@C@* Make();", "const @C@* Make();",
    "auto Make() -> @C@&&;", "auto Make() -> const @C@&;",
    "RA@N@ Make();",
]
CALL_ACC = ["Make().f(@a@)", "Make()->f(@a@)", "(*Make()).f(@a@)", "std::move(Make()).f(@a@)"]
CAPTURES = ["=", "&", "c", "&c", "=,&c", "&,c", "q=c", "&q=c", "=,q=c"]
FIELDS = [
    ("@C@ c;", False), ("const @C@ c;", False), ("mutable @C@ c;", False),
    ("@C@& c;", False), ("const @C@& c;", False), ("static @C@ c;", False),
    ("static const @C@ c;", False), ("@C@* c;", True), ("const @C@* c;", True),
    ("@C@* const c;", True), ("@C@&& c;", False), ("volatile @C@ c;", False),
]
METHOD_QUALS = ["", " const", " volatile", " const volatile"]
SELF_BODIES = ["f(@a@)", "this->f(@a@)", "(*this).f(@a@)", "M@N@::f(@a@)"]
SELF_QUALS = ["", " const", " volatile", " &", " const &", " &&"]
SELF_CAPTURES = ["=", "&", "this", "*this", "=,*this"]

ARRAY_PARAMS = ["@C@ (&a)[2]", "const @C@ (&a)[2]"]
ARRAY_ACC = ["a[0].f(@a@)", "(&a[0])->f(@a@)", "(*a).f(@a@)", "(a + 1)->f(@a@)"]
RANGE_DECLS = ["auto&", "const auto&", "auto&&", "auto", "const auto"]
RANGE_ACC = ["e.f(@a@)", "std::move(e).f(@a@)"]
EXPR_PARAMS = ["@C@& c, bool b", "const @C@& c, bool b", "@C@ c, bool b", "const @C@ c, bool b"]
EXPR_ACC = [
    "(b ? c : c).f(@a@)", "(b ? c : @C@()).f(@a@)", "(c, c).f(@a@)", "(c = c).f(@a@)",
    "(*&c).f(@a@)", "[&]() -> @C@& { return c; }().f(@a@)",
    "[&]() -> const @C@& { return c; }().f(@a@)",
    "[&]() -> @C@&& { return static_cast<@C@&&>(c); }().f(@a@)",
    "@C@().f(@a@)", "@C@{}.f(@a@)", "(new @C@)->f(@a@)", "(new A@N@)->f(@a@)",
    "(&c)->f(@a@)", "(&c)[0].f(@a@)",
]
DERIVED_PARAMS = [
    "D_@S@& d", "const D_@S@& d", "D_@S@&& d", "D_@S@ d", "E_@S@& d", "const E_@S@& d",
    "E_@S@&& d", "D_@S@* d", "const D_@S@* d",
]
DERIVED_ACC = [
    "d.f(@a@)", "std::move(d).f(@a@)", "static_cast<@C@&>(d).f(@a@)",
    "d.@C@::f(@a@)", "d->f(@a@)", "std::move(*d).f(@a@)",
]
DERIVED_SELF = [
    "f(@a@)", "this->f(@a@)", "(*this).f(@a@)", "@C@::f(@a@)",
    "static_cast<@C@&>(*this).f(@a@)", "static_cast<@C@&&>(*this).f(@a@)",
]

SHOW_LINE = "auto x = %s; x->Good(); Show<decltype(x)> s;"


def active_sets(with_volatile):
    return {**SETS, **(VOLATILE_SETS if with_volatile else {})}


def make_cases(sets):
    """(label, set name, text with @TOKENS@) for every combination."""
    cases = []
    for setname, (decls, _args) in sets.items():
        def add(label, text):
            cases.append((label, setname, ALIASES + text))

        def use(expr):
            return SHOW_LINE % expr

        for reference in ["&", "&&", "const &"]:
            result_type = "const @C@&" if reference == "const &" else "@C@" + reference
            add("arrow_star_result[%s]" % reference,
                "struct Proxy@N@ { %s operator->*(int); }; void Run_@N@(Proxy@N@ p) { %s }"
                % (result_type, use("(p->*0).f(@a@)")))
        for declaration in ["@C@& p", "const @C@& p"]:
            add("subscript_result[%s]" % declaration,
                "void Run_@N@(%s) { %s }" % (declaration, use("(&p)[0].f(@a@)")))
        for p in OBJ_PARAMS:
            for acc in OBJ_ACC:
                add("param[%s]|%s" % (p, acc),
                    "void Run_@N@(%s) { %s }" % (p, use(acc)))
        for p in PTR_PARAMS:
            for acc in PTR_ACC:
                add("ptrparam[%s]|%s" % (p, acc),
                    "void Run_@N@(%s) { %s }" % (p, use(acc)))
        for p in ["@C@ p", "const @C@ p", "@C@& p", "const @C@& p", "@C@&& p",
                  "A@N@ p", "volatile @C@& p"]:
            for d in OBJ_DERIVE:
                for acc in ["c.f(@a@)", "std::move(c).f(@a@)", "((const @C@&)c).f(@a@)"]:
                    add("derive[%s;%s]|%s" % (p, d, acc),
                        "void Run_@N@(%s) { %s %s }" % (p, d, use(acc)))
        for p in ["@C@* p", "const @C@* p", "@C@* const p", "P@N@ p"]:
            for d in PTR_DERIVE:
                for acc in PTR_ACC[:2]:
                    add("derive[%s;%s]|%s" % (p, d, acc),
                        "void Run_@N@(%s) { %s %s }" % (p, d, use(acc)))
        for decl, is_ptr in LOCALS:
            for acc in (PTR_ACC[:2] if is_ptr else OBJ_ACC[:7]):
                add("local[%s]|%s" % (decl, acc),
                    "extern @C@ @G@; void Run_@N@() { %s %s }" % (decl, use(acc)))
        for s in SMART:
            for acc in SMART_ACC:
                add("smart[%s]|%s" % (s, acc),
                    "void Run_@N@(%s) { %s }" % (s, use(acc)))
            add("smartget[%s]" % s,
                "void Run_@N@(%s) { auto q = c.get(); %s }" % (s, use("q->f(@a@)")))
        for call in CALLS:
            for acc in CALL_ACC:
                add("call[%s]|%s" % (call, acc),
                    "%s void Run_@N@() { %s }" % (call, use(acc)))
        for p in ["@C@ c", "const @C@ c", "@C@& c", "const @C@& c", "@C@&& c",
                  "@C@* c", "const @C@* c"]:
            ptr = "*" in p
            for cap in CAPTURES:
                for mut in ["", " mutable"]:
                    var = "q" if "q=" in cap else "c"
                    acc = "%s%s" % (var, "->f(@a@)" if ptr else ".f(@a@)")
                    add("lambda[%s;[%s]%s]" % (p, cap, mut),
                        "void Run_@N@(%s) { auto l = [%s]()%s { %s }; l(); }"
                        % (p, cap, mut, use(acc)))
            for mut in ["", " mutable"]:
                acc = "q->f(@a@)" if ptr else "q.f(@a@)"
                add("nested[%s;%s]" % (p, mut),
                    "void Run_@N@(%s) { auto outer = [c]()%s { auto inner = [&q=c]() { %s };"
                    " inner(); }; outer(); }" % (p, mut, use(acc)))
        for field, is_ptr in FIELDS:
            arrow = "c->f(@a@)" if is_ptr else "c.f(@a@)"
            this_arrow = "this->" + arrow
            deref_this = "(*this)." + arrow
            for mq in METHOD_QUALS:
                for acc in [arrow, this_arrow, deref_this]:
                    add("field[%s;Run%s]|%s" % (field, mq, acc),
                        "struct H_@N@ { %s void Run()%s { %s } };" % (field, mq, use(acc)))
            for hp in ["H_@N@& h", "const H_@N@& h", "H_@N@&& h", "volatile H_@N@& h"]:
                add("fieldparam[%s|%s]" % (field, hp),
                    "struct H_@N@ { %s }; void Run_@N@(%s) { %s }"
                    % (field, hp, use("h." + arrow)))
            add("fieldcall[%s]" % field,
                "struct H_@N@ { %s }; H_@N@ MakeH(); const H_@N@ MakeCH();"
                " void Run_@N@() { %s auto y = MakeCH().%s; y->Good();"
                " Show<decltype(y)> s2; }" % (field, use("MakeH()." + arrow), arrow))
        for body in SELF_BODIES:
            for q in SELF_QUALS:
                add("self[Run%s]|%s" % (q, body),
                    "struct M@N@ { %s void Run()%s { %s } };" % (decls, q, use(body)))
            for cap in SELF_CAPTURES:
                for mut in ["", " mutable"]:
                    add("selflambda[[%s]%s]|%s" % (cap, mut, body),
                        "struct M@N@ { %s void Run() { auto l = [%s]()%s { %s }; l(); } };"
                        % (decls, cap, mut, use(body)))
        for p in ARRAY_PARAMS:
            for acc in ARRAY_ACC:
                add("array[%s]|%s" % (p, acc), "void Run_@N@(%s) { %s }" % (p, use(acc)))
            for decl in RANGE_DECLS:
                for acc in RANGE_ACC:
                    add("range[%s;%s]|%s" % (p, decl, acc),
                        "void Run_@N@(%s) { for (%s e : a) { %s } }" % (p, decl, use(acc)))
        for p in EXPR_PARAMS:
            for acc in EXPR_ACC:
                add("expr[%s]|%s" % (p, acc), "void Run_@N@(%s) { %s }" % (p, use(acc)))
        for p in DERIVED_PARAMS:
            for acc in DERIVED_ACC:
                add("derived[%s]|%s" % (p, acc), "void Run_@N@(%s) { %s }" % (p, use(acc)))
        for body in DERIVED_SELF:
            for q in ["", " const"]:
                add("derivedself[Run%s]|%s" % (q, body),
                    "struct G@N@ : @C@ { void Run()%s { %s } };" % (q, use(body)))
            for cap in ["=", "&", "this", "*this"]:
                for mut in ["", " mutable"]:
                    add("derivedlambda[[%s]%s]|%s" % (cap, mut, body),
                        "struct G@N@ : @C@ { void Run() { auto l = [%s]()%s { %s }; l(); } };"
                        % (cap, mut, use(body)))
    return cases


def render(text, n, setname, sets):
    _decls, args = sets[setname]
    subs = {
        "@N@": str(n), "@C@": "C_" + setname, "@a@": args,
        "@G@": "gref_" + setname, "@S@": setname,
    }
    for token, value in subs.items():
        text = text.replace(token, value)
    return text


def build_source(cases, sets):
    parts = [PRELUDE]
    for setname, (decls, _args) in sets.items():
        parts.append("struct C_%s { constexpr C_%s() {} %s };\n" % (setname, setname, decls))
        parts.append("extern C_%s gref_%s;\n" % (setname, setname))
        parts.append("struct D_%s : C_%s {}; struct E_%s : C_%s { using C_%s::f; };\n"
                     % (setname, setname, setname, setname, setname))
    line = sum(p.count("\n") for p in parts) + 1
    ranges = []
    for n, (_label, setname, text) in enumerate(cases):
        block = render(text, n, setname, sets) + "\n"
        parts.append(block)
        ranges.append((line, line + block.count("\n") - 1))
        line += block.count("\n")
    return "".join(parts), ranges


ERROR_LINE = re.compile(r"^.+?:(?P<line>\d+):\d+: error: (?P<msg>.*)$")
SHOW_TYPE = re.compile(r"Show<([A-Z]) \*>")


def case_index(ranges, line):
    i = bisect.bisect_right([r[0] for r in ranges], line) - 1
    return i if 0 <= i and line <= ranges[i][1] else None


def clang_expected(path, ranges, std):
    proc = subprocess.run(
        ["clang++", "-std=" + std, "-fsyntax-only", "-ferror-limit=0", path],
        capture_output=True, text=True)
    errors = collections.defaultdict(list)
    for raw in proc.stderr.splitlines():
        m = ERROR_LINE.match(raw)
        i = case_index(ranges, int(m["line"])) if m else None
        if i is not None:
            errors[i].append(m["msg"])
    expected = {}
    for i, msgs in errors.items():
        shows = [m for m in msgs if "undefined template 'Show" in m]
        others = [m for m in msgs if "undefined template 'Show" not in m]
        found = [SHOW_TYPE.search(m) for m in shows]
        # Valid cases have exactly one deliberate error: the `Show` instantiation.
        if len(shows) == 1 and found[0] and not others:
            expected[i] = found[0].group(1)
    return expected


def analyzer_edges(trace, srcdir, ranges):
    db = os.path.join(srcdir, "out.db")
    subprocess.run([trace, "analyze", srcdir, "-o", db], check=True,
                   capture_output=True, text=True)
    rows = sqlite3.connect(db).execute(
        "SELECT cf.line_start, ce.name FROM call_edges e"
        " JOIN functions cf ON cf.id=e.caller_fn_id"
        " JOIN functions ce ON ce.id=e.callee_fn_id"
        " WHERE ce.name LIKE '%::Good'").fetchall()
    got = collections.defaultdict(set)
    for line, callee in rows:
        i = case_index(ranges, line)
        if i is not None:
            got[i].add(callee.split("::")[0])
    return got


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trace", default="target/release/trace")
    ap.add_argument("--std", default="c++20")
    ap.add_argument("--keep")
    ap.add_argument("--filter", default="")
    ap.add_argument("--show", type=int, default=40)
    ap.add_argument("--volatile", action="store_true",
                    help="include overload sets that differ in volatile")
    ap.add_argument("--strict", action="store_true",
                    help="also fail when a receiver is left untyped")
    args = ap.parse_args()

    sets = active_sets(args.volatile)
    cases = [c for c in make_cases(sets) if args.filter in c[0]]
    source, ranges = build_source(cases, sets)
    tmp = args.keep or tempfile.mkdtemp(prefix="overload_diff_")
    os.makedirs(tmp, exist_ok=True)
    path = os.path.join(tmp, "cases.cpp")
    with open(path, "w") as fh:
        fh.write(source)
    expected = clang_expected(path, ranges, args.std)
    got = analyzer_edges(args.trace, tmp, ranges)

    totals = collections.Counter()
    bad = []
    for i, want in sorted(expected.items()):
        have = got.get(i, set())
        if have == {want}:
            kind = "exact"
        elif want in have:
            kind = "extra"
        elif have:
            kind = "wrong"
        else:
            kind = "missed"
        totals[kind] += 1
        if kind != "exact":
            bad.append((kind, i, want, sorted(have)))
    print("generated %d, clang-valid %d, rejected %d" % (
        len(cases), len(expected), len(cases) - len(expected)))
    for kind in ("exact", "extra", "missed", "wrong"):
        print("  %-7s %d" % (kind, totals[kind]))
    groups = collections.Counter()
    for kind, i, _want, _have in bad:
        groups[(kind, re.split(r"[\[|]", cases[i][0])[0])] += 1
    for (kind, family), count in groups.most_common():
        print("  %-7s %-12s %d" % (kind, family, count))
    for kind, i, want, have in bad[:args.show]:
        print("%s want %s have %s set=%s %s (lines %d-%d)" % (
            kind, want, have, cases[i][1], cases[i][0], *ranges[i]))
    print("sources kept in", tmp)
    failing = ("wrong", "extra") + (("missed",) if args.strict else ())
    return 1 if any(totals[kind] for kind in failing) else 0


if __name__ == "__main__":
    sys.exit(main())
