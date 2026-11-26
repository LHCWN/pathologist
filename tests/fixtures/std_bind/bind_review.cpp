// Keep standard declarations out of this TU to exercise placeholder lookup
// when the standard header is unavailable.
using Callback = void (*)();
void ReviewHit() {}
void ReviewWrong() {}

void BraceTarget() {}
void brace_bind() {
    std::function<void()> f{std::bind(BraceTarget)};
    f();
}

struct CastWorker {
    void run(Callback cb) { cb(); }
    void run(int) {}
};
using Method = void (CastWorker::*)(Callback);
void member_cast(CastWorker* worker) {
    auto f = std::bind(static_cast<Method>(&CastWorker::run), worker, ReviewHit);
    f();
}
void member_variable_cast(CastWorker* worker, Method target) {
    auto f = std::bind(static_cast<Method>(target), worker, ReviewWrong);
    f();
}

void FreeCast(Callback cb) { cb(); }
void FreeCast(int) {}
using Function = void (*)(Callback);
void free_cast() {
    auto f = std::bind(static_cast<Function>(&FreeCast), ReviewHit);
    f();
}

void ReviewTarget(Callback cb) { cb(); }
void global_placeholder() {
    auto f = std::bind(ReviewTarget, ::std::placeholders::_1);
    f(ReviewHit);
}
void duplicate_flow(Callback x, bool choose) {
    std::function<void(Callback)> f = std::bind(ReviewTarget, x);
    if (choose) {
        f = std::bind(ReviewTarget, ::std::placeholders::_1);
    }
    f(x);
}

void ModeledLaunch(void (*entry)(Callback), Callback payload);
void ModelActual(Callback next) { next(); }
void ModelIgnored(Callback) {}
void mode_with_capture() {
    auto entry = ModelActual;
    auto job = std::bind(ModeledLaunch, entry, ReviewHit);
    job(ModelIgnored, ReviewWrong);
}
void mode_with_placeholders() {
    auto job = std::bind(ModeledLaunch, ::std::placeholders::_2, ::std::placeholders::_1);
    job(ReviewHit, ModelActual);
}

void StopWork(std::stop_token token, Callback cb) { cb(); }
void with_stop_token() {
    auto worker = std::jthread(
        std::bind(StopWork, ::std::placeholders::_1, ::std::placeholders::_2),
        ReviewHit);
    worker.join();
}
