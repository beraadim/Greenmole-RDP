/* Greenmole input-only Core Audio AudioServerPlugIn.
 * Original implementation against Apple's public AudioServerPlugIn interface.
 * The device never captures hardware and exposes no output or default-output role.
 */
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CFPlugInCOM.h>
#include <libproc.h>
#include <mach/mach_time.h>
#include <math.h>
#include <os/log.h>
#include <pthread.h>
#include <stdatomic.h>
#include <sys/proc_info.h>
#include <sys/stat.h>
#include "MediaRing.h"
#include "DriverProperties.h"

_Static_assert(ATOMIC_LLONG_LOCK_FREE == 2, "Real-time ring requires lock-free 64-bit atomics");

static AudioServerPlugInDriverInterface gInterface;
static AudioServerPlugInDriverInterface *gInterfacePtr = &gInterface;
static AudioServerPlugInDriverRef gDriver = &gInterfacePtr;
static _Atomic uint32_t gReferences = 1, gRunning;
static uint64_t gAnchor;
static double gTicksPerFrame;
static uint64_t gLeaseTicks;
static GMRing gRing;
static pthread_mutex_t gPropertyLock = PTHREAD_MUTEX_INITIALIZER;
static pid_t gProducer;
static uint64_t gProducerStart, gGeneration, gNextFrame;
static _Atomic uint64_t gEpoch = 1, gLease;
static _Atomic uint32_t gProducerUID;
/* Publish UID and HAL client ID together; separate atomics would permit an old
 * UID to authorize a reused client ID during concurrent removal/registration. */
typedef struct {
    _Atomic uint64_t identity;
} GMClient;
static GMClient gClients[MAX_CLIENTS];

/* Media revocation includes time spent asleep; the device sample timeline uses
 * mach_absolute_time independently because Core Audio host timestamps do too. */
static uint64_t lease_now(void) {
    return mach_continuous_time();
}
static uint64_t frame_now(void) {
    return (uint64_t)((double)(mach_absolute_time() - gAnchor) / gTicksPerFrame);
}
static bool caller(pid_t pid, uid_t *uid, uint64_t *start) {
#ifdef GM_TESTING
    extern bool gm_test_caller(pid_t, uid_t *, uint64_t *);
    return gm_test_caller(pid, uid, start);
#else
    struct proc_bsdinfo info;
    if (pid <= 0 || proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info)) != sizeof(info))
        return false;
    if (!info.pbi_uid || info.pbi_uid != info.pbi_ruid)
        return false;
    *uid = info.pbi_uid;
    *start = info.pbi_start_tvsec * 1000000ull + info.pbi_start_tvusec;
    return true;
