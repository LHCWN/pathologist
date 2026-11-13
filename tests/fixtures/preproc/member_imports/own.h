#pragma once
namespace Local {
struct Result { void Touch(); };
struct Other { void Wrong(); };
struct Base { Result *Select(int); };
struct Derived : Base {
    using Base::Select;
    Other *Select();
};
}
