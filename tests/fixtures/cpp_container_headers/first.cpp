#include "run.h"

void alpha() {}
void first() { run_from_header(); }

struct Node {
    Node* second;
    void run() {}
};

void ordinary_second_chain(Node* node) {
    node->second->second->second->second->second->second
        ->second->second->second->second->second->second
        ->second->second->second->second->second->second
        ->second->second->second->second->second->second->run();
}

struct Chain;
struct Inner { Chain& front(); };
struct Chain {
    Inner inner;
    void run() {}
};

void ordinary_front_chain(Chain& chain) {
    chain.inner.front().inner.front().inner.front().inner.front().inner.front().inner.front()
        .inner.front().inner.front().inner.front().inner.front().inner.front().inner.front()
        .inner.front().inner.front().inner.front().inner.front().inner.front().inner.front()
        .inner.front().inner.front().inner.front().inner.front().inner.front().inner.front().run();
}
