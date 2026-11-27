#include "nested_containers.h"

using Inner = std::vector<Callback>;

void alpha() {}
void beta() {}
void unrelated() {}
void Handler::run() {}
void OtherHandler::run() {}

void vector_direct(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> nested;
    std::vector<Callback> inner;
    inner.push_back(alpha);
    nested.push_back(inner);
    nested[i][j]();
}

void vector_braced_insertion(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> nested;
    nested.push_back({alpha});
    nested[i][j]();
}

void vector_emplace(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> nested;
    std::vector<Callback> inner;
    inner.push_back(alpha);
    nested.emplace_back(inner);
    nested[i][j]();
}

void vector_braced_multi_emplace(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> nested;
    nested.emplace_back(std::initializer_list<Callback>{alpha, beta});
    nested[i][j]();
}

void vector_braced_multi_push(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> nested;
    nested.push_back({alpha, beta});
    nested[i][j]();
}

void vector_map_push_braced(unsigned i) {
    std::vector<std::map<int, Callback>> nested;
    nested.push_back({{1, alpha}});
    nested[i][1]();
}

void vector_map_push_multi(unsigned i) {
    std::vector<std::map<int, Callback>> nested;
    nested.push_back({{1, alpha}, {2, beta}});
    nested[i][1]();
}

void vector_braced_range() {
    std::vector<std::vector<Callback>> nested = {{alpha}, {beta}};
    for (auto &slot : nested) {
        for (Callback callback : slot) {
            callback();
        }
    }
}

void vector_named_range() {
    std::vector<std::vector<Callback>> nested;
    std::vector<Callback> inner;
    inner.push_back(alpha);
    nested.push_back(inner);
    for (auto &slot : nested) {
        for (Callback callback : slot) {
            callback();
        }
    }
}

void vector_field_range() {
    Holder holder;
    std::vector<Callback> inner;
    inner.push_back(alpha);
    holder.slots.push_back(inner);
    for (auto &slot : holder.slots) {
        for (Callback callback : slot) {
            callback();
        }
    }
}

void vector_isolation() {
    std::vector<std::vector<Callback>> left = {{alpha}};
    std::vector<std::vector<Callback>> right = {{unrelated}};
    for (auto &slot : left) {
        for (Callback callback : slot) {
            callback();
        }
    }
}

void pointer_range() {
    Handler handler;
    OtherHandler other;
    std::vector<std::vector<Handler *>> nested;
    std::vector<Handler *> inner;
    inner.push_back(&handler);
    nested.push_back(inner);
    std::vector<OtherHandler *> other_inner;
    other_inner.push_back(&other);
    std::vector<std::vector<OtherHandler *>> others;
    others.push_back(other_inner);
    for (auto &slot : nested) {
        for (Handler *item : slot) {
            item->run();
        }
    }
}

void map_map_subscript(unsigned i, unsigned j) {
    std::map<int, std::map<int, Callback>> nested = {{0, {{1, alpha}, {2, beta}}}};
    nested[i][j]();
}

void map_vector_subscript(unsigned i, unsigned j) {
    std::map<int, std::vector<Callback>> nested = {{0, {alpha, beta}}};
    nested[i][j]();
}

void vector_map_range() {
    std::vector<std::map<int, Callback>> nested = {{{1, alpha}}, {{2, beta}}};
    for (auto &entry : nested) {
        entry[1]();
    }
}



void map_insert_single_pair(unsigned i, unsigned j) {
    std::map<int, std::vector<Callback>> nested;
    nested.insert({1, {alpha, beta}});
    nested[i][j]();
}

void vector_insert_ambiguous(unsigned i) {
    std::vector<std::map<int, Callback>> nested;
    nested.insert(nested.end(), {{1, alpha}, {2, beta}});
    nested[i][1]();
}

void vector_descendant_copy(unsigned i, unsigned j, unsigned k) {
    std::vector<std::vector<std::vector<Callback>>> outer;
    std::vector<std::vector<Callback>> middle = {{alpha}};
    outer.push_back(middle);
    outer[i][j][k]();
}

void vector_one_element_list(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::vector<std::vector<Callback>> outer = {inner};
    outer[i][j]();
}

