struct Global { void GoodGlobal(); };
Global *Make(long);
void Before() { Make(1)->GoodGlobal(); };
#include "imports.h"
void After() { Make(1)->GoodSDK(); }
