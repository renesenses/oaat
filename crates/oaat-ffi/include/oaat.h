/**
 * oaat.h — C bindings for the OAAT endpoint SDK.
 *
 * Allows hardware manufacturers to implement OAAT endpoints in C/C++ firmware.
 *
 * Lifecycle:
 *   1. Create an endpoint with oaat_endpoint_new()
 *   2. Optionally set an audio callback with oaat_endpoint_set_audio_callback()
 *   3. Poll for events with oaat_endpoint_poll_event()
 *   4. When done, free with oaat_endpoint_free()
 *
 * Thread safety:
 *   All functions are safe to call from any thread. The internal tokio runtime
 *   handles concurrency. Do not call oaat_endpoint_free() while another thread
 *   is inside oaat_endpoint_poll_event().
 */

#ifndef OAAT_H
#define OAAT_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---------- Opaque handle ---------- */

typedef struct OaatEndpoint OaatEndpoint;

/* ---------- Audio format (matches oaat-core wire IDs) ---------- */

typedef enum {
    OAAT_FORMAT_PCM_S16LE   = 0x01,
    OAAT_FORMAT_PCM_S24LE   = 0x02,
    OAAT_FORMAT_PCM_S24LE4  = 0x03,
    OAAT_FORMAT_PCM_S32LE   = 0x04,
    OAAT_FORMAT_PCM_F32LE   = 0x05,
    OAAT_FORMAT_DSD_U8      = 0x10,
    OAAT_FORMAT_DSD_U16LE   = 0x11,
    OAAT_FORMAT_DSD_U32LE   = 0x12,
    OAAT_FORMAT_FLAC        = 0x20,
    OAAT_FORMAT_OPUS        = 0x21,
} OaatAudioFormat;

/* ---------- Event types ---------- */

typedef enum {
    OAAT_EVENT_NONE            = 0,
    OAAT_EVENT_CONNECTED       = 1,
    OAAT_EVENT_FORMAT_PROPOSED = 2,
    OAAT_EVENT_AUDIO           = 3,
    OAAT_EVENT_PLAY            = 4,
    OAAT_EVENT_PAUSE           = 5,
    OAAT_EVENT_STOP            = 6,
    OAAT_EVENT_METADATA        = 7,
    OAAT_EVENT_DISCONNECTED    = 8,
    OAAT_EVENT_VOLUME_SET      = 9,
    OAAT_EVENT_VOLUME_GET      = 10,
    OAAT_EVENT_VOLUME_MUTE     = 11,
    OAAT_EVENT_FORMAT_ACCEPTED = 12,
    OAAT_EVENT_FORMAT_REJECTED = 13,
    OAAT_EVENT_ERROR           = 14,
} OaatEventType;

/* ---------- Event data structs ---------- */

typedef struct {
    const char *controller_id;   /* Caller must free with oaat_string_free() */
    const char *controller_name; /* Caller must free with oaat_string_free() */
} OaatConnectedData;

typedef struct {
    const char     *stream_id;   /* Caller must free with oaat_string_free() */
    OaatAudioFormat format;
    uint32_t        sample_rate;
    uint8_t         channels;
    uint8_t         bits_per_sample;
} OaatFormatProposedData;

typedef struct {
    const uint8_t  *data;        /* Valid only for the duration of the event */
    size_t          len;
    OaatAudioFormat format;
    uint16_t        sequence;
    uint64_t        pts_ns;
    uint64_t        sample_offset;
} OaatAudioData;

typedef struct {
    const char *stream_id; /* Caller must free with oaat_string_free() */
} OaatPlaybackData;

typedef struct {
    const char *title;       /* Caller must free with oaat_string_free() */
    const char *artist;      /* Caller must free with oaat_string_free() */
    const char *album;       /* Caller must free with oaat_string_free() */
    uint64_t    duration_ms;
} OaatMetadataData;

typedef struct {
    uint8_t level;
    bool    muted;
} OaatVolumeData;

typedef struct {
    const char *stream_id; /* Caller must free with oaat_string_free() */
    const char *reason;    /* Caller must free with oaat_string_free() */
} OaatFormatRejectedData;

typedef struct {
    const char *message; /* Caller must free with oaat_string_free() */
} OaatErrorData;

