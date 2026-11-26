#include "callbacks.h"

void Dispatch(Callback callback) { callback(); }
void Queue::Store(Task work) { task = work; }
void Queue::Fire() { task(); }

void CrossTarget() {}
void Cross(Queue* queue) {
    Task a = MakeTask(CrossTarget);
    Task copy = a;
    queue->Store(copy);
    queue->Fire();
}
