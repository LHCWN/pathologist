#pragma once
// A header's preamble unit drops its lambdas; these sites bind after merge.
int HeaderLeaf();
inline int InHeader() { return [] { return HeaderLeaf(); }(); }

// Written by a macro and shared by every including unit: still one function.
#define HEADER_CALL(theCall) ([&]() { auto retVal = theCall; return retVal; }())
inline int InHeaderMacro() { return HEADER_CALL(HeaderLeaf()); }

struct HeaderHolder {
    int Run() { return [this] { return HeaderLeaf(); }(); }
};
