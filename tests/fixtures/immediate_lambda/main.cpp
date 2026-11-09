// Lambdas invoked where they are written, the expansion of OpenHarmony's
// IN_PROCESS_CALL among them, without SDK headers.
#include "shared.h"

#define IN_PROCESS_CALL(theCall)                                     \
    ([&]() {                                                         \
        int identity = 0;                                            \
        auto retVal = theCall;                                       \
        identity = 0;                                                \
        return retVal;                                               \
    }())
#define EITHER(a, b) ([&] { return a; }() + [&] { return b; }())

int CallStartAbilityInner(int userId) { return userId; }

int Start(const char *bundle)
{
    int request = 1;
    auto startAbilityTask = [request](const char *b, int appIndex) mutable {
        return CallStartAbilityInner(request + appIndex);
    };
    return IN_PROCESS_CALL(startAbilityTask(bundle, 7));
}

int Leaf(const char *name) { return 0; }

// The lambda's text holds a string literal.
int WithLiteral() { return IN_PROCESS_CALL(Leaf("demo")); }

// Not a macro; the callee is parenthesized.
int First() { return 1; }
int Parenthesized() { return ([] { return First(); })(); }

// Its enclosing name holds quotes.
unsigned long long operator""_kb(unsigned long long v)
{
    return [&] { return static_cast<unsigned long long>(Leaf("q")); }();
}

// Both lambdas sit at the macro's invocation.
int Second() { return 2; }
int Both() { return EITHER(First(), Second()); }

// One inside the other, the inner written in the outer's macro argument.
int Third() { return 3; }
int Nested() { return IN_PROCESS_CALL(IN_PROCESS_CALL(First()) + Third()); }

struct Holder {
    int Run() { return [this](int v) { return Keep(v); }(3); }
    int Keep(int v) { return v; }
};

void Sink(int *p) {}
int global;
void PassesArgument() { [](int *q) { Sink(q); }(&global); }

// Written but never invoked: its body is no call of the enclosing function.
int Unused() { return 4; }
void NeverInvoked() { auto lambda = [] { return Unused(); }; }

// Unevaluated: the lambda's body never runs from here.
int NotRun() { return 5; }
void Unevaluated() { using T = decltype([] { return NotRun(); }()); }

// The value the lambda returns reaches the destination.
void Target() {}
using Fn = void (*)();
void UsesResult() { Fn f = [] { return &Target; }(); f(); }

// The call depends on the argument reaching the lambda's parameter.
void PassesCallback() { [](Fn f) { f(); }(&Target); }

// Reaches no visible callee: the site stays, with an edge only in the raw graph.
void EmptyBody() { [] {}(); }

int HeaderLeaf() { return 6; }

// No function encloses it, so its body stays in the complete graph.
int FileScopeLeaf() { return 7; }
int file_scope = [] { return FileScopeLeaf(); }();