void typed_reference_range() {
    std::vector<std::vector<Callback>> outer = {{alpha}};
    for (std::vector<Callback> &inner : outer) {
        for (Callback callback : inner) {
            callback();
        }
    }
}

void by_value_range_isolation() {
    std::vector<std::vector<Callback>> outer = {{alpha}};
    for (auto inner : outer) {
        inner.push_back(beta);
    }
    outer[0][0]();
}

#define OUTER_INDEX [0]
void nested_ignored_bracket() {
    std::vector<std::vector<Callback>> outer = {{alpha}};
    outer OUTER_INDEX [0]();
}

void vector_triple_braced(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> nested = {{{alpha}}};
    nested[i][j]();
}

void vector_named_inner_braced_copy(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::vector<std::vector<Callback>> outer;
    outer.push_back({inner});
    outer[i][j]();
}

void map_named_pair_insert(unsigned i, unsigned j) {
    std::pair<int, std::vector<Callback>> pair;
    pair.second.push_back(alpha);
    std::map<int, std::vector<Callback>> nested;
    nested.insert(pair);
    nested[i][j]();
}

void map_without_subscript_model(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::map<int, std::vector<Callback>> nested;
    nested.insert({0, inner});
    nested[i][j]();
}

void map_inner_without_initializer() {
    std::map<int, std::vector<Callback>> nested;
    nested.insert({0, {alpha}});
    nested[0][0]();
}

void unevaluated_nested(unsigned i) {
    std::vector<std::vector<Callback>> nested = {{alpha}};
    (void)sizeof(nested[i][0]);
}

#define SLOT slots
void ignored_range_field_subscript() {
    Holder holder;
    holder.slots.push_back({alpha});
    for (Callback callback : holder.SLOT[0]) {
        callback();
    }
}

void map_pair_key_insert(unsigned i, unsigned j) {
    std::map<std::pair<int, int>, std::vector<Callback>> nested;
    nested.insert({{1, 2}, {alpha, beta}});
    nested[std::pair<int, int>{1, 2}][j]();
}

void map_pair_braced_insert(unsigned i, unsigned j) {
    std::pair<unsigned, std::vector<Callback>> pair{1u, {alpha, beta}};
    std::map<unsigned, std::vector<Callback>> nested;
    nested.insert(pair);
    nested[i][j]();
}

void map_pair_named_insert(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::pair<long, std::vector<Callback>> pair{1L, inner};
    std::map<long, std::vector<Callback>> nested;
    nested.insert(pair);
    nested[i][j]();
}

void vector_allocator_braced_push(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> nested;
    nested.push_back({{alpha, beta}, {}});
    nested[i][j]();
}

void vector_typed_inner_brace(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> nested = {
        std::vector<Callback>{alpha, beta},
    };
    nested[i][j]();
}

#define METHOD push_back
void ignored_method_macro() {
    std::vector<Callback> callbacks;
    callbacks.METHOD(alpha);
    callbacks[0]();
}

void map_insert_allocator(unsigned i, unsigned j) {
    std::map<int, std::vector<Callback>> nested;
    nested.insert({0, {{alpha, beta}, {}}});
    nested[i][j]();
}

void map_callback_key_insert() {
    std::map<Callback, std::vector<Callback>> nested;
    nested.insert({{alpha, {beta}}, {alpha, {beta}}});
    nested[alpha][0]();
}

void map_pair_braced_copy(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::pair<long, std::vector<Callback>> pair{1L, {inner}};
    std::map<long, std::vector<Callback>> nested;
    nested.insert(pair);
    nested[i][j]();
}

void map_pair_typed_brace(unsigned i, unsigned j) {
    std::pair<long long, std::vector<Callback>> pair{
        1LL, std::vector<Callback>{alpha, beta}};
    std::map<long long, std::vector<Callback>> nested;
    nested.insert(pair);
    nested[i][j]();
}

#define INNER std::vector<Callback>
void ignored_typed_insert() {
    std::vector<std::vector<Callback>> outer;
    outer.push_back(INNER{alpha});
    outer[0][0]();
}