#endif
}
static bool console_uid(uid_t *uid) {
#ifdef GM_TESTING
    extern bool gm_test_console(uid_t *);
    return gm_test_console(uid);
#else
    struct stat st;
    if (stat("/dev/console", &st) || !st.st_uid)
        return false;
    *uid = st.st_uid;
    return true;
#endif
}
static HRESULT query(void *driver, REFIID iid, LPVOID *out) {
    if (!out || driver != gDriver)
        return E_NOINTERFACE;
    *out = NULL;
    CFUUIDRef uuid = CFUUIDCreateFromUUIDBytes(NULL, iid);
    bool found =
        CFEqual(uuid, IUnknownUUID) || CFEqual(uuid, kAudioServerPlugInDriverInterfaceUUID);
    CFRelease(uuid);
    if (!found)
        return E_NOINTERFACE;
    atomic_fetch_add(&gReferences, 1);
    *out = gDriver;
    return S_OK;
}
static ULONG retain(void *d) {
    return d == gDriver ? atomic_fetch_add(&gReferences, 1) + 1 : 0;
}
static ULONG release(void *d) {
    if (d != gDriver)
        return 0;
    uint32_t count = atomic_load(&gReferences);
    while (count > 1 && !atomic_compare_exchange_weak(&gReferences, &count, count - 1)) {
    }
    return count > 1 ? count - 1 : count;
}
__attribute__((visibility("default"))) void *GreenmoleMicrophone_Create(CFAllocatorRef allocator,
                                                                        CFUUIDRef type) {
    (void)allocator;
    if (!CFEqual(type, kAudioServerPlugInTypeUUID))
        return NULL;
    return gDriver;
}
static OSStatus initialize(AudioServerPlugInDriverRef d, AudioServerPlugInHostRef host) {
    (void)host;
    if (d != gDriver)
        return kAudioHardwareBadObjectError;
    mach_timebase_info_data_t t;
    mach_timebase_info(&t);
    gTicksPerFrame = 1e9 * t.denom / ((double)t.numer * GM_SAMPLE_RATE);
    gLeaseTicks = (uint64_t)(0.25 * 1e9 * t.denom / t.numer);
    gAnchor = mach_absolute_time();
    os_log(OS_LOG_DEFAULT,
           "Greenmole microphone initialized; input-only, 48 kHz stereo, protocol 1");
    return 0;
}
static OSStatus create_device(AudioServerPlugInDriverRef d, CFDictionaryRef desc,
                              const AudioServerPlugInClientInfo *client, AudioObjectID *out) {
    (void)d;
    (void)desc;
    (void)client;
    (void)out;
    return kAudioHardwareUnsupportedOperationError;
}
static OSStatus destroy_device(AudioServerPlugInDriverRef d, AudioObjectID id) {
    (void)d;
    (void)id;
    return kAudioHardwareUnsupportedOperationError;
}
static OSStatus add_client(AudioServerPlugInDriverRef d, AudioObjectID id,
                           const AudioServerPlugInClientInfo *client) {
    if (d != gDriver || !device(id) || !client || !client->mClientID)
        return kAudioHardwareBadObjectError;
    uid_t uid;
    uint64_t start;
    if (!caller(client->mProcessID, &uid, &start))
        return kAudioHardwareIllegalOperationError;
    (void)start;
    pthread_mutex_lock(&gPropertyLock);
    for (unsigned i = 0; i < MAX_CLIENTS; ++i) {
        uint64_t identity = atomic_load(&gClients[i].identity);
        if (!identity || (uint32_t)identity == client->mClientID) {
            atomic_store_explicit(&gClients[i].identity, (uint64_t)uid << 32 | client->mClientID,
                                  memory_order_release);
            pthread_mutex_unlock(&gPropertyLock);
            return 0;
        }
    }
    pthread_mutex_unlock(&gPropertyLock);
    return kAudioHardwareIllegalOperationError;
}
static OSStatus remove_client(AudioServerPlugInDriverRef d, AudioObjectID id,
                              const AudioServerPlugInClientInfo *client) {
    if (d != gDriver || !device(id) || !client)
        return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gPropertyLock);
    for (unsigned i = 0; i < MAX_CLIENTS; ++i)
        if ((uint32_t)atomic_load(&gClients[i].identity) == client->mClientID)
            atomic_store_explicit(&gClients[i].identity, 0, memory_order_release);
    pthread_mutex_unlock(&gPropertyLock);
    return 0;
}
static OSStatus configuration(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 action,
                              void *info) {
    (void)action;
    (void)info;
    return d == gDriver && device(id) ? 0 : kAudioHardwareBadObjectError;
}

