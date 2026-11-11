// Callables wrapped by `std::bind` and `std::packaged_task` keep their call
// edge from the code that wraps them. As in the corpora, the standard headers
// are not in the tree: `std::bind`, `std::packaged_task`, `std::function` and
// `std::make_shared` are names the unit never sees declared.
#include <functional>
#include <future>
#include <memory>
#include <vector>

typedef void (*Callback)();

// A minimal `Submit`-style thread pool: the task is a `packaged_task` over a
// `std::bind` of the submitted callable and its arguments, run later through
// a queued lambda. The callable is only ever named inside `std::bind`.
void Hit() {}
int Work(Callback cb, int n) {
    cb();
    return n;
}

std::vector<std::function<void()>> queue;

template <typename F, typename... Args>
auto Submit(F &&f, Args &&...args) {
    using ReturnType = decltype(f(args...));
    auto task = std::make_shared<std::packaged_task<ReturnType()>>(
        std::bind(std::forward<F>(f), std::forward<Args>(args)...));
    queue.emplace_back([task]() { (*task)(); });
}
void Enqueue() { Submit(Work, Hit, 1); }

// The same pool with the bound argument declared by name rather than in a
// parameter pack, which the analysis does not follow: the argument reaches
// the callable's formal.
void FixedHit() {}
int FixedWork(Callback cb, int n) {
    cb();
    return n;
}
template <typename F, typename A>
auto SubmitFixed(F &&f, A &&a) {
    auto task = std::make_shared<std::packaged_task<int()>>(
        std::bind(std::forward<F>(f), std::forward<A>(a), 1));
    queue.emplace_back([task]() { (*task)(); });
}
void EnqueueFixed() { SubmitFixed(FixedWork, FixedHit); }

// A parameter pack is not a formal, so `Hit` does not reach `Work`'s `cb`
// through `Submit`'s `args`; the edge from `Submit` to `Work` is still there.

// `std::bind` on its own: the callable and its bound arguments.
void BoundHit() {}
void BoundWork(Callback cb, int n) { cb(); }
void Bind() { auto bound = std::bind(BoundWork, BoundHit, 3); }

// A `std::packaged_task` built straight from a callable.
void Direct() {}
void RunDirect() {
    std::packaged_task<void()> task(Direct);
    task();
}

// The task's signature, not the constructor, says what the callable takes: a
// callable with parameters is still the one the task stores.
int Typed(int a, double b) { return a; }
void RunTyped() {
    std::packaged_task<int(int, double)> task(Typed);
    task(1, 2.0);
}

// `std::bind` of a member function and its receiver: the member is the
// callable, and the receiver is the argument bound as its `this`.
struct Worker {
    int Step(int n) { return n; }
    void Start() { auto bound = std::bind(&Worker::Step, this, 1); }
};
void StartOn(Worker &worker) { auto bound = std::bind(&Worker::Step, &worker, 2); }

// A callable held in a `std::function` is called through it with no model.
void Held() {}
void CallHeld() {
    std::function<void()> fn = Held;
    fn();
}
