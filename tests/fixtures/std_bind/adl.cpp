namespace std {}

namespace custom {
struct Tag {};
struct Job {
    void operator()() const {}
};
Job bind(void (*callback)(), Tag tag);
}

void AdlTarget() {}
void via_adl() {
    using namespace std;
    custom::Tag tag;
    auto job = bind(AdlTarget, tag);
    job();
}