static Boolean has_property(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid,
                            const AudioObjectPropertyAddress *a) {
    (void)pid;
    UInt32 size;
    return d == gDriver &&
           gm_property(id, a, 0, NULL, 0, &size, NULL, true, atomic_load(&gRunning)) == 0;
}
static OSStatus settable(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid,
                         const AudioObjectPropertyAddress *a, Boolean *out) {
    if (!out || !has_property(d, id, pid, a))
        return kAudioHardwareUnknownPropertyError;
    *out = (id == DEVICE && (a->mSelector == GM_PROP_FRAME ||
                             a->mSelector == kAudioDevicePropertyNominalSampleRate)) ||
           (id == STREAM && (a->mSelector == kAudioStreamPropertyPhysicalFormat ||
                             a->mSelector == kAudioStreamPropertyVirtualFormat ||
                             a->mSelector == kAudioStreamPropertyIsActive));
    return 0;
}
static OSStatus data_size(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid,
                          const AudioObjectPropertyAddress *a, UInt32 qn, const void *q,
                          UInt32 *out) {
    (void)pid;
    if (d != gDriver || !out)
        return kAudioHardwareBadObjectError;
    return gm_property(id, a, qn, q, 0, out, NULL, true, atomic_load(&gRunning));
}
static OSStatus get_data(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid,
                         const AudioObjectPropertyAddress *a, UInt32 qn, const void *q, UInt32 n,
                         UInt32 *written, void *out) {
    (void)pid;
    if (d != gDriver)
        return kAudioHardwareBadObjectError;
    return gm_property(id, a, qn, q, n, written, out, false, atomic_load(&gRunning));
}
static OSStatus media(pid_t pid, UInt32 n, const void *data) {
    if (n != sizeof(CFDataRef) || !data)
        return kAudioHardwareBadPropertySizeError;
    CFDataRef cf = *(CFDataRef const *)data;
    if (!cf || CFGetTypeID(cf) != CFDataGetTypeID())
        return kAudioHardwareIllegalOperationError;
    GMPacket p;
    if (!gm_packet(CFDataGetBytePtr(cf), (size_t)CFDataGetLength(cf), &p))
        return kAudioHardwareIllegalOperationError;
    uid_t uid, console;
    uint64_t start;
    if (!caller(pid, &uid, &start) || !console_uid(&console) || uid != console)
        return kAudioHardwareIllegalOperationError;
    pthread_mutex_lock(&gPropertyLock);
    uint64_t now = lease_now(),
             lease = atomic_load_explicit(&gLease, memory_order_acquire);
    bool same = gProducer == pid && gProducerStart == start && gGeneration == p.generation;
    if (p.opcode == 1) {
        if (lease > now && !same) {
            pthread_mutex_unlock(&gPropertyLock);
            return kAudioHardwareIllegalOperationError;
        }
        if (!same || lease <= now) {
            atomic_store_explicit(&gLease, 0, memory_order_release);
            atomic_fetch_add_explicit(&gEpoch, 1, memory_order_acq_rel);
            gProducer = pid;
            gProducerStart = start;
            gGeneration = p.generation;
            gNextFrame = frame_now() + 960;
            atomic_store(&gProducerUID, uid);
        }
        atomic_store_explicit(&gLease, now + gLeaseTicks, memory_order_release);
    } else if (!same || lease <= now) {
        pthread_mutex_unlock(&gPropertyLock);
        return kAudioHardwareIllegalOperationError;
    } else if (p.opcode == 3) {
        atomic_store_explicit(&gLease, 0, memory_order_release);
        atomic_fetch_add_explicit(&gEpoch, 1, memory_order_acq_rel);
        gProducer = 0;
        gGeneration = 0;
    } else {
        uint64_t current = frame_now(), epoch = atomic_load(&gEpoch);
        if (gNextFrame < current)
            gNextFrame = current + 960;
        /* Bound queued media to 100ms. Discard an overrun packet instead of
         * allowing a call to accumulate increasing conversational latency. */
        if (gNextFrame + p.frames <= current + 4800) {
            for (uint32_t i = 0; i < p.frames; ++i) {
                uint64_t packed = gm_u64(p.samples + i * 8);
                float channels[2];
                memcpy(channels, &packed, 8);
                for (unsigned ch = 0; ch < 2; ++ch)
                    channels[ch] = isfinite(channels[ch]) ? fmaxf(-1, fminf(1, channels[ch])) : 0;
                memcpy(&packed, channels, 8);
                gm_ring_write(&gRing, gNextFrame++, epoch, packed);
            }
        }
        atomic_store_explicit(&gLease, now + gLeaseTicks, memory_order_release);
    }
    pthread_mutex_unlock(&gPropertyLock);
    return 0;
}
static OSStatus set_data(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid,
                         const AudioObjectPropertyAddress *a, UInt32 qn, const void *q, UInt32 n,
                         const void *data) {
    (void)q;
    if (d != gDriver || !object(id) || !address(a) || qn || !data)
        return kAudioHardwareIllegalOperationError;
    if (id == DEVICE && a->mSelector == GM_PROP_FRAME &&
        a->mScope == kAudioObjectPropertyScopeGlobal)
        return media(pid, n, data);
    if (id == DEVICE && a->mSelector == kAudioDevicePropertyNominalSampleRate)
        return n == sizeof(Float64) && *(const Float64 *)data == GM_SAMPLE_RATE
                   ? 0
                   : kAudioDeviceUnsupportedFormatError;
    if (id == STREAM && (a->mSelector == kAudioStreamPropertyPhysicalFormat ||
                         a->mSelector == kAudioStreamPropertyVirtualFormat)) {
        AudioStreamBasicDescription f = gm_format();
        return n == sizeof(f) && !memcmp(data, &f, sizeof(f)) ? 0
                                                              : kAudioDeviceUnsupportedFormatError;
    }
    if (id == STREAM && a->mSelector == kAudioStreamPropertyIsActive)
        return n == sizeof(UInt32) && *(const UInt32 *)data == 1
                   ? 0
                   : kAudioHardwareIllegalOperationError;
    return kAudioHardwareUnknownPropertyError;
}
static OSStatus start_io(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client) {
    (void)client;
    if (d != gDriver || !device(id))
        return kAudioHardwareBadObjectError;
    atomic_fetch_add(&gRunning, 1);
    return 0;
}
static OSStatus stop_io(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client) {
    (void)client;
    if (d != gDriver || !device(id))
        return kAudioHardwareBadObjectError;
    uint32_t n = atomic_load(&gRunning);
    while (n && !atomic_compare_exchange_weak(&gRunning, &n, n - 1)) {
    }
    return 0;
}
static OSStatus zero_time(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client,
                          Float64 *sample, UInt64 *host, UInt64 *seed) {
    (void)client;
    if (d != gDriver || !device(id) || !sample || !host || !seed)
        return kAudioHardwareBadObjectError;
    uint64_t frame = frame_now() / 512 * 512;
    *sample = (Float64)frame;
    *host = gAnchor + (uint64_t)(frame * gTicksPerFrame);
    *seed = 1;
    return 0;
}
static OSStatus will_io(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 op,
                        Boolean *will, Boolean *inplace) {
    (void)client;
    if (d != gDriver || !device(id) || !will || !inplace)
        return kAudioHardwareBadObjectError;
    *will = op == kAudioServerPlugInIOOperationReadInput;
    *inplace = true;
    return 0;
}
static OSStatus boundary_io(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client,
                            UInt32 op, UInt32 frames, const AudioServerPlugInIOCycleInfo *cycle) {
    (void)client;
    (void)op;
    (void)frames;
    (void)cycle;
    return d == gDriver && device(id) ? 0 : kAudioHardwareBadObjectError;
}
static OSStatus do_io(AudioServerPlugInDriverRef d, AudioObjectID id, AudioObjectID stream,
                      UInt32 client, UInt32 op, UInt32 frames,
                      const AudioServerPlugInIOCycleInfo *cycle, void *main, void *secondary) {
    (void)secondary;
    if (d != gDriver || !device(id) || stream != STREAM)
        return kAudioHardwareBadObjectError;
    if (op != kAudioServerPlugInIOOperationReadInput)
        return 0;
    if (!main || !cycle || frames > GM_RING_FRAMES)
        return kAudioHardwareIllegalOperationError;
    memset(main, 0, (size_t)frames * 8);
    uint64_t now = lease_now(),
             lease = atomic_load_explicit(&gLease, memory_order_acquire);
    uint64_t epoch = atomic_load_explicit(&gEpoch, memory_order_acquire);
    uint32_t uid = atomic_load(&gProducerUID);
    GMClient *consumer = NULL;
    uint64_t identity = 0;
    if (lease <= now || !uid || !isfinite(cycle->mInputTime.mSampleTime) ||
        cycle->mInputTime.mSampleTime < 0 ||
        cycle->mInputTime.mSampleTime > (double)(UINT64_MAX - GM_RING_FRAMES))
        return 0;
    for (unsigned i = 0; i < MAX_CLIENTS; ++i) {
        uint64_t candidate = atomic_load_explicit(&gClients[i].identity, memory_order_acquire);
        if ((uint32_t)candidate == client && (uint32_t)(candidate >> 32) == uid) {
            consumer = &gClients[i];
            identity = candidate;
            break;
        }
    }
    if (!consumer)
        return 0;
    uint64_t first = (uint64_t)cycle->mInputTime.mSampleTime;
    for (uint32_t i = 0; i < frames; ++i) {
        uint64_t sample = gm_ring_read(&gRing, first + i, epoch);
        memcpy((unsigned char *)main + i * 8, &sample, 8);
    }
    if (atomic_load_explicit(&consumer->identity, memory_order_acquire) != identity ||
        atomic_load_explicit(&gEpoch, memory_order_acquire) != epoch ||
        atomic_load_explicit(&gLease, memory_order_acquire) <= lease_now())
        memset(main, 0, (size_t)frames * 8);
    return 0;
}
static AudioServerPlugInDriverInterface gInterface = {
    NULL,           query,      retain,        release,       initialize,    create_device,
    destroy_device, add_client, remove_client, configuration, configuration, has_property,
    settable,       data_size,  get_data,      set_data,      start_io,      stop_io,
    zero_time,      will_io,    boundary_io,   do_io,         boundary_io};
