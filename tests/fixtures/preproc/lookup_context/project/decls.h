#pragma once
#include <api>
namespace SDK {
struct Obj { void GoodSDK(); };
Obj *Make(int);
extern Obj shared;
Obj *MakeDefault();
struct UsesDefault { int value = (MakeDefault()->GoodSDK(), 0); };
void Leaf();
static void Callback() { Leaf(); }
using Handler = void (*)();
#ifdef TEST_STATIC_CONTEXT
static Handler slot = Callback;
#endif
inline Handler GetHandler() { return Callback; }
}
