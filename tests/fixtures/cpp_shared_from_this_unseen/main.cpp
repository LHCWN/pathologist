// The base clause names std::enable_shared_from_this even though <memory> is
// not visible. The standard intrinsics remain recognized from the class
// ancestry, so parenthesized auto initializers recover the object without
// system headers.
struct Manager : std::enable_shared_from_this<Manager> {
public:
    void Handle();
    void Run() {
        auto self(shared_from_this());
        auto task = [self]() { self->Handle(); };
        task();
    }
    void Weak() {
        auto w(weak_from_this());
        auto task = [w]() { (void)w; };
        task();
    }
};
void Manager::Handle() {}
struct Plain {
    void Run() { auto self(shared_from_this()); }
};
struct Qualified : std::enable_shared_from_this<Qualified> {
    void Run();
};
void Qualified::Run() {
    auto self(std::enable_shared_from_this<Qualified>::shared_from_this());
}
struct GlobalScope : std::enable_shared_from_this<GlobalScope> {
    void Run() { auto self(::shared_from_this()); }
};
struct Nested : std::enable_shared_from_this<Nested> {
    void Run() { auto x(shared_from_this()()); }
};
int main() {
    Manager m;
    m.Run();
    m.Weak();
    Plain p;
    p.Run();
    Qualified q;
    q.Run();
    GlobalScope g;
    g.Run();
}
