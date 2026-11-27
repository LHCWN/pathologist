#include "nested_containers.h"

struct FloatingKey {
    FloatingKey(double);
};

void map_numeric_key_initializer() {
    std::map<FloatingKey, Callback> callbacks{{1, alpha}};
    callbacks.at(1)();
}

void map_numeric_key_insert() {
    std::map<FloatingKey, Callback> callbacks;
    callbacks.insert({1, alpha});
    callbacks.at(1)();
}

struct NonDefaultKey {
    NonDefaultKey(int);
    NonDefaultKey(int, Callback);
};

void empty_brace_with_nondefault_key() {
    std::map<NonDefaultKey, Callback> callbacks;
    callbacks.insert({{1, unrelated}, {}});
    callbacks.at(1)();
}

using Inner = std::vector<Callback>;
struct DerivedCallbacks : Inner {};
struct ConvertsToCallbacks {
    operator Inner() const;
};

void inner_container_conversions(DerivedCallbacks derived, ConvertsToCallbacks converted) {
    std::vector<Inner> outer{{alpha}, derived, converted};
    outer[0][0]();
}

#define EXPLICIT_SLOT operator[]
void explicit_subscript_macro() {
    std::vector<Inner> outer{{alpha}};
    outer.EXPLICIT_SLOT(0)[0]();
}

void move_inner_callbacks() {
    Inner first{alpha};
    Inner second{beta};
    std::vector<Inner> outer;
    outer.push_back(std::move(first));
    outer.push_back(std::forward<Inner>(second));
    outer[0][0]();
}

void copy_list_with_allocator() {
    using Callbacks = std::list<Callback>;
    Callbacks inner{alpha};
    std::vector<Callbacks> outer;
    outer.push_back({inner, std::allocator<Callback>{}});
    outer[0].front()();
}

void copy_map_with_allocator() {
    using Callbacks = std::map<int, Callback>;
    Callbacks inner{{1, alpha}};
    std::vector<Callbacks> outer;
    outer.push_back({inner, std::allocator<std::pair<const int, Callback>>{}});
    outer[0].at(1)();
}

void int_callback(int) {}
void pointer_callback(int*) {}
void const_pointer_callback(const int*) {}

void copy_callback_parameter_cv() {
    std::vector<void (*)(int)> inner{int_callback};
    std::vector<std::vector<void (*)(const int)>> outer;
    outer.push_back(inner);
    outer[0][0](0);
}

struct PointerParameters {
    std::vector<std::vector<void (*)(int*)>> callbacks;
    std::vector<std::vector<void (*)(const int*)>> const_callbacks;
};

void copy_callback_pointer_parameter_cv() {
    PointerParameters values;
    values.callbacks.push_back({pointer_callback});
    values.const_callbacks.push_back({const_pointer_callback});
    values.callbacks[0][0](nullptr);
}

struct TagKey { int ident; };
enum class EnumTagKey { First };

void map_pair_struct_key() {
    std::pair<TagKey, Callback> key{TagKey{1}, unrelated};
    std::map<std::pair<TagKey, Callback>, Callback> callbacks;
    callbacks.insert({key, {}});
    callbacks.at(key)();
}

void map_make_pair_enum_key() {
    std::map<std::pair<EnumTagKey, Callback>, Callback> callbacks;
    callbacks.insert({std::make_pair(EnumTagKey::First, unrelated), {}});
    callbacks.at({EnumTagKey::First, nullptr})();
}

void map_nested_pair_key() {
    using InnerKey = std::pair<int, int>;
    using Key = std::pair<InnerKey, Callback>;
    Key key{{1, 2}, unrelated};
    std::map<Key, Callback> callbacks;
    callbacks.insert({key, {}});
    callbacks.at(key)();
}

void map_pair_container_key() {
    using Key = std::pair<Inner, Inner>;
    Key key{{unrelated}, {unrelated}};
    std::map<Key, Inner> callbacks;
    callbacks.insert({key, {}});
    callbacks.at(key)[0]();
}

void map_pair_key_entries() {
    using Key = std::pair<TagKey, Callback>;
    std::pair<Key, Callback> first{{TagKey{1}, unrelated}, alpha};
    std::map<Key, Callback> callbacks;
    callbacks.insert({first, std::make_pair(Key{TagKey{2}, unrelated}, beta)});
    callbacks.at({TagKey{1}, nullptr})();
}

void map_aggregate_key_allocator_copy() {
    Inner inner{alpha};
    std::map<CallbackKey2, Inner> callbacks;
    callbacks.insert({{unrelated, {unrelated}}, {inner, {}}});
    callbacks.at({unrelated, {}})[0]();
}

struct WrapperKey {
    WrapperKey(Callback);
};

void map_class_key_from_callback() {
    std::map<WrapperKey, Callback> callbacks;
    callbacks.insert({unrelated, alpha});
    callbacks.at(unrelated)();
}
