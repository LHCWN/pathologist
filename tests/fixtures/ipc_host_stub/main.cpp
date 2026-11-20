// The remote half of an OpenHarmony binder pair is sometimes spelled
// `*Host` instead of `*Stub` (`AppSchedulerHost`, `RenderSchedulerHost`).
// A `*Host` qualifies as a stub only when it defines the dispatcher.

class IRemoteObject {
public:
    int SendRequest(int code, void *data, void *reply, void *option);
};

IRemoteObject *Remote();

class IAppScheduler {
public:
    virtual ~IAppScheduler() = default;
    virtual bool ScheduleForegroundApplication() = 0;
    virtual void ScheduleLowMemory() = 0;
};

class AppSchedulerHost : public IAppScheduler {
public:
    int OnRemoteRequest(int code, void *data, void *reply, void *option)
    {
        (void)data;
        (void)reply;
        (void)option;
        return code;
    }
    bool HandleScheduleForegroundApplication() { return ScheduleForegroundApplication(); }
    int HandleScheduleLowMemory()
    {
        ScheduleLowMemory();
        return 0;
    }
};

class AppSchedulerProxy : public IAppScheduler {
public:
    bool ScheduleForegroundApplication() override;
    void ScheduleLowMemory() override;

private:
    int SendTransactCmd(int code);
};

int AppSchedulerProxy::SendTransactCmd(int code)
{
    void *data = 0, *reply = 0, *option = 0;
    return Remote()->SendRequest(code, data, reply, option);
}

bool AppSchedulerProxy::ScheduleForegroundApplication()
{
    return SendTransactCmd(1) == 0;
}

void AppSchedulerProxy::ScheduleLowMemory()
{
    SendTransactCmd(2);
}

// A `*Host` without a dispatcher, `Handle*` method, or binder-stub ancestry
// is not evidence of a stub, even when its method name matches a sender.
class OtherServiceHost {
public:
    int Ping() { return 3; }
};

class OtherServiceProxy {
public:
    int Ping();

private:
    int SendTransactCmd(int code);
};

int OtherServiceProxy::SendTransactCmd(int code)
{
    void *data = 0, *reply = 0, *option = 0;
    return Remote()->SendRequest(code, data, reply, option);
}

int OtherServiceProxy::Ping()
{
    return SendTransactCmd(3);
}

// A `*Host` with a dispatcher but no handler methods takes the interface
// fallback, exactly like an interface-only `*Stub`.
class IDataCallback {
public:
    virtual ~IDataCallback() = default;
    virtual int OnData(int value) = 0;
};

class DataCallbackHost : public IDataCallback {
public:
    int OnRemoteRequest(int code, void *data, void *reply, void *option)
    {
        (void)data;
        (void)reply;
        (void)option;
        return OnData(code);
    }
};

class DataCallbackService : public DataCallbackHost {
public:
    int OnData(int value) override { return value; }
};

class DataCallbackProxy : public IDataCallback {
public:
    int OnData(int value) override;

private:
    int SendTransactCmd(int code);
};

int DataCallbackProxy::SendTransactCmd(int code)
{
    void *data = 0, *reply = 0, *option = 0;
    return Remote()->SendRequest(code, data, reply, option);
}

int DataCallbackProxy::OnData(int value)
{
    return SendTransactCmd(4) + value;
}

// The dispatcher may live in another TU; a `Handle*` handler still marks the
// remote side.
class HandleOnlyHost {
public:
    int HandlePing() { return 5; }
};

class HandleOnlyProxy {
public:
    int Ping();

private:
    int SendTransactCmd(int code);
};

int HandleOnlyProxy::SendTransactCmd(int code)
{
    void *data = 0, *reply = 0, *option = 0;
    return Remote()->SendRequest(code, data, reply, option);
}

int HandleOnlyProxy::Ping()
{
    return SendTransactCmd(5);
}

// The dispatcher may also be inherited through an `IRemoteStub` base.
template <typename Interface>
class IRemoteStub : public Interface {};

class RemoteCallbackBase : public IRemoteStub<IDataCallback> {
public:
    int OnRemoteRequest(int code, void *data, void *reply, void *option)
    {
        (void)data;
        (void)reply;
        (void)option;
        return code;
    }
};

class InheritedDataHost : public RemoteCallbackBase {
public:
    int OnData(int value) override { return value; }
};

class InheritedDataProxy : public IDataCallback {
public:
    int OnData(int value) override;

private:
    int SendTransactCmd(int code);
};

int InheritedDataProxy::SendTransactCmd(int code)
{
    void *data = 0, *reply = 0, *option = 0;
    return Remote()->SendRequest(code, data, reply, option);
}

int InheritedDataProxy::OnData(int value)
{
    return SendTransactCmd(6) + value;
}

int main()
{
    AppSchedulerProxy p;
    OtherServiceProxy q;
    DataCallbackProxy r;
    HandleOnlyProxy s;
    InheritedDataProxy t;
    return p.ScheduleForegroundApplication()
        ? q.Ping() + r.OnData(1) + s.Ping() + t.OnData(1)
        : 0;
}
