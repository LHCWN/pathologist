#include "vector.h"

void alpha() {}
void beta() {}
void unrelated() {}

void array_control() { Callback callbacks[1] = {alpha}; callbacks[0](); }

void stored() {
    std::vector<Callback> callbacks;
    callbacks.push_back(alpha);
    callbacks[0]();
}

void braced_insertion() {
    std::vector<Callback> callbacks;
    callbacks.push_back({alpha});
    callbacks[0]();
}

void inline_function_pointer(unsigned i) {
    std::vector<void (*)()> callbacks;
    callbacks.push_back(beta);
    callbacks.operator[](i)();
}

void unknown_index(unsigned i) {
    std::vector<Callback> callbacks;
    callbacks.push_back(alpha);
    callbacks.emplace_back(&beta);
    callbacks.clear();
    callbacks.push_back(alpha);
    callbacks[i]();
}

void assigned(unsigned i) {
    std::vector<Callback> callbacks(1);
    callbacks[i] = beta;
    callbacks.at(i)();
}

void braced_size_constructor() {
    std::vector<Callback> callbacks{5};
}

void braced_null_constructor() {
    std::vector<Callback> callbacks{0};
}

enum Sizes : unsigned { kCount = 4 };
unsigned count();
void braced_size_enum() { std::vector<Callback> callbacks{kCount}; }
void braced_size_call() { std::vector<Callback> callbacks{count()}; }
void braced_size_expression(unsigned size) { std::vector<Callback> callbacks{size + 1}; }

namespace operations {
enum Tag { A, B };
Callback operator+(Tag, Tag) { return alpha; }
enum Count { One = 1, Two = 2 };
}
void braced_overloaded_sum() {
    std::vector<Callback> callbacks{operations::A + operations::B};
}
void braced_builtin_enum_sum() {
    std::vector<Callback> callbacks{operations::One + operations::Two};
}
namespace pointer_operations {
enum Tag { A = 1, B = 2 };
Callback operator+(Tag, const int*) { return alpha; }
}
void braced_nonviable_enum_sum() {
    std::vector<Callback> callbacks{pointer_operations::A + pointer_operations::B};
}
void braced_nonviable_integer_sum() {
    std::vector<Callback> callbacks{pointer_operations::A + 1};
}
void braced_enum_sum_with_null() {
    std::vector<Callback> callbacks{pointer_operations::A + nullptr};
}
namespace integer_operations {
enum Tag { A = 1 };
unsigned operator+(Tag, unsigned);
}
void braced_nonviable_pointer_sum(int* items) {
    std::vector<int*> pointers{integer_operations::A + items};
}
namespace scoped_operations {
enum class Tag { first = 1 };
Callback operator+(Tag, int) { return alpha; }
Callback operator+(Tag, Tag) { return beta; }
unsigned operator+(Tag, unsigned);
void braced_scoped_enum_local() {
    Tag tag = Tag::first;
    std::vector<Callback> callbacks{tag + 1};
}
}
void braced_scoped_enum_constant() {
    std::vector<Callback> callbacks{scoped_operations::Tag::first + 1};
}
void braced_scoped_enum_variable() {
    scoped_operations::Tag tag = scoped_operations::Tag::first;
    std::vector<Callback> callbacks{tag + 1};
}
void braced_scoped_enum_no_promotion() {
    std::vector<Callback> callbacks{scoped_operations::Tag::first + scoped_operations::Tag::first};
}
void braced_array_decay() {
    int items[5];
    std::vector<int*> pointers{items};
}

struct CallbackOrCount {
    operator Callback() const;
    operator unsigned() const;
};
void braced_uncertain_conversion(CallbackOrCount value) { std::vector<Callback> callbacks{value}; }

namespace plain_first {
struct Left { void hit() {} };
struct Right { void hit() {} };
struct Object { Left select(); Right select() const; };
using CObj = const Object;
void plain(std::vector<Object*> values) { values[0]->select().hit(); }
void aliased(std::vector<CObj*> values) { values[0]->select().hit(); }
}

namespace alias_first {
struct Left { void hit() {} };
struct Right { void hit() {} };
struct Object { Left select(); Right select() const; };
using CObj = const Object;
void aliased(std::vector<CObj*> values) { values[0]->select().hit(); }
void plain(std::vector<Object*> values) { values[0]->select().hit(); }
}

struct ArrayHolder {
    std::vector<Callback> slots;
    void fire() { slots[0](); }
};
ArrayHolder holders[2];
void add_array_slot() { holders[0].slots.push_back(alpha); }
void add_pointer_slot(ArrayHolder* holder) { holder[1].slots.push_back(beta); }

template<class Alloc>
bool std::vector<bool, Alloc>::operator[](unsigned) const {
    return true;
}

bool bool_subscript() {
    std::vector<bool> flags;
    return flags.operator[](0);
}

void initialized(unsigned i) {
    std::vector<Callback> callbacks = {alpha, &beta};
    callbacks[i]();
}

void initialized_with_braced_elements(unsigned i) {
    std::vector<Callback> callbacks{{alpha}, {&beta}};
    callbacks[i]();
}

void parenthesized_list() {
    std::vector<Callback> callbacks({alpha});
    callbacks[0]();
}

