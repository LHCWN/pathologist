namespace std {
template<class T> class initializer_list {
    const T* first;
    decltype(sizeof(0)) size;
    constexpr initializer_list(const T* p, decltype(sizeof(0)) n) : first(p), size(n) {}
};

template<class K, class V> struct pair { K first; V second; };
template<class K, class V> pair<K, V> make_pair(K key, V value) { return {key, value}; }
template<class F> class function;
template<class R, class... Args> class function<R(Args...)> {
public:
    template<class F> function(F) {}
    R operator()(Args...) const;
};

template<class K, class V> class map {
public:
    struct iterator {
        pair<K, V>* operator->();
        pair<K, V>& operator*();
    };
    struct const_iterator { const pair<K, V>* operator->() const; };
    map();
    map(initializer_list<pair<K, V>>) {}
    iterator find(K);
    const_iterator find(K) const;
    iterator begin();
    void insert(pair<K, V>);
    void insert(initializer_list<pair<K, V>>);
    template<class... Args> void emplace(Args&&...);
    void insert_or_assign(K, V);
    V& at(K);
    V& operator[](K);
};

template<class K, class V> class unordered_map {
public:
    struct iterator { pair<K, V>* operator->(); };
    unordered_map(std::initializer_list<pair<K, V>> values);
    iterator find(K);
};

inline namespace __cxx11 {
template<class T> class list {
public:
    template<class... Args> void emplace_back(Args&&...);
    T& back();
};
}

template<class T> class forward_list {
public:
    void push_front(T);
    T& front();
};

template<class T> class vector {
public:
    struct iterator {};
    iterator begin();
    iterator insert(iterator, T);
    iterator insert(iterator, initializer_list<T>);
    template<class... Args> iterator emplace(iterator, Args&&...);
    T& front();
};

template<class T> class queue {
public:
    void push(T);
    template<class... Args> T& emplace(Args&&...);
    void pop();
    T& front();
    T& back();
};

template<class T> class set {
public:
    struct iterator { const T& operator*() const; };
    set();
    set(initializer_list<T>);
    iterator begin();
    iterator find(T);
    void insert(initializer_list<T>);
    template<class... Args> void emplace(Args&&...);
    template<class... Args> iterator emplace_hint(iterator, Args&&...);
};

template<class T> class unordered_set {
public:
    struct iterator { const T& operator*() const; };
    unordered_set(std::initializer_list<T> values);
    iterator begin();
    iterator find(T);
    void insert(T);
};
}

using Callback = void (*)();
void alpha() {}
void beta() {}
void unrelated() {}
void from_capture() {}
void from_argument() {}

void list_dispatch() {
    std::list<Callback> callbacks;
    callbacks.emplace_back(alpha);
    callbacks.back()();
}

void forward_list_dispatch() {
    std::forward_list<Callback> callbacks;
    callbacks.push_front(beta);
    callbacks.front()();
}

void vector_position_dispatch() {
    std::vector<Callback> callbacks;
    callbacks.insert(callbacks.begin(), alpha);
    callbacks.emplace(callbacks.begin(), beta);
    callbacks.front()();
}

void vector_insert_list() {
    std::vector<Callback> callbacks;
    callbacks.insert(callbacks.begin(), {alpha, beta});
    callbacks.front()();
}

void set_insert_list() {
    std::set<Callback> callbacks;
    callbacks.insert({alpha, beta});
    (*callbacks.begin())();
}

void map_insert_factory_pair() {
    std::map<int, Callback> callbacks;
    callbacks.insert(std::make_pair(0, alpha));
    callbacks.at(0)();
}

void map_insert_named_pair() {
    std::map<int, Callback> callbacks;
    std::pair<long, Callback> other{0, unrelated};
    std::pair<int, Callback> entry{0, beta};
    callbacks.insert(entry);
    callbacks.at(0)();
}

void map_insert_list() {
    std::map<int, Callback> callbacks;
    callbacks.insert({{0, alpha}, {1, beta}});
    callbacks.at(0)();
}

void queue_front_dispatch() {
    std::queue<Callback> callbacks;
    std::queue<Callback> other;
    callbacks.push(alpha);
    callbacks.emplace(beta);
    other.push(unrelated);
    callbacks.pop();
    callbacks.front()();
}

