#include "nested_containers.h"

void read_slots() {
    Holder holder;
    for (auto &slot : holder.slots) {
        for (Callback callback : slot) {
            callback();
        }
    }
}

void use_shared_slots_read() {
    shared_slots[0][0]();
}
