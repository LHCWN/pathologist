#include "callbacks.h"

void alpha() {}
void unrelated() {}

void control() { std::function<void()> f = alpha; f(); }
void bound() { std::function<void()> f = std::bind(alpha); f(); }
void dropped() { auto unused = std::bind(alpha); }
void immediate() { std::bind(alpha)(); }
void via_directive() {
    using namespace std;
    auto f = bind(alpha);
    auto copy = f;
    copy();
}

struct Worker { void run() {} };
void member(Worker* w) {
    std::function<void()> f = std::bind(&Worker::run, w);
    f();
}
void member_placeholder(Worker* later) {
    std::function<void(Worker*)> f = std::bind(&Worker::run, std::placeholders::_1);
    f(later);
}

void Reorder(Callback first, Callback second, Callback third, Callback stored) {
    first();
    second();
    third();
    stored();
}
void Left() {}
void Right() {}
void Captured() {}
void order() {
    using std::bind;
    using std::placeholders::_2;
    using namespace std::placeholders;
    std::function<void(Callback, Callback, Callback)> f = bind(Reorder, _2, _1, _2, Captured);
    f(Left, Right, unrelated);
}

Callback Echo(Callback cb) { return cb; }
void returned() {
    std::function<Callback()> f = ::std::bind(Echo, Captured);
    Callback result = f();
    result();
}

Task MakeTask(Callback captured) { return std::bind(Dispatch, captured); }

void alternatives(bool choose, Callback first, Callback second) {
    Task f = std::bind(Dispatch, first);
    if (choose) {
        f = std::bind(Dispatch, second);
    }
    f();
}

namespace custom { Task bind(Callback); }
void foreign_bind() {
    using custom::bind;
    using namespace std;
    Task f = bind(unrelated);
    f();
}
void opaque(Callback callback) {
    Task f = std::bind(callback);
    f();
}
void maybe_opaque(Callback callback, bool choose) {
    Task f = std::bind(alpha);
    if (choose) {
        f = std::bind(callback);
    }
    f();
}
void GiveOpaqueTarget(bool choose) {
    opaque(unrelated);
    maybe_opaque(unrelated, choose);
}

int* AddTimerEvent(Task task);
struct Plugin {
    void OnEventProxy(Callback event) { event(); }
    void DelayProcessEvent(Callback event) {
        AddTimerEvent(std::bind(&Plugin::OnEventProxy, this, event));
    }
};
void Schedule(Plugin* plugin) { plugin->DelayProcessEvent(Captured); }

void StartWorker(Callback left, Callback right) { left(); right(); }
void Launch(Callback left, Callback right) {
    auto worker = std::thread(std::bind(StartWorker, std::placeholders::_2, std::placeholders::_1),
                              left, right);
    worker.join();
}
void LaunchBoth() { Launch(Left, Right); }
