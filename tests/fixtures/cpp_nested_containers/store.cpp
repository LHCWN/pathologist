#include "nested_containers.h"

void store_slots() {
    Holder holder;
    std::vector<Callback> inner;
    inner.push_back(alpha);
    holder.slots.push_back(inner);
}

void use_shared_slots_store() {
    shared_slots[0][0]();
}
