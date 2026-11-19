#include "vector.h"

using Unsigned = unsigned int;
void select_vector(std::vector<Unsigned*>) {}
void select_vector(std::vector<unsigned int*>);
using IntPointer = int*;
void match_pointer_const(std::vector<IntPointer const*>) {}
void match_pointer_const(std::vector<int* const*>);
