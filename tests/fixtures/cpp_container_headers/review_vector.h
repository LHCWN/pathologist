#pragma once

namespace std {
inline namespace __1 {
template<class T> class vector {
public:
    void push_back(T);
    T& operator[](unsigned);
};
}
}
