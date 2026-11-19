#include "vector.h"

void match_default_allocator(std::vector<Callback>*);
void match_dependent_default(std::deque<Callback>*);

void call_default_allocator(std::vector<Callback>* callbacks) {
    match_default_allocator(callbacks);
}

void call_dependent_default(std::deque<Callback>* callbacks) {
    match_dependent_default(callbacks);
}
