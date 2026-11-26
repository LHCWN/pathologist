#pragma once

namespace std {
template<class Signature> class function;
template<class R, class... A> class function<R(A...)> {
public:
    template<class F> function(F);
    R operator()(A...);
};
template<class F, class... A> function<void()> bind(F, A...);
namespace placeholders { extern int _1; extern int _2; }
}

using Callback = void (*)();
using Task = std::function<void()>;

void Dispatch(Callback callback);
Task MakeTask(Callback captured);

struct Queue {
    Task task;
    void Store(Task work);
    void Fire();
};
