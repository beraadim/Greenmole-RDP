/* Core Audio object topology and property metadata for the fixed input device. */
#include "DriverProperties.h"
#include "MediaRing.h"
#include <stddef.h>
#include <string.h>

static const CFStringRef UID = CFSTR("com.macrdp.microphone");
static const CFStringRef NAME = CFSTR("Greenmole Remote Microphone");

AudioStreamBasicDescription gm_format(void) {
    AudioStreamBasicDescription f = {0};
    f.mSampleRate = GM_SAMPLE_RATE;
    f.mFormatID = kAudioFormatLinearPCM;
    f.mFormatFlags =
        kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked | kAudioFormatFlagsNativeEndian;
    f.mBytesPerPacket = f.mBytesPerFrame = 8;
    f.mFramesPerPacket = 1;
    f.mChannelsPerFrame = 2;
    f.mBitsPerChannel = 32;
    return f;
}
static OSStatus copy_value(UInt32 capacity, UInt32 *written, void *out, const void *value,
                           UInt32 size) {
    if (!written || (!out && size))
        return kAudioHardwareIllegalOperationError;
    if (capacity < size)
        return kAudioHardwareBadPropertySizeError;
    if (size)
        memcpy(out, value, size);
    *written = size;
    return 0;
}
static OSStatus copy_string(UInt32 n, UInt32 *written, void *out, CFStringRef string) {
    if (n < sizeof(string) || !out || !written)
        return kAudioHardwareBadPropertySizeError;
    if (string)
        CFRetain(string);
    memcpy(out, &string, sizeof(string));
    *written = sizeof(string);
    return 0;
}

/* Keep property metadata and payload generation in one implementation, so the
 * host cannot observe disagreement between HasProperty, size, and get calls. */
