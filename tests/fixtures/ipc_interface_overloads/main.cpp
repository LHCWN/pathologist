// The interface fallback can tell overloads apart: the proxy method and the
// service method it implements share one interface signature, so only the
// same-arity, same-parameter overload is a candidate. Handler-name tiers are
// unaffected and still keep every possible overload.

class IRemoteObject {
public:
    int SendRequest(int code, void *data, void *reply, void *option);
};

IRemoteObject *Remote();

class IAbilityManager {
public:
    virtual ~IAbilityManager() = default;
    virtual int StartAbility(int a, int b) = 0;
    virtual int StartAbility(void *token, int a, int b) = 0;
};

class AbilityManagerStub : public IAbilityManager {
public:
    int OnRemoteRequest(int code, void *data, void *reply, void *option)
    {
        (void)data;
        (void)reply;
        (void)option;
        return code;
    }
    int StartAbilityInner() { return StartAbility(1, 2); }
    int StartAbilityTokenInner() { return StartAbility(nullptr, 1, 2); }
};

class AbilityManagerService : public AbilityManagerStub {
public:
    int StartAbility(int a, int b) override { return a + b; }
    int StartAbility(void *token, int a, int b) override
    {
        return token == nullptr ? a + b : a;
    }
};

class AbilityManagerProxy : public IAbilityManager {
public:
    int StartAbility(int a, int b) override;
    int StartAbility(void *token, int a, int b) override;

private:
    int SendRequest(int code);
};

int AbilityManagerProxy::SendRequest(int code)
{
    void *data = 0, *reply = 0, *option = 0;
    return Remote()->SendRequest(code, data, reply, option);
}

int AbilityManagerProxy::StartAbility(int a, int b)
{
    return SendRequest(1) + a + b;
}

int AbilityManagerProxy::StartAbility(void *token, int a, int b)
{
    (void)token;
    return SendRequest(2) + a + b;
}

int main()
{
    AbilityManagerProxy p;
    return p.StartAbility(1, 2) + p.StartAbility(nullptr, 3, 4);
}
