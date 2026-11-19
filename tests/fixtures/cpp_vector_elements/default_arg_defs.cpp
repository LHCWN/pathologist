#include "vector.h"

void match_default_allocator(std::vector<Callback, std::allocator_stub>*) {}
void match_default_allocator(std::vector<Callback, std::other_allocator_stub>*) {}
void match_dependent_default(std::deque<Callback, std::typed_allocator_stub<Callback>>*) {}
void match_dependent_default(std::deque<Callback, std::other_allocator_stub>*) {}
