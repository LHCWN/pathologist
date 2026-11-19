namespace std {
template<class T> class deque {
public:
    void push_front(T);
    T& front();
    T& operator[](unsigned);
};

template<class Key, class Value> class map {
public:
    void insert_or_assign(Key, Value);
    Value& operator[](Key);
};

template<class T> class vector {
public:
    void push_back(T);
    T& operator[](unsigned);
};
}

using Callback = void (*)();
void alpha() {}
void beta() {}
void unrelated() {}

namespace app {
template<class T> struct Box {
    void put(T, int = 0);
    void put_extra(T, ...);
    T& get(int = 0);
};
}

void defaulted_box_dispatch() {
    app::Box<Callback> callbacks;
    callbacks.put(alpha);
    callbacks.put_extra(beta, 0);
    callbacks.get()();
}

void deque_dispatch(unsigned i) {
    std::deque<Callback> callbacks;
    callbacks.push_front(alpha);
    callbacks.front() = beta;
    callbacks[i]();
}

void map_dispatch() {
    std::map<Callback, Callback> callbacks;
    callbacks.insert_or_assign(unrelated, beta);
    callbacks[unrelated]();
}

void vector_dispatch() {
    std::vector<Callback> callbacks;
    callbacks.push_back(alpha);
    callbacks[0]();
}
