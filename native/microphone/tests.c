/* Exercises the plug-in interface in-process with synthetic samples only.
 * Does not install a HAL driver, start Core Audio, or access a real microphone.
 */
#define GM_TESTING 1
#include "GreenmoleMicrophone.c"
#include "DriverProperties.c"
#include <assert.h>
#include <stdio.h>

static uid_t test_console = 501;
static uint64_t test_start = 100;
bool gm_test_caller(pid_t pid, uid_t *uid, uint64_t *start) {
    if (pid != 10 && pid != 11 && pid != 12)
        return false;
    *uid = pid == 12 ? 502 : 501;
    *start = test_start;
    return true;
}
bool gm_test_console(uid_t *uid) {
    *uid = test_console;
    return test_console != 0;
}
static void u32(unsigned char *p, uint32_t n) {
    for (unsigned i = 0; i < 4; ++i)
        p[i] = (unsigned char)(n >> (i * 8));
}
static void u64(unsigned char *p, uint64_t n) {
    for (unsigned i = 0; i < 8; ++i)
        p[i] = (unsigned char)(n >> (i * 8));
}
static OSStatus send_packet(pid_t pid, uint32_t op, uint64_t gen, uint32_t frames, float sample) {
    unsigned char packet[GM_HEADER_BYTES + GM_MAX_PACKET_FRAMES * 8] = {0};
    memcpy(packet, "GMM1", 4);
    u32(packet + 4, op);
    u64(packet + 8, gen);
    u32(packet + 16, frames);
    for (uint32_t i = 0; i < frames && i < GM_MAX_PACKET_FRAMES; ++i) {
        memcpy(packet + GM_HEADER_BYTES + i * 8, &sample, 4);
        memcpy(packet + GM_HEADER_BYTES + i * 8 + 4, &sample, 4);
    }
    CFDataRef cf = CFDataCreate(
        NULL, packet, GM_HEADER_BYTES + (frames <= GM_MAX_PACKET_FRAMES ? frames * 8 : 0));
    OSStatus result = media(pid, sizeof(cf), &cf);
    CFRelease(cf);
    return result;
}
static void read_frames(uint32_t client, uint64_t frame, float *buffer, uint32_t count) {
    AudioServerPlugInIOCycleInfo cycle = {0};
    cycle.mInputTime.mSampleTime = (Float64)frame;
    assert(do_io(gDriver, DEVICE, STREAM, client, kAudioServerPlugInIOOperationReadInput, count,
                 &cycle, buffer, NULL) == 0);
}
static void protocol_tests(void) {
    unsigned char b[GM_HEADER_BYTES + 8] = {0};
    GMPacket p;
    assert(!gm_packet(b, sizeof(b), &p));
    memcpy(b, "GMM1", 4);
    u32(b + 4, 2);
    u64(b + 8, 1);
    u32(b + 16, 1);
    assert(gm_packet(b, sizeof(b), &p));
    assert(!gm_packet(b, sizeof(b) - 1, &p));
    u32(b + 20, 1);
    assert(!gm_packet(b, sizeof(b), &p));
    u32(b + 20, 0);
    u64(b + 8, 0);
    assert(!gm_packet(b, sizeof(b), &p));
    u64(b + 8, 1);
    u32(b + 16, GM_MAX_PACKET_FRAMES + 1);
    assert(!gm_packet(b, sizeof(b), &p));
    u32(b + 4, 3);
    u32(b + 16, 0);
    assert(gm_packet(b, GM_HEADER_BYTES, &p));
    assert(!gm_packet(b, sizeof(b), &p));
    GMRing ring = {0};
    gm_ring_write(&ring, 42, 1, 123);
    assert(gm_ring_read(&ring, 42, 1) == 123);
    assert(gm_ring_read(&ring, 42, 2) == 0);
    gm_ring_write(&ring, 42 + GM_RING_FRAMES, 1, 456);
    assert(gm_ring_read(&ring, 42, 1) == 0);
    assert(gm_ring_read(&ring, 42 + GM_RING_FRAMES, 1) == 456);
}
static GMRing concurrent_ring;
static _Atomic uint64_t concurrent_written;
static _Atomic bool concurrent_done;
static uint64_t pattern(uint64_t frame) {
    return (frame + 1) * 0x123456789abcdefull;
}
static void *ring_writer(void *unused) {
    (void)unused;
    for (uint64_t frame = 0; frame < 200000; ++frame) {
        gm_ring_write(&concurrent_ring, frame, 1, pattern(frame));
        atomic_store(&concurrent_written, frame);
    }
    atomic_store(&concurrent_done, true);
    return NULL;
}
static void *ring_reader(void *unused) {
    (void)unused;
    uint64_t iteration = 0;
    while (!atomic_load(&concurrent_done)) {
        uint64_t latest = atomic_load(&concurrent_written);
        uint64_t offset = iteration++ % (GM_RING_FRAMES * 2);
        uint64_t frame = latest > offset ? latest - offset : 0;
        uint64_t value = gm_ring_read(&concurrent_ring, frame, 1);
        assert(value == 0 || value == pattern(frame));
    }
    return NULL;
}
static void concurrency_test(void) {
    pthread_t writer, readers[3];
    for (unsigned i = 0; i < 3; ++i)
        assert(pthread_create(&readers[i], NULL, ring_reader, NULL) == 0);
    assert(pthread_create(&writer, NULL, ring_writer, NULL) == 0);
    assert(pthread_join(writer, NULL) == 0);
    for (unsigned i = 0; i < 3; ++i)
        assert(pthread_join(readers[i], NULL) == 0);
}
static void property_tests(void) {
    AudioObjectPropertyAddress a = {kAudioPlugInPropertyTranslateUIDToDevice,
                                    kAudioObjectPropertyScopeGlobal, 0};
    assert(has_property(gDriver, PLUGIN, 10, &a));
    uint32_t size, result = 0;
    CFStringRef uid = UID;
    assert(get_data(gDriver, PLUGIN, 10, &a, sizeof(uid), &uid, sizeof(result), &size, &result) ==
           0);
    assert(result == DEVICE);
    a.mSelector = kAudioPlugInPropertyBoxList;
    assert(get_data(gDriver, PLUGIN, 10, &a, 0, NULL, sizeof(result), &size, &result) == 0 &&
           result == BOX);
    a.mSelector = kAudioDevicePropertyStreams;
    a.mScope = kAudioObjectPropertyScopeOutput;
    assert(data_size(gDriver, DEVICE, 10, &a, 0, NULL, &size) == 0 && size == 0);
    a.mSelector = GM_PROP_VERSION;
    a.mScope = kAudioObjectPropertyScopeGlobal;
    CFNumberRef version = NULL;
    assert(get_data(gDriver, DEVICE, 10, &a, 0, NULL, sizeof(version), &size, &version) == 0);
    int v;
    assert(CFNumberGetValue(version, kCFNumberIntType, &v) && v == 1);
    CFRelease(version);
    a.mSelector = GM_PROP_FRAME;
    CFDataRef private_data = NULL;
    assert(get_data(gDriver, DEVICE, 10, &a, 0, NULL, sizeof(private_data), &size, &private_data) !=
           0);
}
static void privacy_and_audio_tests(void) {
    AudioServerPlugInClientInfo a = {1, 10, true, NULL}, b = {2, 11, true, NULL},
                                other = {3, 12, true, NULL};
    assert(add_client(gDriver, DEVICE, &a) == 0 && add_client(gDriver, DEVICE, &b) == 0 &&
           add_client(gDriver, DEVICE, &other) == 0);
    assert(send_packet(0, 1, 1, 0, 0) != 0);
    assert(send_packet(12, 1, 1, 0, 0) != 0);
    assert(send_packet(10, 2, 1, 1, .5) != 0); /* PCM before begin */
    assert(send_packet(10, 1, 1, 0, 0) == 0);
    assert(send_packet(11, 1, 2, 0, 0) != 0);  /* producer cannot be stolen */
    assert(send_packet(10, 2, 2, 1, .5) != 0); /* wrong generation */
    uint64_t first = gNextFrame;
    assert(send_packet(10, 2, 1, 64, .5f) == 0);
    float one[128], two[128];
    read_frames(1, first, one, 64);
    read_frames(2, first, two, 64);
    assert(one[0] == .5f &&
           !memcmp(one, two, sizeof(one))); /* clients do not consume each other's audio */
    read_frames(3, first, two, 64);
    for (unsigned i = 0; i < 128; ++i)
        assert(two[i] == 0);
    assert(send_packet(10, 3, 1, 0, 0) == 0);
    read_frames(1, first, one, 64);
    for (unsigned i = 0; i < 128; ++i)
        assert(one[i] == 0);
    assert(send_packet(10, 1, 2, 0, 0) == 0);
    read_frames(1, first, one, 64);
    assert(one[0] == 0); /* old epoch cannot replay */
    first = gNextFrame;
    assert(send_packet(10, 2, 2, 1, NAN) == 0);
    read_frames(1, first, one, 1);
    assert(one[0] == 0 && one[1] == 0);
    test_start++;
    assert(send_packet(10, 2, 2, 1, .5f) != 0); /* PID reuse */
    test_start--;
    atomic_store(&gLease, lease_now() - 1);
    read_frames(1, first, one, 1);
    assert(one[0] == 0);
    assert(send_packet(10, 2, 2, 1, .5f) != 0);
    assert(send_packet(11, 1, 3, 0, 0) == 0); /* expired producer can be replaced */
    test_console = 502;
    assert(send_packet(11, 2, 3, 1, .5f) != 0);
    test_console = 501;
    assert(send_packet(11, 3, 3, 0, 0) == 0);
    assert(remove_client(gDriver, DEVICE, &a) == 0);
    /* Reusing the same HAL client ID with a different UID cannot retain access. */
    assert(send_packet(10, 1, 4, 0, 0) == 0);
    first = gNextFrame;
    assert(send_packet(10, 2, 4, 1, .25f) == 0);
    a.mProcessID = 12;
    assert(add_client(gDriver, DEVICE, &a) == 0);
    read_frames(1, first, one, 1);
    assert(one[0] == 0);
    assert(send_packet(10, 3, 4, 0, 0) == 0);
}
int main(void) {
    assert(initialize(gDriver, NULL) == 0);
    protocol_tests();
    concurrency_test();
    property_tests();
    privacy_and_audio_tests();
    puts("microphone protocol, HAL properties, authorization, generation, lease and multi-client "
         "tests passed");
    return 0;
}
