int storage;
struct C {
    int &ref() { return storage; }
    int &&rref() { return static_cast<int &&>(storage); }
    void go(void (*callback)(int)) { callback(1); }
};
void reference_member(C &c) { auto pm = &C::ref; (c.*pm)(); }
void rvalue_reference_member(C &c) { auto pm = &C::rref; (c.*pm)(); }

struct Callbacks {
    static void target(int) {}
    static void target(double) {}
};
void callback_argument(C &c) {
    void (C::*pm)(void (*)(int)) = &C::go;
    (c.*pm)(&Callbacks::target);
}

struct Functor {
    void operator()() & {}
    void operator()() && {}
    void operator()() const & {}
    void operator()() const && {}
};
Functor functor;
struct Proxy {
    Functor operator->*(int) { return {}; }
    Functor &operator->*(double) { return functor; }
    const Functor operator->*(char) { return {}; }
    const Functor &operator->*(bool) { return functor; }
    Functor &&operator->*(long) { return static_cast<Functor &&>(functor); }
};
void value_functor(Proxy p) { (p->*1)(); }
void reference_functor(Proxy p) { (p->*1.0)(); }
void const_value_functor(Proxy p) { (p->*'x')(); }
void const_reference_functor(Proxy p) { (p->*true)(); }
void rvalue_reference_functor(Proxy p) { (p->*1L)(); }

using Callback = void (*)();
void callback_target() {}
struct CallbackProxy {
    Callback operator->*(int) { return callback_target; }
};
void after() {}
void callback_label(CallbackProxy p) { (p->*1)(); after(); }