/* ---------- Event (returned by poll) ---------- */

typedef struct {
    OaatEventType event_type;
    union {
        OaatConnectedData      connected;
        OaatFormatProposedData format_proposed;
        OaatAudioData          audio;
        OaatPlaybackData       playback;      /* PLAY, PAUSE, STOP */
        OaatMetadataData       metadata;
        OaatVolumeData         volume;         /* VOLUME_SET, VOLUME_MUTE */
        OaatFormatRejectedData format_rejected;
        OaatErrorData          error;
    } data;
} OaatEvent;

/* ---------- Audio callback ---------- */

/**
 * Audio callback signature.
 *
 * @param data        Raw audio payload bytes
 * @param len         Number of bytes in data
 * @param format      Audio sample format
 * @param sample_rate Sample rate in Hz (e.g. 44100, 192000)
 * @param channels    Number of channels (e.g. 2)
 * @param user_data   User-provided context pointer
 */
typedef void (*OaatAudioCallback)(
    const uint8_t  *data,
    size_t          len,
    OaatAudioFormat format,
    uint32_t        sample_rate,
    uint8_t         channels,
    void           *user_data
);

/* ---------- Status ---------- */

typedef enum {
    OAAT_STATUS_IDLE       = 0,
    OAAT_STATUS_CONNECTED  = 1,
    OAAT_STATUS_STREAMING  = 2,
    OAAT_STATUS_PAUSED     = 3,
    OAAT_STATUS_ERROR      = 4,
} OaatStatus;

/* ---------- Lifecycle ---------- */

/**
 * Create a new OAAT endpoint.
 *
 * Starts a tokio runtime, binds to the given port, and begins listening
 * for controller connections. Returns NULL on failure.
 *
 * @param name  Endpoint display name (e.g. "Living Room DAC")
 * @param port  TCP control port (0 for auto-assign)
 * @return      Opaque endpoint handle, or NULL on error
 */
OaatEndpoint *oaat_endpoint_new(const char *name, uint16_t port);

/**
 * Destroy an endpoint and free all resources.
 *
 * Shuts down the internal runtime and closes all sockets.
 * The pointer is invalid after this call.
 */
void oaat_endpoint_free(OaatEndpoint *ep);

/* ---------- Event polling ---------- */

/**
 * Poll for the next event from the endpoint.
 *
 * Blocks up to timeout_ms milliseconds. Returns an event with type
 * OAAT_EVENT_NONE if the timeout expires with no event.
 *
 * String fields in the returned event (controller_id, stream_id, etc.)
 * must be freed by the caller with oaat_string_free().
 *
 * @param ep          Endpoint handle
 * @param timeout_ms  Maximum wait time in milliseconds (0 = non-blocking)
 * @return            The next event
 */
OaatEvent oaat_endpoint_poll_event(OaatEndpoint *ep, uint32_t timeout_ms);

/* ---------- Audio callback ---------- */

/**
 * Register a callback for incoming audio data.
 *
 * When set, audio packets are delivered via this callback instead of
 * (or in addition to) OAAT_EVENT_AUDIO poll events. The callback is
 * invoked from an internal thread — keep it fast and non-blocking.
 *
 * @param ep         Endpoint handle
 * @param callback   Function pointer, or NULL to unregister
 * @param user_data  Opaque pointer passed through to the callback
 */
void oaat_endpoint_set_audio_callback(
    OaatEndpoint    *ep,
    OaatAudioCallback callback,
    void            *user_data
);

/* ---------- Control ---------- */

/**
 * Set the endpoint volume level (0-100).
 *
 * This reports the volume back to the connected controller.
 */
void oaat_endpoint_set_volume(OaatEndpoint *ep, uint8_t level);

/**
 * Get the current endpoint status.
 */
OaatStatus oaat_endpoint_get_status(const OaatEndpoint *ep);

/* ---------- String management ---------- */

/**
 * Free a string returned by an OaatEvent.
 *
 * Strings allocated by the FFI layer must be freed with this function,
 * not with free().
 */
void oaat_string_free(const char *s);

#ifdef __cplusplus
}
#endif

#endif /* OAAT_H */
