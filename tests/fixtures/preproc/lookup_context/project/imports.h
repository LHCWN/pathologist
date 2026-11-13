#pragma once
#include "decls.h"
#define IMPORT_MAKE using SDK::Make
IMPORT_MAKE;
using SDK::GetHandler;
namespace Client {
using SDK::Make;
inline void HeaderRun() { Make(1)->GoodSDK(); auto callback = GetHandler(); callback(); }
inline void DirectCallback() { SDK::Callback(); }
inline void GlobalRun() { SDK::shared.GoodSDK(); }
inline void InitRun() { SDK::UsesDefault value; }
#ifdef TEST_STATIC_CONTEXT
inline void StaticRun() { SDK::slot(); }
#endif
}