#define PAYLOAD inner
void ignored_named_payload() {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::vector<std::vector<Callback>> outer;
    outer.push_back({PAYLOAD});
    outer[0][0]();
}

void alias_descendant_copy(unsigned i, unsigned j, unsigned k) {
    std::vector<Inner> middle = {{alpha}};
    std::vector<std::vector<std::vector<Callback>>> outer;
    outer.push_back(middle);
    outer[i][j][k]();
}

void vector_typed_brace_copy(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::vector<std::vector<Callback>> outer = {std::vector<Callback>{inner}};
    outer[i][j]();
}

void vector_nested_allocator(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer{{{alpha, beta}, {beta, alpha}}, {}};
    outer[i][j]();
}

void map_pair_key_entry(unsigned i, unsigned j) {
    std::map<std::pair<int, Callback>, std::vector<Callback>> nested;
    nested.insert({{1, unrelated}, {alpha, beta}});
    nested[std::pair<int, Callback>{1, unrelated}][j]();
}

void vector_explicit_inner_allocator(unsigned i, unsigned j) {
    std::vector<std::vector<Callback, std::allocator<Callback>>> outer;
    std::vector<Callback, std::allocator<Callback>> inner;
    inner.push_back(alpha);
    outer.push_back(inner);
    outer[i][j]();
}

void vector_nested_allocator_mixed(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer = {{{alpha, beta}, {}}, {beta}};
    outer[i][j]();
}

void map_callback_pair_key_entry() {
    std::map<std::pair<Callback, Callback>, std::vector<Callback>> nested;
    nested.insert({{unrelated, unrelated}, {alpha, beta}});
    nested[std::pair<Callback, Callback>{unrelated, unrelated}][0]();
}

void map_class_key_insert(unsigned i, unsigned j) {
    std::map<NumberKey, std::vector<Callback>> nested;
    nested.insert({1, {alpha, beta}});
    nested[NumberKey(1)][j]();
}

void custom_entry_insert(unsigned i) {
    app::Custom<app::Policy, std::vector<Callback>> custom;
    custom.insert({0, {alpha, beta}});
    custom[i][0]();
}

void vector_nested_ordinary_constructor(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer{{alpha}, {3, nullptr}};
    outer[i][j]();
}

void ignored_nested_field_bracket() {
    Holder holder;
    holder.slots.push_back({alpha});
    holder.SLOT[0][0]();
}

void map_vector_pair_key_entry() {
    using V = std::vector<Callback>;
    std::map<std::pair<V, V>, V> nested;
    nested.insert({{{unrelated}, {unrelated}}, {{alpha}, {beta}}});
    nested[std::pair<V, V>{{unrelated}, {unrelated}}][0]();
}

void map_typed_mapped_entry(unsigned i) {
    std::map<int, std::vector<Callback>> nested;
    nested.insert({{1, std::vector<Callback>{alpha, beta}}});
    nested[1][i]();
}

void vector_braced_copy_allocator(unsigned i, unsigned j) {
    std::vector<Callback> source;
    source.push_back(alpha);
    std::vector<std::vector<Callback, std::allocator<Callback>>> outer;
    outer.push_back({source});
    outer[i][j]();
}

void vector_mixed_allocator_copy(unsigned i, unsigned j) {
    std::vector<Callback, std::allocator<Callback>> middle;
    middle.push_back(alpha);
    std::vector<std::vector<Callback>> outer;
    outer.push_back(middle);
    outer[i][j]();
}

void vector_at_chain(unsigned i) {
    std::vector<std::vector<Callback>> outer = {{alpha}, {beta}};
    outer.at(i)[0]();
}

void map_at_chain(unsigned i) {
    std::map<int, std::vector<Callback>> nested = {{0, {alpha, beta}}};
    nested.at(0)[i]();
}

void vector_double_braced_push(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> nested;
    nested.push_back({{alpha}});
    nested[i][j]();
}

void vector_braced_named_copy(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::vector<std::vector<Callback>> outer = {{inner}};
    outer[i][j]();
}

void map_braced_named_insert(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::map<int, std::vector<Callback>> nested;
    nested.insert({1, {inner}});
    nested[i][j]();
}

