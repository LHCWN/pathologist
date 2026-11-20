// Both the *Stub and *Host spellings are indexed for one proxy. Both are
// plausible remote sides, so the detector evaluates both instead of stopping
// at the first candidate in name order.

class IRemoteObject {
public:
    int SendRequest(int code, void *data, void *reply, void *option);
};

IRemoteObject *Remote();

class IBoth {
public:
    virtual ~IBoth() = default;
    virtual int Run(int value) = 0;
};

class BothStub : public IBoth {
public:
    int OnRemoteRequest(int code, void *data, void *reply, void *option)
    {
        (void)data;
        (void)reply;
        (void)option;
        return code;
    }
    int Run(int value) override { return value; }
};

class BothHost : public IBoth {
public:
    int OnRemoteRequest(int code, void *data, void *reply, void *option)
    {
        (void)data;
        (void)reply;
        (void)option;
        return code;
    }
    int Run(int value) override { return value; }
};

class BothProxy : public IBoth {
public:
    int Run(int value) override;

private:
    int SendTransactCmd(int code);
};

int BothProxy::SendTransactCmd(int code)
{
    void *data = 0, *reply = 0, *option = 0;
    return Remote()->SendRequest(code, data, reply, option);
}

int BothProxy::Run(int value)
{
    return SendTransactCmd(1) + value;
}

// Both candidates are dispatcher-only and share the interface base, so the
// fallback resolves the same inherited declaration for both. The detector
// must not emit that pair twice.
class IShared {
public:
    virtual ~IShared() = default;
    virtual int Run(int value) = 0;
};

class SharedStub : public IShared {
public:
    int OnRemoteRequest(int code, void *data, void *reply, void *option)
    {
        (void)data;
        (void)reply;
        (void)option;
        return code;
    }
};

class SharedHost : public IShared {
public:
    int OnRemoteRequest(int code, void *data, void *reply, void *option)
    {
        (void)data;
        (void)reply;
        (void)option;
        return code;
    }
};

class SharedProxy : public IShared {
public:
    int Run(int value) override;

private:
    int SendTransactCmd(int code);
};

int SharedProxy::SendTransactCmd(int code)
{
    void *data = 0, *reply = 0, *option = 0;
    return Remote()->SendRequest(code, data, reply, option);
}

int SharedProxy::Run(int value)
{
    return SendTransactCmd(2) + value;
}

int main()
{
    BothProxy proxy;
    SharedProxy shared;
    return proxy.Run(1) + shared.Run(1);
}
