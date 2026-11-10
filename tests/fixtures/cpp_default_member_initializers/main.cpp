#include "shared.h"

int Li() { return 1; }
int Lu() { return 1; }
int Lp() { return 1; }
int La() { return 1; }
int Ls() { return 1; }
int Lc() { return 1; }
int Ld() { return 1; }
int Lh() { return 1; }
int Ln() { return 1; }
int Lb() { return 1; }
int Lt() { return 1; }
int Lt2() { return 1; }
int Leaf() { return 1; }
int Named() { return 1; }
int Delegated() { return 1; }
int HeaderLeaf() { return 1; }

struct K {
    K(int) {}
};

struct Implicit {
    int v = Li();
    K k = K(Li());
};
void MakeImplicit() {
    Implicit i;
    (void)i;
}
void MakeOnHeap() {
    Implicit *p = new Implicit();
    (void)p;
}

struct User {
    int v = Lu();
    K k = K(Lu());
    User() {}
};
void MakeUser() { User u; }

struct OutOfLine {
    int v = Lp();
    OutOfLine();
};
OutOfLine::OutOfLine() {}

struct Agg {
    int first;
    int v = La();
};
void MakeAgg() { Agg a{}; }
struct Partial {
    int first;
    int v = Ls();
};
void MakePartial() { Partial p{1}; }
void MakeHeapPartial() {
    Partial *p = new Partial{1};
    (void)p;
}

struct Copied {
    int v = Lc();
    Copied() : v(0) {}
    Copied(const Copied &) {}
};

struct Defaulted {
    int v = Ld();
    Defaulted() = default;
};
void MakeDefaulted() { Defaulted d; }

struct InnerImplicit {
    int v = Ln();
};
struct NestedOwner {
    InnerImplicit inner;
    NestedOwner() {}
};
struct ImplicitBase {
    int v = Lb();
};
struct DerivedOwner : ImplicitBase {
    DerivedOwner() {}
};

template <class T>
struct Tmpl {
    int v = Lt();
    Tmpl() {}
};
void MakeTmpl() { Tmpl<int> t; }

// The list entry replaces the default.
struct ListK {
    K k{Named()};
    ListK() : k(Leaf()) {}
};
struct ListScalar {
    int v = Named();
    ListScalar() : v(1) {}
};
struct NamedInner {
    InnerImplicit inner;
    NamedInner() : inner{5} {}
};

// A delegating constructor leaves the defaults to its target.
struct Delegating {
    int v = Delegated();
    Delegating() : Delegating(1) {}
    Delegating(int) {}
};

struct TwoConstructors {
    int v = Lt2();
    TwoConstructors() {}
    TwoConstructors(int) {}
};

typedef void (*Callback)();
void Handler() {}
void Unrelated() {}
struct CallbackDefault {
    Callback cb = Handler;
    CallbackDefault() {}
    void Run() { cb(); }
};
struct RawCallbackDefault {
    void (*cb)() = Handler;
    RawCallbackDefault() {}
    void Run() { cb(); }
};
struct NullCallback {
    Callback cb = nullptr;
    NullCallback() {}
    void Set() { cb = Unrelated; }
    void Run() { cb(); }
};

int Ldf() { return 1; }
struct DefaultedWithOther {
    int v = Ldf();
    DefaultedWithOther() = default;
    DefaultedWithOther(int) {}
};
void MakeDefaultedWithOther() { DefaultedWithOther d; }

int Lib() { return 1; }
struct InheritedDefaultBase {
    int v = Lib();
};
struct OnlyBase : InheritedDefaultBase {};
void MakeOnlyBase() { OnlyBase d; }

int Lmk() { return 1; }
namespace std {
template <class T> struct shared_ptr {};
template <class T> shared_ptr<T> make_shared();
}
struct FactoryBuilt {
    int v = Lmk();
};
void MakeFactory() { auto p = std::make_shared<FactoryBuilt>(); }

int Lext() { return 1; }
struct ExternDeclared {
    int v = Lext();
};
void UsesExtern() {
    extern ExternDeclared g_declared;
    (void)g_declared;
}

void HandlerAddress() {}
struct UnknownTypeDefault {
    UnknownCb f = &HandlerAddress;
    UnknownTypeDefault() {}
    void Run() { f(); }
};

int Lself() { return 1; }
struct SelfBuilding {
    SelfBuilding *next = new SelfBuilding();
    int v = Lself();
};
void MakeSelfBuilding() { SelfBuilding s; }
static void MakeSelfBuildingInternal() { SelfBuilding s; }
void CallInternal() { MakeSelfBuildingInternal(); }

int Lmutual() { return 1; }
struct MutualB;
struct MutualA {
    MutualB *b = nullptr;
    int v = Lmutual();
};
struct MutualB {
    MutualA *a = new MutualA();
    MutualB *self = new MutualB();
};
void MakeMutual() { MutualB b; }

int Larg() { return 1; }
struct ArgumentConstructor {
    int v = Larg();
    ArgumentConstructor(int) {}
};
void MakeWithArgument() { new ArgumentConstructor(1); }

struct Lambda {
    int v = [] { return Lh(); }();
    Lambda() {}
};

int Ltwice() { return 1; }
struct Twice {
    int v = Ltwice();
};
static void ConstructTwiceInternal()
{
    Twice a;
    Twice b{};
    new Twice();
}
void CallConstructTwice() { ConstructTwiceInternal(); }
void ConstructTwiceExternal()
{
    Twice a;
    Twice b{};
    new Twice();
}

using TwiceCallback = int (*)();
int TwiceHandler() { return 1; }
TwiceCallback MakeTwiceHandler() { return TwiceHandler; }
struct TwiceHolder {
    TwiceCallback f = MakeTwiceHandler();
};
static void CallSecondHolder()
{
    TwiceHolder a;
    TwiceHolder b;
    b.f();
}
void RunSecondHolder() { CallSecondHolder(); }