void map_initializer_named(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::map<int, std::vector<Callback>> nested = {{1, {inner}}};
    nested[i][j]();
}

void map_typed_pair_compound_insert(unsigned i) {
    std::map<int, std::vector<Callback>> nested;
    nested.insert(std::pair<int, std::vector<Callback>>{1, {alpha, beta}});
    nested[1][i]();
}

void reference_alias_range(unsigned i) {
    using VecRef = std::vector<Callback> &;
    std::vector<std::vector<Callback>> outer = {{alpha}};
    for (VecRef slot : outer) {
        slot[i]();
        for (Callback callback : slot) {
            callback();
        }
    }
}

#define AT_METHOD at
void ignored_accessor_macro(unsigned i) {
    std::vector<std::vector<Callback>> outer = {{alpha}};
    outer.AT_METHOD(0)[i]();
}

void ignored_accessor_range() {
    std::vector<std::vector<Callback>> outer = {{alpha}};
    for (Callback callback : outer.AT_METHOD(0)) {
        callback();
    }
}

void ignored_range_bracket() {
    std::vector<std::vector<Callback>> outer = {{alpha}};
    for (Callback callback : outer OUTER_INDEX) {
        callback();
    }
}

void unevaluated_at_chain(unsigned i) {
    std::vector<std::vector<Callback>> outer = {{alpha}};
    (void)sizeof(outer.at(i)[0]);
}

void map_pair_compound_entry_list_insert(unsigned i) {
    std::map<int, std::vector<Callback>> nested;
    nested.insert({std::pair<int, std::vector<Callback>>{1, {alpha}}});
    nested[1][i]();
}

using CallbackKey = Callback;
void map_callback_alias_key_multi_insert() {
    std::map<CallbackKey, std::vector<Callback>> nested;
    nested.insert({{alpha, {beta}}, {alpha, {beta}}});
    nested[alpha][0]();
}

void ignored_nested_accessor_macro(unsigned i) {
    std::vector<std::vector<Callback>> outer = {{alpha}};
    outer[i].AT_METHOD(0)();
}

void ignored_accessor_assignment() {
    std::vector<std::vector<Callback>> outer(1);
    outer[0].AT_METHOD(0) = alpha;
    outer[0][0]();
}

void map_const_key_insert(unsigned i) {
    std::map<const int, std::vector<Callback>> nested;
    nested.insert({1, {alpha}});
    nested[1][i]();
}

void map_allocator_entry_list(unsigned i, unsigned j) {
    std::map<int, std::vector<Callback>> nested = {
        {{1, {alpha}}, {2, {beta}}}, {}};
    nested[i][j]();
}

void map_aggregate_key_multi_insert() {
    std::map<CallbackKey2, std::vector<Callback>> nested;
    nested.insert({{{unrelated, {}}, {alpha}}, {{unrelated, {}}, {beta}}});
    nested[CallbackKey2{unrelated, {}}][0]();
}

void map_typed_pair_initializer(unsigned i) {
    std::map<int, std::vector<Callback>> nested = {
        std::pair<int, std::vector<Callback>>{1, {alpha}}};
    nested[1][i]();
}

void vector_map_typed_pair_initializer(unsigned i) {
    std::vector<std::map<int, std::vector<Callback>>> outer = {
        {std::pair<int, std::vector<Callback>>{1, {alpha}}}};
    outer[0][1][i]();
}

void map_allocator_aggregate_key(unsigned i) {
    std::map<CallbackKey2, std::vector<Callback>> nested = {
        {{{unrelated, {}}, {alpha}}, {{unrelated, {}}, {beta}}}, {}};
    nested[CallbackKey2{unrelated, {}}][i]();
}

void map_mixed_pair_insert(unsigned i) {
    std::map<int, CallbackVecAlt> nested;
    std::pair<int, CallbackVecAlt> named{2, {beta}};
    nested.insert({{1, {alpha}}, named});
    nested[1][i]();
    nested[2][i]();
}

