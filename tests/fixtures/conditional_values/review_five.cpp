using FiveF = void(*)();
void alpha(); void beta();
struct FiveProjection { FiveF a; FiveF b; };
FiveProjection five_table[1] = {{alpha,beta}};
void five_store_projection() {
    FiveF *p=nullptr; FiveF **out=&p;
    *out=reinterpret_cast<FiveF*>(five_table);
    FiveF f=*p; f();
}
struct FivePointerField { FiveF *p; };
void five_field_projection() { FivePointerField s{}; s.p=reinterpret_cast<FiveF*>(five_table); FiveF f=*s.p; f(); }
void five_aggregate_projection() { FivePointerField s{reinterpret_cast<FiveF*>(five_table)}; FiveF f=*s.p; f(); }
struct FiveDefaultProjection { FiveF *p=reinterpret_cast<FiveF*>(five_table); };
struct FiveDefaultOwner { FiveDefaultProjection object; FiveDefaultOwner():object{} {} };
void five_default_projection() { FiveDefaultOwner owner; FiveF f=*owner.object.p; f(); }
FiveDefaultProjection five_default_object;
void five_default_global_projection() { FiveF f=*five_default_object.p; f(); }
void five_array_projection() { FiveF *p[1]={reinterpret_cast<FiveF*>(five_table)}; FiveF f=*p[0]; f(); }
void five_projection_other() { five_table[0].b(); }
FiveF five_static_cell=alpha;
struct FiveStatic {
    static void take(FiveF f=alpha) { f(); }
    static FiveF &get(FiveF &f=five_static_cell) { return f; }
    static FiveF &get_second(int=0,FiveF &f=five_static_cell) { return f; }
};
void five_static_caller() { FiveStatic::take(); FiveF *p=&FiveStatic::get(); (*p)(); }
void five_static_second() { FiveF *p=&FiveStatic::get_second(); (*p)(); }
void five_static_object() { FiveStatic s; s.take(); }
FiveF *five_static_selected=&FiveStatic::get();
void five_static_file_getter() { (*five_static_selected)(); }
struct FiveStaticPrototype { static void take(FiveF f=alpha); };
void FiveStaticPrototype::take(FiveF f) { f(); }
void five_static_prototype_caller() { FiveStaticPrototype::take(); }
struct FiveStaticOverride { static void take(FiveF f=alpha) { f(); } };
void five_static_override_caller() { FiveStaticOverride::take(beta); }
namespace FiveBlock {
void take(FiveF f=alpha);
void caller() { void take(FiveF f=beta); take(); }
void take(FiveF f) { f(); }
}
namespace FiveAlias {
void take(FiveF);
void caller() { using C=FiveF; void take(FiveF f=C(alpha)); take(); }
void take(FiveF f) { f(); }
}
namespace FiveTypedef {
void take(FiveF);
void caller() { typedef FiveF C; void take(FiveF f=C(alpha)); take(); }
void take(FiveF f) { f(); }
}
namespace FiveAliasReference {
FiveF g=alpha;
void use(FiveF *);
void caller() { using R=FiveF&; void use(FiveF *p=&R(g)); use(); }
void use(FiveF *p) { (*p)(); }
}
namespace FiveAliasNested {
FiveF leaf(FiveF f) { return f; }
void take(FiveF f) { f(); }
void caller() {
    using C=FiveF;
    FiveF leaf(FiveF f=C(alpha));
    void take(FiveF f=leaf());
    { FiveF leaf(FiveF f=beta); take(); }
}
}
