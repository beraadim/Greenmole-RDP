#ifndef GREENMOLE_MEDIA_RING_H
#define GREENMOLE_MEDIA_RING_H

#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>

#define GM_SAMPLE_RATE 48000u
#define GM_CHANNELS 2u
#define GM_RING_FRAMES 8192u
#define GM_MAX_PACKET_FRAMES 1024u
#define GM_HEADER_BYTES 24u
#define GM_PROP_FRAME 0x676d6d66u   /* gmmf: serialized CFData, write only */
#define GM_PROP_VERSION 0x676d6d76u /* gmmv: UInt32 protocol version */

/* Every field touched by both a property thread and an IO thread is atomic.
 * Timeline-addressed frames allow all consumers to read the same IO quantum.
 * Sequential consistency orders both invalidation and publication around the
 * atomic payload, including on ARM; readers verify the stamp after reading.
 */
typedef struct {
    _Atomic uint64_t sequence;
    _Atomic uint64_t epoch;
    _Atomic uint64_t stereo;
} GMFrame;

typedef struct {
    GMFrame frames[GM_RING_FRAMES];
} GMRing;

typedef struct {
    uint32_t opcode;
    uint64_t generation;
    uint32_t frames;
    const unsigned char *samples;
} GMPacket;

static inline uint32_t gm_u32(const unsigned char *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}
static inline uint64_t gm_u64(const unsigned char *p) {
    return (uint64_t)gm_u32(p) | (uint64_t)gm_u32(p + 4) << 32;
}
static inline bool gm_packet(const void *bytes, size_t size, GMPacket *p) {
    const unsigned char *b = bytes;
    if (!b || !p || size < GM_HEADER_BYTES || memcmp(b, "GMM1", 4) || gm_u32(b + 20))
        return false;
    p->opcode = gm_u32(b + 4);
    p->generation = gm_u64(b + 8);
    p->frames = gm_u32(b + 16);
    p->samples = b + GM_HEADER_BYTES;
    if (!p->generation || p->opcode < 1 || p->opcode > 3)
        return false;
    if (p->opcode != 2)
        return p->frames == 0 && size == GM_HEADER_BYTES;
    return p->frames > 0 && p->frames <= GM_MAX_PACKET_FRAMES &&
           size == GM_HEADER_BYTES + (size_t)p->frames * 8;
}
static inline void gm_ring_write(GMRing *ring, uint64_t frame, uint64_t epoch, uint64_t stereo) {
    GMFrame *slot = &ring->frames[frame % GM_RING_FRAMES];
    atomic_store_explicit(&slot->sequence, 0, memory_order_seq_cst);
    atomic_store_explicit(&slot->stereo, stereo, memory_order_seq_cst);
    atomic_store_explicit(&slot->epoch, epoch, memory_order_seq_cst);
    atomic_store_explicit(&slot->sequence, frame + 1, memory_order_seq_cst);
}
static inline uint64_t gm_ring_read(const GMRing *ring, uint64_t frame, uint64_t epoch) {
    const GMFrame *slot = &ring->frames[frame % GM_RING_FRAMES];
    if (atomic_load_explicit(&slot->sequence, memory_order_seq_cst) != frame + 1)
        return 0;
    uint64_t value = atomic_load_explicit(&slot->stereo, memory_order_seq_cst);
    if (atomic_load_explicit(&slot->epoch, memory_order_seq_cst) != epoch ||
        atomic_load_explicit(&slot->sequence, memory_order_seq_cst) != frame + 1)
        return 0;
    return value;
}
#endif