void map_three_entry_named_insert() {
    std::map<CallbackKey2, std::vector<Callback>> nested;
    std::pair<CallbackKey2, std::vector<Callback>> trailing{
        CallbackKey2{unrelated, {}}, {}};
    nested.insert({{{unrelated, {}}, {alpha}}, {{unrelated, {}}, {beta}}, trailing});
    nested[CallbackKey2{unrelated, {}}][0]();
}


void unevaluated_chained_accessor(unsigned i) {
    std::vector<std::vector<std::vector<Callback>>> outer = {{{alpha}}};
    (void)sizeof(outer[0].at(i)[0]);
}

using CVec = const std::vector<Callback>;
using CPair = const std::pair<int, std::vector<Callback>>;

void map_insert_inner_allocator_one(unsigned i, unsigned j) {
    std::map<int, std::vector<Callback>> nested;
    nested.insert({1, {{alpha}, {}}});
    nested[i][j]();
}

void map_insert_triple_single(unsigned i, unsigned j, unsigned k) {
    std::map<int, std::vector<std::vector<Callback>>> nested;
    nested.insert({1, {{alpha}, {beta}}});
    nested[i][j][k]();
}

void map_pair_key_map_single(unsigned i, unsigned j) {
    std::map<std::pair<int, int>, std::map<int, Callback>> nested;
    nested.insert({{1, 2}, {{3, alpha}, {4, beta}}});
    nested[std::pair<int, int>{1, 2}][i]();
}

void vector_position_named_inner(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer;
    std::vector<Callback> first;
    std::vector<Callback> second;
    first.push_back(alpha);
    second.push_back(beta);
    outer.insert(outer.end(), {first, second});
    outer[i][j]();
}

void vector_position_inner_lists(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer;
    outer.insert(outer.end(), {{alpha, beta}, {beta}});
    outer[i][j]();
}

void vector_const_alias_typed_inner(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer;
    outer.push_back(CVec{alpha});
    outer[i][j]();
}

void map_const_pair_alias_insert(unsigned i) {
    std::map<int, std::vector<Callback>> nested;
    nested.insert(CPair{1, {alpha}});
    nested[1][i]();
}

void map_initializer_mixed_named(unsigned i) {
    std::pair<int, CallbackVecAlt> named{2, {beta}};
    std::map<int, CallbackVecAlt> nested = {{1, {alpha}}, named};
    nested[1][i]();
    nested[2][i]();
}

void map_initializer_mixed_make_pair(unsigned i) {
    std::map<int, CallbackVecAlt> nested = {
        {1, {alpha}}, std::make_pair(2, CallbackVecAlt{beta})};
    nested[1][i]();
    nested[2][i]();
}

void map_insert_named_first(unsigned i) {
    std::map<int, CallbackVecAlt> nested;
    std::pair<int, CallbackVecAlt> first;
    first.second.push_back(alpha);
    CallbackVecAlt second;
    second.push_back(beta);
    nested.insert({first, {2, second}});
    nested[1][i]();
    nested[2][i]();
}

void map_insert_make_pair_first(unsigned i) {
    std::map<int, CallbackVecAlt> nested;
    nested.insert({std::make_pair(1, CallbackVecAlt{alpha}), {2, {beta}}});
    nested[1][i]();
    nested[2][i]();
}

void map_aggregate_key_single_pair() {
    std::map<CallbackKey2, std::vector<Callback>> nested;
    nested.insert({{unrelated, {alpha}}, {beta, beta}});
    nested[CallbackKey2{unrelated, {}}][0]();
}

std::vector<Callback> make_callbacks();

using CCallbackKey = const Callback;
void map_const_callback_alias_key() {
    std::map<CCallbackKey, std::vector<Callback>> nested;
    nested.insert({{alpha, {beta}}, {alpha, {beta}}});
    nested[alpha][0]();
}

void map_pair_empty_key_insert(unsigned i) {
    std::map<std::pair<int, int>, std::vector<Callback>> nested;
    nested.insert({{}, {alpha}});
    nested[std::pair<int, int>{}][i]();
}

void map_pair_empty_key_initializer(unsigned i) {
    std::map<std::pair<int, int>, std::vector<Callback>> nested = {
        {{}, {alpha}}, {{1, 2}, {beta}}};
    nested[std::pair<int, int>{}][i]();
    nested[std::pair<int, int>{1, 2}][i]();
}

