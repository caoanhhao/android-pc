// Codec2 AIDL IComponentStore/default of gts7l: the V4L2 components of external/v4l2_codec2
// (c2.v4l2.*) on the SM8250 video codec (qcom-iris, /dev/video*). v4l2_codec2's own service
// is HIDL only, which the AIDL HAL selection (media.c2.hal.selection=aidl) never asks, so this
// wraps its store in libcodec2_aidl's ComponentStore instead.

#define LOG_TAG "android.hardware.media.c2-service.gts7l"

#include <android-base/logging.h>
#include <android/binder_manager.h>
#include <android/binder_process.h>
#include <codec2/aidl/ComponentStore.h>
#include <minijail.h>
#include <signal.h>
#include <v4l2_codec2/v4l2/V4L2ComponentStore.h>

static constexpr char kBaseSeccompPolicyPath[] =
        "/vendor/etc/seccomp_policy/android.hardware.media.c2-default-seccomp_policy";
static constexpr char kExtSeccompPolicyPath[] =
        "/vendor/etc/seccomp_policy/android.hardware.media.c2-extended-seccomp_policy";

int main() {
    android::base::InitLogging(nullptr, android::base::LogdLogger());
    signal(SIGPIPE, SIG_IGN);
    android::SetUpMinijail(kBaseSeccompPolicyPath, kExtSeccompPolicyPath);

    ABinderProcess_setThreadPoolMaxThreadCount(16);
    ABinderProcess_startThreadPool();

    using namespace ::aidl::android::hardware::media::c2;
    std::shared_ptr<C2ComponentStore> v4l2 = android::V4L2ComponentStore::Create();
    if (v4l2 == nullptr) {
        LOG(ERROR) << "No V4L2 component store";
        return 1;
    }
    auto store = ::ndk::SharedRefBase::make<utils::ComponentStore>(v4l2);
    const std::string instance = std::string(IComponentStore::descriptor) + "/default";
    if (AServiceManager_addService(store->asBinder().get(), instance.c_str()) != EX_NONE) {
        LOG(ERROR) << "Cannot register " << instance;
        return 1;
    }
    LOG(INFO) << instance << " registered";
    ABinderProcess_joinThreadPool();
    return 1;
}
