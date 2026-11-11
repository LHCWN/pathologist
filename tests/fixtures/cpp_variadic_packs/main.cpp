// Arguments passed through a variadic template's parameter pack reach the
// formals the pack is forwarded to. As in the corpora, the standard headers
// are not in the tree: `std::forward` and `std::thread` are names the unit
// never sees declared.
#include <thread>
#include <utility>

typedef void (*Callback)();

// A pack forwarded to a named callee with `std::forward`.
void HitForwarded() {}
void Target(Callback cb) { cb(); }
template <typename... Args>
void Forward(Args &&...args) {
    Target(std::forward<Args>(args)...);
}
void CallForward() { Forward(HitForwarded); }

// A pack by value, expanded bare, with a scalar beside the callback.
void HitPlain() {}
void TargetPlain(Callback cb, int n) { cb(); }
template <typename... Args>
void ForwardPlain(Args... args) {
    TargetPlain(args...);
}
void CallPlain() { ForwardPlain(HitPlain, 1); }

// A pack after a fixed parameter, forwarded through a call the parameter
// names: the callee is an indirect target.
void HitAfter() {}
void TargetAfter(Callback cb) { cb(); }
template <typename F, typename... Args>
void Call(F &&f, Args &&...args) {
    f(std::forward<Args>(args)...);
}
void CallAfter() { Call(TargetAfter, HitAfter); }

// A pack handed to a model that runs its callable: `std::thread`.
void ThreadBody(Callback cb) { cb(); }
void HitThread() {}
template <typename F, typename... Args>
void Spawn(F &&f, Args &&...args) {
    std::thread t(std::forward<F>(f), std::forward<Args>(args)...);
    t.join();
}
void CallSpawn() { Spawn(ThreadBody, HitThread); }

// The expansion covers the positions from its own on, not those before it:
// `first` is bound to the fixed argument alone.
void HitFirst() {}
void HitRest() {}
void Pick(Callback first, Callback second) {
    first();
    second();
}
template <typename... Args>
void PassOn(Callback first, Args... rest) {
    Pick(first, rest...);
}
void CallPassOn() { PassOn(HitFirst, HitRest); }

// Several arguments: a pack takes every one passed, up to the last, and a
// formal after the expansion's position takes the expansion although no
// argument is written there.
void H1() {}
void H2() {}
void H3() {}
template <typename... Args>
void Collect(Args &&...all) {}
void CallCollect() { Collect(H1, H2, H3); }
template <typename... Args>
void Gather(Callback head, Args &&...tail) {}
void CallGather() { Gather(H1, H2, H3); }
void HitOne() {}
void HitTwo() {}
void TargetTwo(Callback a, Callback b) {
    a();
    b();
}
template <typename... Args>
void ForwardTwo(Args &&...args) {
    TargetTwo(std::forward<Args>(args)...);
}
void CallTwo() { ForwardTwo(HitOne, HitTwo); }

// A pack nothing forwards carries its arguments nowhere.
void Unrelated() {}
void Loose(Callback cb) { cb(); }
template <typename... Args>
void Drop(Args &&...args) {}
void CallDrop() { Drop(Unrelated); }

// An expansion may be empty: a callable that takes nothing is still the one
// a member template runs, though the call lists only the callable. The member
// is declared in its class and defined outside it.
class Pool {
public:
    template <typename F, typename... Args>
    void Run(F &&f, Args &&...args);
};
template <typename F, typename... Args>
void Pool::Run(F &&f, Args &&...args) {
    std::thread t(std::forward<F>(f), std::forward<Args>(args)...);
    t.join();
}
void Idle() {}
void CallPool(Pool &pool) { pool.Run(Idle); }

// A call that passes nothing for the pack still reaches the function: the
// pack is no declared parameter, so it adds none to the arity a call needs.
void Sink() {}
template <typename... Args>
void Emit(Args &&...args) {
    Sink();
}
void CallEmpty() { Emit(); }
class Holder {
public:
    template <typename... Args>
    Holder(Args &&...args) {
        Sink();
    }
};
void MakeEmpty() { Holder holder; }

// A member template declared in its class and defined outside it, with a
// parameter whose class the unit never sees: the pack is no declared
// parameter, so the definition's signature and the declaration's still agree
// and the two are one function.
class Widget {
public:
    template <typename... Args>
    void Add(Mystery mystery, Args &&...args);
};
template <typename... Args>
void Widget::Add(Mystery mystery, Args &&...args) {}

// An argument after an expansion has no known position: it may reach any
// formal from the expansion's own on, so it is not dropped.
void HitLead() {}
void HitTrail() {}
void TargetTrail(Callback a, Callback b) {
    a();
    b();
}
template <typename... Args>
void ForwardTrail(Args &&...args) {
    TargetTrail(std::forward<Args>(args)..., HitTrail);
}
void CallTrail() { ForwardTrail(HitLead); }