void map_direct_typed_pair_non_nested(unsigned i) {
    std::map<int, Callback> nested;
    nested.insert(std::pair<int, Callback>{1, alpha});
    nested[1]();
}

void map_single_unmodeled_mapped() {
    std::map<CallbackKey2, std::vector<Callback>> nested;
    nested.insert({{unrelated, {alpha}}, make_callbacks()});
    nested[CallbackKey2{unrelated, {}}][0]();
}

void map_pair_key_callback_non_nested(unsigned i) {
    std::map<std::pair<int, int>, Callback> nested;
    nested.insert({{1, 2}, alpha});
    nested[std::pair<int, int>{1, 2}]();
}

void map_pair_callback_key_non_nested() {
    std::map<std::pair<int, Callback>, Callback> nested;
    nested.insert({{1, beta}, alpha});
    nested[std::pair<int, Callback>{1, beta}]();
}

void map_named_pair_key_non_nested() {
    std::pair<int, int> key{1, 2};
    std::map<std::pair<int, int>, Callback> nested;
    nested.insert({key, alpha});
    nested[std::pair<int, int>{1, 2}]();
}

void map_make_pair_key_non_nested() {
    std::map<std::pair<int, int>, Callback> nested;
    nested.insert({std::make_pair(1, 2), alpha});
    nested[std::pair<int, int>{1, 2}]();
}

void map_braced_mapped_non_nested() {
    std::map<std::pair<int, int>, Callback> nested;
    nested.insert({{1, 2}, {alpha}});
    nested[std::pair<int, int>{1, 2}]();
}

std::pair<int, Callback> make_callback_entry();

void map_insert_braced_then_unmodeled() {
    std::map<int, Callback> nested;
    nested.insert({{1, alpha}, make_callback_entry()});
    nested[1]();
}

void map_insert_unmodeled_then_braced() {
    std::map<int, Callback> nested;
    nested.insert({make_callback_entry(), {2, alpha}});
    nested[2]();
}

void pair_container_field_copy(unsigned i) {
    std::pair<int, CallbackVecPairCopy> src{1, {alpha}};
    std::pair<long, CallbackVecPairCopy> dst(src);
    std::map<long, CallbackVecPairCopy> nested;
    nested.insert(dst);
    nested[1L][i]();
}

void pair_container_field_assignment(unsigned i) {
    std::pair<int, CallbackVecPairCopy> src{1, {alpha}};
    std::pair<long, CallbackVecPairCopy> dst;
    dst = src;
    std::map<long, CallbackVecPairCopy> nested;
    nested.insert(dst);
    nested[1L][i]();
}

void map_empty_inner_brace_without_initializer(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::map<int, std::vector<Callback>> nested = {{1, {}}, {2, inner}};
    nested[2][j]();
}

void map_initializer_default_entry() {
    std::map<int, Callback> nested = {{1, alpha}, {}, {2, beta}};
    nested[1]();
    nested[2]();
}

std::pair<int, Callback> get_callback_pair();

void map_initializer_unmodeled_pair() {
    std::map<int, Callback> nested = {{1, alpha}, get_callback_pair()};
    nested[1]();
}

void map_copy_list_pair_key() {
    std::pair<int, int> key{1, 2};
    std::map<std::pair<int, int>, Callback> nested = {{{key}, alpha}};
    nested[std::pair<int, int>{1, 2}]();
}

void vector_allocator_extended_copy(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::vector<std::vector<Callback>> outer;
    outer.push_back({inner, std::allocator<Callback>{}});
    outer[i][j]();
}

void vector_empty_map_without_initializer(unsigned i) {
    std::map<int, Callback> populated;
    populated.insert({1, alpha});
    std::vector<std::map<int, Callback>> outer{{}, populated};
    outer[i][1]();
}

#define IDX [0]
void ignored_receiver_chain_macro() {
    std::vector<Holder *> holders;
    Holder holder;
    holders.push_back(&holder);
    holder.slots.push_back({alpha});
    holders IDX ->slots[0][0]();
}

