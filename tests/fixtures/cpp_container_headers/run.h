#pragma once
#include <review_vector.h>

using Callback = void (*)();
void alpha();

inline void run_from_header() {
    std::vector<Callback> callbacks;
    callbacks.push_back(alpha);
    callbacks[0]();
}
