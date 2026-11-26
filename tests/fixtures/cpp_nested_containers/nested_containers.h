#pragma once

namespace std {
template<class T> class initializer_list {
    const T* first;
    decltype(sizeof(0)) size;
};

template<class K, class V> struct pair {
    K first;
    V second;
};

// The converting constructor and assignment make construction and assignment
// from pair<int, V> to pair<long, V> valid C++.
template<class V> struct pair<long, V> {
    long first;
    V second;
    pair();
    pair(long, V);
    template<class K2, class V2> pair(const pair<K2, V2> &);
    template<class K2, class V2> pair &operator=(const pair<K2, V2> &);
};

template<class A, class B> pair<A, B> make_pair(A, B);

struct allocator_stub {};

template<class T> class allocator;
template<class T> class less {};

template<class K, class V, class Compare = less<K>, class Alloc = allocator<pair<const K, V>>>
class map {
public:
    struct iterator {
        pair<K, V>* operator->();
        pair<K, V>& operator*();
    };
    map();
    map(initializer_list<pair<K, V>>);
    map(initializer_list<pair<K, V>>, allocator_stub);
    map(const map &, Alloc);
    iterator find(K);
    void insert(pair<K, V>);
    void insert(initializer_list<pair<K, V>>);
    V& at(K);
    V& operator[](K);
};

template<class T> class allocator {
public:
    allocator();
    template<class U> allocator(U);
};

template<class T, class Alloc = allocator<T>> class list {
public:
    list(initializer_list<T>);
    list(const list &, Alloc);
    T& front();
};

template<class T> T&& move(T&);
template<class T> T&& forward(T&);

template<class T, class Allocator = allocator<T>> class vector {
public:
    struct iterator {
        T& operator*() const;
        iterator& operator++();
        bool operator!=(const iterator&) const;
    };
    vector();
    vector(unsigned);
    vector(unsigned, const T&, allocator_stub = allocator_stub());
    vector(initializer_list<T>, allocator_stub = allocator_stub());
    vector(const vector &, Allocator);
    void push_back(T);
    template<class U> void emplace_back(U);
    iterator begin();
    iterator end();
    iterator insert(iterator, T);
    iterator insert(iterator, initializer_list<T>);
    T& operator[](unsigned);
    T& at(unsigned);
};
} // namespace std

using Callback = void (*)();
using CallbackVecAlt = std::vector<Callback, std::allocator<long>>;
// A distinct specialization no other fixture function populates, so a pair
// field copy across differing pair specializations is observable in isolation.
using CallbackVecPairCopy = std::vector<Callback, std::allocator<short>>;

void alpha();
void beta();
void unrelated();

struct Holder {
    std::vector<std::vector<Callback>> slots;
};

struct Handler {
    void run();
};

struct OtherHandler {
    void run();
};

struct NumberKey {
    NumberKey(int);
};

namespace std {
template<class V> struct pair<NumberKey, V> {
    NumberKey first;
    V second;
    pair(NumberKey, V);
    template<class U> pair(const pair<int, U> &);
};

template<class K, class V> struct pair<K const, V> {
    K const first;
    V second;
    pair(K, V);
    template<class U> pair(const pair<K, U> &);
};

template<class K, class V> struct pair<K const *, V> {
    K const *first;
    V second;
    pair(K const *, V);
    template<class U> pair(const pair<K *, U> &);
};
} // namespace std

namespace app {
struct Policy {
    int value;
};

template<class PolicyT, class Value> class Custom {
public:
    void insert(std::pair<int, Value>);
    Value& operator[](int);
};
} // namespace app

static std::vector<std::vector<Callback>> shared_slots = {{alpha}};

struct CallbackKey2 {
    Callback cb;
    std::vector<Callback> items;
};

struct ConvertibleEntry {
    operator std::pair<int, Callback>() const;
};

struct ConvertibleCallback {
    operator Callback() const;
};

struct Key {
    int a;
    int b;
};

struct AggregateKey {
    int a;
    Callback cb;
};

struct RawHolder {
    Callback items[1];
};