void ignored_accessor_receiver_chain_macro() {
    std::vector<Holder *> holders;
    Holder holder;
    holders.push_back(&holder);
    holder.slots.push_back({alpha});
    holders.AT_METHOD(0)->slots[0][0]();
}

void ignored_parenthesized_receiver_chain_macro() {
    std::vector<Holder *> holders;
    Holder holder;
    holders.push_back(&holder);
    holder.slots.push_back({alpha});
    (holders IDX)->slots[0][0]();
}

void map_entry_conversion() {
    std::map<int, Callback> nested = {{1, alpha}, ConvertibleEntry{}};
    nested[1]();
}

std::vector<Callback> make_inner();

void vector_returned_inner(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer{make_inner(), {beta}};
    outer[i][j]();
}

std::pair<std::pair<int, int>, std::vector<Callback>> make_nested_entry();

void map_insert_braced_then_pair_return(unsigned i) {
    std::map<std::pair<int, int>, std::vector<Callback>> nested;
    nested.insert({{{1, 2}, {alpha}}, make_nested_entry()});
    nested[std::pair<int, int>{1, 2}][i]();
}

void vector_initializer_allocator_copy(unsigned i, unsigned j) {
    std::map<int, Callback> inner;
    inner.insert({1, alpha});
    std::vector<std::map<int, Callback>> outer{{inner, {}}, {{2, beta}}};
    outer[i][j]();
}

void vector_parenthesized_typed_inner(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer{
        (std::vector<Callback>{beta}), {alpha}};
    outer[i][j]();
}

void map_converting_pair_key() {
    std::pair<int, int> key{1, 2};
    std::map<std::pair<long, int>, Callback> nested;
    nested.insert({{key}, alpha});
    nested[std::pair<long, int>{1, 2}]();
}

void map_convertible_mapped_value() {
    ConvertibleCallback value;
    std::map<long, Callback> nested = {{1, alpha}, std::make_pair(2L, value)};
    nested[1L]();
}

void map_single_pair_empty_mapped() {
    std::map<std::pair<int, Callback>, Callback> nested;
    nested.insert({{1, alpha}, {}});
    nested[std::pair<int, Callback>{1, alpha}]();
}

void map_initializer_trailing_empty(unsigned i) {
    std::map<int, Callback> nested{{1, alpha}, {}};
    nested[1]();
}

void map_initializer_trailing_empty_nested(unsigned i, unsigned j) {
    std::map<int, std::vector<Callback>> nested{{1, {alpha}}, {}};
    nested[1][j]();
}

void vector_braced_typed_temporary_copy(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer;
    outer.push_back({std::vector<Callback>{alpha}});
    outer[i][j]();
}

void map_insert_empty_then_make_pair() {
    std::map<int, Callback> nested;
    nested.insert({{}, std::make_pair(1, alpha)});
    nested[1]();
}

void vector_callback_param_spelling(unsigned i) {
    std::vector<void (*)(void)> inner;
    inner.push_back(alpha);
    std::vector<std::vector<Callback>> outer;
    outer.push_back(inner);
    outer[i][0]();
}

void map_initializer_aggregate_trailing_empty() {
    std::map<Key, Callback> nested = {{{1, 2}, alpha}, {}};
    nested[Key{1, 2}]();
}

void map_initializer_aggregate_trailing_empty_nested() {
    std::map<Key, std::vector<Callback>> nested = {{{1, 2}, {alpha}}, {}};
    nested[Key{1, 2}][0]();
}

void map_insert_inner_call_aggregate_key(unsigned i) {
    std::map<Key, std::vector<Callback>> nested;
    nested.insert({{{1, 2}, {alpha}}, {{3, 4}, make_inner()}});
    nested[Key{1, 2}][i]();
}

void vector_braced_typed_named_copy(unsigned i, unsigned j) {
    std::vector<Callback> inner;
    inner.push_back(alpha);
    std::vector<std::vector<Callback>> outer;
    outer.push_back({std::vector<Callback>{inner}});
    outer[i][j]();
}

void vector_braced_typed_allocator_copy(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer;
    outer.push_back({std::vector<Callback>{alpha}, {}});
    outer[i][j]();
}

