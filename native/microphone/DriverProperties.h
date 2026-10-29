#ifndef GREENMOLE_DRIVER_PROPERTIES_H
#define GREENMOLE_DRIVER_PROPERTIES_H

#include <CoreAudio/AudioServerPlugIn.h>
#include <stdbool.h>
#include <stdint.h>

enum { PLUGIN = kAudioObjectPlugInObject, BOX = 2, DEVICE = 3, STREAM = 4, MAX_CLIENTS = 128 };
static inline bool device(AudioObjectID id) {
    return id == DEVICE;
}
static inline bool object(AudioObjectID id) {
    return id >= PLUGIN && id <= STREAM;
}
static inline bool address(const AudioObjectPropertyAddress *a) {
    return a && a->mElement == kAudioObjectPropertyElementMain &&
           (a->mScope == kAudioObjectPropertyScopeGlobal ||
            a->mScope == kAudioObjectPropertyScopeInput ||
            a->mScope == kAudioObjectPropertyScopeOutput);
}
AudioStreamBasicDescription gm_format(void);
OSStatus gm_property(AudioObjectID id, const AudioObjectPropertyAddress *a, UInt32 qn,
                     const void *q, UInt32 capacity, UInt32 *written, void *out, bool size_only,
                     uint32_t running);
#endif
