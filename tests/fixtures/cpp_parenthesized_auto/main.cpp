// Parenthesized `auto` declarations keep C++'s declaration/value split: a
// call-shaped apparent type that names a value is an initializer, while a
// type name (including a template parameter or an imported name) is a
// prototype. A bare unresolved name stays a value, as direct initialization
// assumes for `T w(E::kind);`.
struct Worker {
    void Run() {}
};
struct PrimaryTag {};
struct Remote {};
struct IFoo {
    void Call() {}
};
template <class Tag>
Worker *Lookup();
template <class Tag>
Tag *iface_cast(Remote *remote);

namespace ns {
struct Widget {
    void Go() {}
};
template <class Tag>
Worker *Find();
} // namespace ns
using ns::Widget;
using namespace ns;

template <class T>
struct Box {};

template <class T>
class Holder {
public:
    void TypeParameter() { auto make(T); }
    void Alias() {
        using Alias = T;
        auto value(Alias());
    }
};

class Known {
public:
    void TypeAlias() {
        using Alias = Widget;
        auto w(Alias());
    }
    void QualifiedType() { auto x(ns::Widget()); }
    void UnqualifiedTemplateType() { auto b(Box<int>()); }
};

class Templates {
public:
    void UnqualifiedTemplateId() {
        auto w(Lookup<PrimaryTag>());
        auto task = [w]() { w->Run(); };
        task();
    }
    void QualifiedTemplateId() {
        auto w(ns::Find<PrimaryTag>());
        auto task = [w]() { w->Run(); };
        task();
    }
    void CopyTemplateId() {
        auto w = Lookup<PrimaryTag>();
        auto task = [w]() { w->Run(); };
        task();
    }
};

class Ipc {
public:
    void Cast(Remote *remote) {
        auto proxy(iface_cast<IFoo>(remote));
        auto task = [proxy]() { proxy->Call(); };
        task();
    }
};

class Missing {
public:
    void TypeName() { auto v(Missing()); }
    void BareValue() { auto u(UNKNOWN_VALUE); }
};

struct Gadget {
    Gadget(int);
    Gadget();
};
Gadget::Gadget(int) {}
Gadget::Gadget() {}
template <class T>
struct NonAutoHolder {
    void Proto() { Gadget build(T); }
    void Value(int n) { Gadget w(n); }
};
template <class T>
struct OutOfLine {
    void Member();
};
template <class T>
void OutOfLine<T>::Member() { Gadget build(T); }

void drive() {
    Holder<int> h;
    h.TypeParameter();
    h.Alias();
    Known k;
    k.TypeAlias();
    k.QualifiedType();
    k.UnqualifiedTemplateType();
    Templates t;
    t.UnqualifiedTemplateId();
    t.QualifiedTemplateId();
    t.CopyTemplateId();
    Remote remote;
    Ipc ipc;
    ipc.Cast(&remote);
    Missing m;
    m.TypeName();
    m.BareValue();
    NonAutoHolder<int> n;
    n.Proto();
    n.Value(1);
    OutOfLine<int> o;
    o.Member();
}
