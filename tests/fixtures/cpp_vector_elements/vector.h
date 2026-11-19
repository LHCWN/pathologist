#pragma once

namespace std {
struct allocator_stub {};
struct other_allocator_stub {};
template<class T> struct typed_allocator_stub {};
template<class T, class Allocator = typed_allocator_stub<T>> class deque;
template<class Element, class Alloc> class deque {};
template<class T> class initializer_list {
    const T* first;
    decltype(sizeof(0)) size;
    constexpr initializer_list(const T* p, decltype(sizeof(0)) n) : first(p), size(n) {}
};
template<class T, class Allocator = allocator_stub> class vector {
public:
    vector();
    explicit vector(unsigned);
    vector(initializer_list<T>, allocator_stub = allocator_stub());
    void push_back(T);
    template<class U> void emplace_back(U);
    T& operator[](unsigned);
    T& at(unsigned);
    void clear();
};

template<class Allocator> class vector<bool, Allocator> {
public:
    bool operator[](unsigned) const;
};
}

using Callback = void (*)();

void alpha();
void beta();
void unrelated();

struct Holder {
    std::vector<Callback> slots;
    std::vector<Callback> other_slots;
    void store();
};

struct OtherHolder {
    std::vector<Callback> slots;
};