OSStatus gm_property(AudioObjectID id, const AudioObjectPropertyAddress *a, UInt32 qn,
                     const void *q, UInt32 capacity, UInt32 *written, void *out, bool size_only,
                     uint32_t running) {
    if (!object(id) || !address(a))
        return kAudioHardwareBadObjectError;
    UInt32 u = 0, items[3] = {0};
    Float64 rate = GM_SAMPLE_RATE;
    CFStringRef string = NULL;
    const void *value = &u;
    UInt32 bytes = sizeof(u);
    AudioStreamBasicDescription f;
    AudioStreamRangedDescription ranged;
    AudioValueRange range;
    AudioChannelLayout layout;
    AudioServerPlugInCustomPropertyInfo custom[2];
    switch (a->mSelector) {
    case kAudioObjectPropertyBaseClass:
        u = kAudioObjectClassID;
        break;
    case kAudioObjectPropertyClass:
        u = id == PLUGIN   ? kAudioPlugInClassID
            : id == BOX    ? kAudioBoxClassID
            : id == DEVICE ? kAudioDeviceClassID
                           : kAudioStreamClassID;
        break;
    case kAudioObjectPropertyOwner:
        u = id == PLUGIN ? kAudioObjectUnknown : id == STREAM ? DEVICE : PLUGIN;
        break;
    case kAudioObjectPropertyManufacturer:
        string = CFSTR("Greenmole");
        break;
    case kAudioObjectPropertyName:
        string = id == STREAM ? CFSTR("Remote microphone input") : NAME;
        break;
    case kAudioObjectPropertyOwnedObjects:
        value = items;
        bytes = 0;
        if (id == PLUGIN) {
            items[0] = BOX;
            items[1] = DEVICE;
            bytes = 8;
        }
        if (id == DEVICE && a->mScope != kAudioObjectPropertyScopeOutput) {
            items[0] = STREAM;
            bytes = 4;
        }
        break;
    default:
        if (id == PLUGIN)
            switch (a->mSelector) {
            case kAudioPlugInPropertyBundleID:
                string = UID;
                break;
            case kAudioPlugInPropertyDeviceList:
                u = DEVICE;
                break;
            case kAudioPlugInPropertyBoxList:
                u = BOX;
                break;
            case kAudioPlugInPropertyTranslateUIDToDevice:
            case kAudioPlugInPropertyTranslateUIDToBox:
                if (size_only)
                    break;
                if (qn != sizeof(CFStringRef) || !q)
                    return kAudioHardwareBadPropertySizeError;
                if (*(CFStringRef const *)q &&
                    CFGetTypeID(*(CFStringRef const *)q) == CFStringGetTypeID() &&
                    CFEqual(*(CFStringRef const *)q, UID))
                    u = a->mSelector == kAudioPlugInPropertyTranslateUIDToDevice ? DEVICE : BOX;
                break;
            default:
                return kAudioHardwareUnknownPropertyError;
            }
        else if (id == BOX)
            switch (a->mSelector) {
            case kAudioBoxPropertyBoxUID:
                string = UID;
                break;
            case kAudioObjectPropertyModelName:
                string = NAME;
                break;
            case kAudioObjectPropertySerialNumber:
                string = CFSTR("1");
                break;
            case kAudioObjectPropertyFirmwareVersion:
                string = CFSTR("1.0.0");
                break;
            case kAudioBoxPropertyTransportType:
                u = kAudioDeviceTransportTypeVirtual;
                break;
            case kAudioBoxPropertyHasAudio:
            case kAudioBoxPropertyAcquired:
                u = 1;
                break;
            case kAudioBoxPropertyHasVideo:
            case kAudioBoxPropertyHasMIDI:
            case kAudioBoxPropertyIsProtected:
            case kAudioBoxPropertyAcquisitionFailed:
            case kAudioObjectPropertyIdentify:
                break;
            case kAudioBoxPropertyDeviceList:
                u = DEVICE;
                break;
            case kAudioBoxPropertyClockDeviceList:
                bytes = 0;
                break;
            default:
                return kAudioHardwareUnknownPropertyError;
            }
        else if (id == DEVICE)
            switch (a->mSelector) {
            case kAudioDevicePropertyDeviceUID:
            case kAudioDevicePropertyModelUID:
                string = UID;
                break;
            case kAudioDevicePropertyTransportType:
                u = kAudioDeviceTransportTypeVirtual;
                break;
            case kAudioDevicePropertyRelatedDevices:
                u = DEVICE;
                break;
            case kAudioDevicePropertyClockDomain:
            case kAudioDevicePropertyIsHidden:
            case kAudioDevicePropertyLatency:
            case kAudioDevicePropertySafetyOffset:
                break;
            case kAudioDevicePropertyDeviceIsAlive:
                u = 1;
                break;
            case kAudioDevicePropertyDeviceIsRunning:
                u = running > 0;
                break;
            case kAudioDevicePropertyDeviceCanBeDefaultDevice:
                break;
            case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
                break;
            case kAudioDevicePropertyStreams:
                bytes = a->mScope == kAudioObjectPropertyScopeOutput ? 0 : 4;
                u = STREAM;
                break;
            case kAudioObjectPropertyControlList:
                bytes = 0;
                break;
            case kAudioDevicePropertyNominalSampleRate:
                value = &rate;
                bytes = sizeof(rate);
                break;
            case kAudioDevicePropertyAvailableNominalSampleRates:
                range = (AudioValueRange){GM_SAMPLE_RATE, GM_SAMPLE_RATE};
                value = &range;
                bytes = sizeof(range);
                break;
            case kAudioDevicePropertyZeroTimeStampPeriod:
                u = 512;
                break;
            case kAudioDevicePropertyPreferredChannelsForStereo:
                items[0] = 1;
                items[1] = 2;
                value = items;
                bytes = 8;
                break;
            case kAudioDevicePropertyPreferredChannelLayout:
                memset(&layout, 0, sizeof(layout));
                layout.mChannelLayoutTag = kAudioChannelLayoutTag_Stereo;
                value = &layout;
                bytes = offsetof(AudioChannelLayout, mChannelDescriptions);
                break;
            case kAudioObjectPropertyCustomPropertyInfoList:
                custom[0] = (AudioServerPlugInCustomPropertyInfo){
                    GM_PROP_FRAME, kAudioServerPlugInCustomPropertyDataTypeCFPropertyList,
                    kAudioServerPlugInCustomPropertyDataTypeNone};
                custom[1] = (AudioServerPlugInCustomPropertyInfo){
                    GM_PROP_VERSION, kAudioServerPlugInCustomPropertyDataTypeCFPropertyList,
                    kAudioServerPlugInCustomPropertyDataTypeNone};
                value = custom;
                bytes = sizeof(custom);
                break;
            case GM_PROP_VERSION:
                if (size_only) {
                    *written = sizeof(CFNumberRef);
                    return 0;
                }
                if (capacity < sizeof(CFNumberRef) || !written || !out)
                    return kAudioHardwareBadPropertySizeError;
                u = 1;
                CFNumberRef version = CFNumberCreate(NULL, kCFNumberSInt32Type, &u);
                memcpy(out, &version, sizeof(version));
                *written = sizeof(version);
                return 0;
            case GM_PROP_FRAME:
                /* A getter must never disclose microphone payload to another process. */
                bytes = sizeof(CFDataRef);
                value = NULL;
                break;
            default:
                return kAudioHardwareUnknownPropertyError;
            }
        else if (id == STREAM)
            switch (a->mSelector) {
            case kAudioStreamPropertyIsActive:
                u = 1;
                break;
            case kAudioStreamPropertyDirection:
                u = 1;
                break;
            case kAudioStreamPropertyTerminalType:
                u = kAudioStreamTerminalTypeMicrophone;
                break;
            case kAudioStreamPropertyStartingChannel:
                u = 1;
                break;
            case kAudioStreamPropertyLatency:
                break;
            case kAudioStreamPropertyVirtualFormat:
            case kAudioStreamPropertyPhysicalFormat:
                f = gm_format();
                value = &f;
                bytes = sizeof(f);
                break;
            case kAudioStreamPropertyAvailableVirtualFormats:
            case kAudioStreamPropertyAvailablePhysicalFormats:
                ranged.mFormat = gm_format();
                ranged.mSampleRateRange = (AudioValueRange){GM_SAMPLE_RATE, GM_SAMPLE_RATE};
                value = &ranged;
                bytes = sizeof(ranged);
                break;
            default:
                return kAudioHardwareUnknownPropertyError;
            }
        else
            return kAudioHardwareUnknownPropertyError;
    }
    if (size_only) {
        *written = string ? sizeof(CFStringRef) : bytes;
        return 0;
    }
    if (string)
        return copy_string(capacity, written, out, string);
    if (!value && bytes)
        return kAudioHardwareIllegalOperationError;
    return copy_value(capacity, written, out, value, bytes);
}
