int local_seed();
void before();
void after();

struct WithDefault {
    int v = local_seed();
};

void Build(bool c)
{
    before();
    if (c) {
        WithDefault object;
        after();
    }
}

int self_seed();

struct SelfBuilding {
    SelfBuilding *next = new SelfBuilding();
    int v = self_seed();
};

void BuildSelf()
{
    SelfBuilding object;
}
