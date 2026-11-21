#include "vector.h"

using Signed = signed int;
void select_vector(std::vector<Signed*>) {}
void match_const(std::vector<int const*>) {}
void match_pointer_const(std::vector<int const**>) {}
using TypedCallback = void (*)(int const*);
void match_callback_cv(std::vector<TypedCallback>) {}
