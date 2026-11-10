int HeaderLeaf();

struct HeaderOwner {
    int v = HeaderLeaf();
    HeaderOwner() {}
};

struct HeaderImplicit {
    int v = HeaderLeaf();
};

inline void BuildHeaderImplicit() {
    HeaderImplicit h;
    (void)h;
}