void vector_braced_typed_temporary_initializer(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer = {
        {std::vector<Callback>{alpha}}};
    outer[i][j]();
}

void map_pair_compound_key() {
    std::map<std::pair<int, int>, Callback> nested = {
        {{std::pair<int, int>{1, 2}}, alpha}};
    nested[std::pair<int, int>{1, 2}]();
}

void map_three_level_single_pair(unsigned i) {
    std::map<std::pair<int, int>, std::vector<std::vector<Callback>>> nested;
    nested.insert({{1, 2}, {{3, nullptr}, {{alpha}}}});
    nested[std::pair<int, int>{1, 2}][0][i]();
}

void map_insert_aggregate_key_empty_mapped() {
    std::map<AggregateKey, Callback> nested;
    nested.insert({{1, unrelated}, {}});
    nested[AggregateKey{1, nullptr}]();
}

void map_insert_pair_key_integer_variable() {
    int k = 1;
    std::map<std::pair<int, Callback>, Callback> nested;
    nested.insert({{k, unrelated}, {}});
    nested[std::pair<int, Callback>{1, nullptr}]();
}

void map_parenthesized_pair_compound(unsigned i) {
    std::map<int, std::vector<Callback>> nested = {
        (std::pair<int, std::vector<Callback>>{1, {alpha}})};
    nested[1][i]();
}

void map_insert_parenthesized_pair_compound(unsigned i) {
    std::map<int, std::vector<Callback>> nested;
    nested.insert((std::pair<int, std::vector<Callback>>{1, {alpha}}));
    nested[1][i]();
}

void vector_initializer_list_temporary(unsigned i, unsigned j) {
    std::vector<std::vector<Callback>> outer = {
        std::initializer_list<Callback>{alpha, beta}};
    outer[i][j]();
}

void map_initializer_list_temporary(unsigned i) {
    std::map<int, std::vector<Callback>> nested = {
        {1, std::initializer_list<Callback>{alpha, beta}}};
    nested[1][i]();
}

void map_insert_initializer_list_temporary(unsigned i) {
    std::map<int, std::vector<Callback>> nested;
    nested.insert({1, std::initializer_list<Callback>{alpha, beta}});
    nested[1][i]();
}

void ignored_range_pointer_macro() {
    RawHolder holder;
    holder.items[0] = alpha;
    std::vector<RawHolder *> holders;
    holders.push_back(&holder);
    for (Callback callback : (*holders IDX).items) {
        callback();
    }
}

void map_pair_key_as_entry_non_nested() {
    std::pair<int, Callback> key{1, unrelated};
    std::map<std::pair<int, Callback>, Callback> nested;
    nested.insert({key, {}});
    nested[std::pair<int, Callback>{1, unrelated}]();
}

void map_make_pair_key_as_entry_non_nested() {
    std::map<std::pair<int, Callback>, Callback> nested;
    nested.insert({std::make_pair(1, unrelated), {}});
    nested[std::pair<int, Callback>{1, unrelated}]();
}

void map_insert_widening_keys() {
    std::pair<short, Callback> first{1, alpha};
    std::map<long, Callback> callbacks;
    callbacks.insert({first, std::make_pair(2, beta)});
    callbacks[1L]();
}

void map_insert_converting_class_keys(unsigned i) {
    using Values = std::vector<Callback, std::allocator<char>>;
    std::pair<int, Values> first{1, {alpha}};
    Values second{beta};
    std::map<NumberKey, Values> callbacks;
    callbacks.insert({first, std::make_pair(2, second)});
    callbacks[NumberKey(1)][i]();
}

void map_insert_qualified_callback_keys() {
    Callback first = unrelated;
    Callback second = alpha;
    std::map<const Callback, Callback> callbacks;
    callbacks.insert({std::make_pair(first, alpha), std::make_pair(second, beta)});
    callbacks[first]();
}

void map_insert_qualified_pointer_keys(int *first, int *second) {
    std::map<const int *, Callback> callbacks;
    callbacks.insert({std::make_pair(first, alpha), std::make_pair(second, beta)});
    callbacks[first]();
}