void parenthesized_list_with_allocator() {
    std::vector<Callback> callbacks({beta}, std::allocator_stub{});
    callbacks[0]();
}

void braced_list_with_allocator() {
    std::vector<Callback> callbacks{{alpha, beta}, std::allocator_stub{}};
    callbacks[0]();
}

void braced_list_with_empty_allocator() {
    std::vector<Callback> callbacks{{alpha, beta}, {}};
    callbacks[0]();
}

void braced_list_with_empty_callback() {
    std::vector<Callback> callbacks{{alpha}, {}};
    callbacks[0]();
}

struct DefaultSlots {
    std::vector<Callback> slots{alpha};
    std::vector<Callback> other_slots{unrelated};
    DefaultSlots() {}
};

struct ExplicitSlots {
    std::vector<Callback> slots{unrelated};
    ExplicitSlots(Callback cb) : slots{cb} {}
};

struct ImplicitSlots {
    std::vector<Callback> slots{beta};
};

void read_default_member_initializer() {
    DefaultSlots holder;
    holder.slots[0]();
}

void read_explicit_member_initializer() {
    ExplicitSlots holder(beta);
    holder.slots[0]();
}

void read_implicit_member_initializer() {
    ImplicitSlots holder;
    holder.slots[0]();
}

#define SUBSCRIPT_CALLS(v) v[0](); v[1]()
void macro_subscripts() {
    std::vector<Callback> callbacks = {alpha, beta};
    SUBSCRIPT_CALLS(callbacks);
}

#define CALL_ELEMENT(v) v[0]()
#define CALL_ELEMENT_TWICE(v) CALL_ELEMENT(v); CALL_ELEMENT(v)
void nested_macro_subscripts() {
    std::vector<Callback> callbacks = {alpha};
    CALL_ELEMENT_TWICE(callbacks);
}

#define IGNORED_VECTOR_SLOT [0]
void ignored_vector_subscripts() {
    std::vector<Callback> callbacks = {alpha};
    callbacks IGNORED_VECTOR_SLOT = beta;
    Callback from_macro = callbacks IGNORED_VECTOR_SLOT;
    from_macro();
    callbacks[0]();
}

void select_vector(std::vector<int*>);
void only_signed_vector() {
    std::vector<int*> values;
    select_vector(values);
}

void match_const(std::vector<const int*>);
void match_pointer_const(std::vector<const int**>);
void match_callback_cv(std::vector<void (*)(const int* value)>);
void cross_tu_cv() {
    std::vector<const int*> values;
    std::vector<const int**> pointers;
    std::vector<void (*)(const int*)> callbacks;
    match_const(values);
    match_pointer_const(pointers);
    match_callback_cv(callbacks);
}

unsigned subscript_index() { return 0; }

void unevaluated_subscripts() {
    std::vector<Callback> callbacks;
    (void)sizeof(callbacks[subscript_index()]);
    (void)sizeof(callbacks.operator[](0));
}

struct CallbackHolder { Callback callback; };

void inserted_member(unsigned i) {
    CallbackHolder holder;
    holder.callback = beta;
    std::vector<Callback> callbacks;
    callbacks.push_back(holder.callback);
    callbacks[i]();
}

void initialized_member(unsigned i) {
    CallbackHolder holder;
    holder.callback = beta;
    std::vector<Callback> callbacks = {holder.callback};
    callbacks[i]();
}

void isolated(unsigned i) {
    std::vector<Callback> callbacks = {alpha};
    std::vector<Callback> other = {unrelated};
    callbacks[i]();
}

void unevaluated(unsigned i) {
    std::vector<Callback> callbacks = {alpha};
    (void)noexcept(callbacks.push_back(unrelated));
    (void)sizeof(callbacks[i] = unrelated);
    callbacks[i]();
}

struct Handler { void run() {} };
struct OtherHandler { void run() {} };

void element_type_in_declaration_scope(unsigned i) {
    using Element = Callback;
    std::vector<Element> callbacks;
    callbacks.push_back(alpha);
    {
        using Element = OtherHandler*;
        callbacks[i]();
    }
}

void pointer_element(unsigned i) {
    Handler item;
    OtherHandler other_item;
    std::vector<Handler*> handlers;
    std::vector<OtherHandler*> other;
    handlers.push_back(&item);
    other.push_back(&other_item);
    Handler* selected = handlers.at(i);
    handlers[i]->run();
}

void invoke(Callback callback) { callback(); }
void pass_read(unsigned i) {
    std::vector<Callback> callbacks = {beta};
    invoke(callbacks[i]);
}

namespace app {
template<class T> struct vector {
    void push_back(T) {}
    T& operator[](unsigned);
    T& at(unsigned);
};
}

void user_vector(unsigned i) {
    app::vector<Callback> callbacks;
    callbacks.push_back(unrelated);
    callbacks.at(i)();
}

std::vector<Callback> global_callbacks = {beta};
void read_global(unsigned i) { global_callbacks.at(i)(); }

Callback return_read(unsigned i) { return global_callbacks[i]; }
void invoke_returned(unsigned i) { Callback callback = return_read(i); callback(); }

void Holder::store() {
    slots.push_back(alpha);
    other_slots.emplace_back(unrelated);
}