void queue_back_dispatch() {
    std::queue<Callback> callbacks;
    callbacks.emplace(beta);
    callbacks.back()();
}

void set_iterator_dispatch() {
    std::set<Callback> callbacks{alpha};
    std::set<Callback> other{unrelated};
    auto it = callbacks.begin();
    auto copy = it;
    (*copy)();
}

void set_lookup_dispatch() {
    std::set<Callback> callbacks;
    callbacks.emplace(alpha);
    callbacks.emplace_hint(callbacks.begin(), beta);
    (*callbacks.find(beta))();
}

void invoke_set_element(Callback callback) { callback(); }

void unordered_set_dispatch() {
    std::unordered_set<Callback> callbacks{alpha};
    callbacks.insert(beta);
    invoke_set_element(*callbacks.find(beta));
}

struct Handler { void run() {} };

void set_pointer_dispatch() {
    Handler handler;
    std::unordered_set<Handler*> handlers{&handler};
    (*handlers.find(&handler))->run();
}

void map_lookup() {
    std::map<Callback, Callback> callbacks = {{unrelated, alpha}, {unrelated, beta}};
    auto it = callbacks.find(unrelated);
    auto copy = it;
    copy->second();
}

void map_braced_mapped_values() {
    std::map<int, Callback> callbacks{{0, {alpha}}, {1, {beta}}};
    callbacks.begin()->second();
}

struct Left { void hit() {} };
struct Right { void hit() {} };
struct Object {
    Left select() { return {}; }
    Right select() const { return {}; }
};

void map_pointer_cv_arrow() {
    std::map<int, const Object*> values;
    auto it = values.begin();
    it->second->select().hit();
}

void map_pointer_cv_deref() {
    std::map<int, const Object*> values;
    auto it = values.begin();
    (*it).second->select().hit();
}

void explicitly_typed_iterator() {
    std::map<int, Callback> callbacks = {{0, alpha}};
    std::map<int, Callback>::iterator it = callbacks.find(0);
    it->second();
}

void explicitly_typed_const_iterator() {
    const std::map<int, Callback> callbacks = {{0, beta}};
    std::map<int, Callback>::const_iterator it = callbacks.find(0);
    it->second();
}

using CallbackMap = std::map<int, Callback>;
void aliased_iterator() {
    CallbackMap callbacks = {{0, alpha}};
    CallbackMap::iterator it = callbacks.begin();
    it->second();
}

void locally_aliased_const_iterator() {
    using Map = CallbackMap;
    const Map callbacks = {{0, beta}};
    Map::const_iterator it = callbacks.find(0);
    it->second();
}

void iterator_isolation() {
    std::map<int, Callback> left = {{0, alpha}};
    std::map<int, Callback> right = {{0, unrelated}};
    auto it = left.begin();
    auto other = right.begin();
    (*it).second();
}

void iterator_reassignment() {
    std::map<int, Callback> left = {{0, alpha}};
    std::map<int, Callback> right = {{0, beta}};
    auto it = left.find(0);
    it = right.find(0);
    it->second();
}

void map_insertion() {
    std::map<Callback, Callback> callbacks;
    callbacks.insert({unrelated, alpha});
    callbacks.insert_or_assign(unrelated, beta);
    callbacks.at(unrelated)();
}

void immediate_lookup() {
    std::map<int, Callback> callbacks;
    callbacks.emplace(0, alpha);
    callbacks.find(0)->second();
}

void iterator_write() {
    std::map<int, Callback> callbacks = {{0, alpha}};
    auto it = callbacks.find(0);
    it->second = beta;
    callbacks.at(0)();
}

void unordered_dispatch(Callback captured, Callback forwarded) {
    std::unordered_map<int, std::function<Callback(Callback)>> callbacks = {
        {0, [captured](Callback value) { value(); return captured; }},
        {1, [](Callback value) { value(); return beta; }},
    };
    auto iter = callbacks.find(0);
    Callback result = iter->second(forwarded);
    result();
}

void entry() { unordered_dispatch(from_capture, from_argument); }
