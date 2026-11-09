// apple_speech.h - Objective-C bridge for Apple on-device Speech Recognition.
#ifndef APPLE_SPEECH_H
#define APPLE_SPEECH_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/// Checks current Speech Recognition permission without prompting.
/// 0: NotDetermined, 1: Denied, 2: Restricted, 3: Authorized, -1: Unavailable
int apple_speech_permission_status(void);

/// Requests Speech Recognition permission, waiting up to timeout_seconds.
/// Returns the resulting permission status.
int apple_speech_request_permission(double timeout_seconds);

typedef void (*AppleSpeechTranscriptCallback)(const char *text, bool is_final, void *user_data);
typedef void (*AppleSpeechStatusCallback)(int code, const char *message, void *user_data);

typedef void* AppleSpeechSessionRef;

/// Creates and starts an Apple Speech recognition session.
/// Returns a session handle on success, NULL on failure.
AppleSpeechSessionRef apple_speech_start(
    AppleSpeechTranscriptCallback on_transcript,
    AppleSpeechStatusCallback on_status,
    void *user_data
);

/// Feeds 16kHz mono linear PCM 16-bit signed integer samples to the session.
void apple_speech_feed_audio(AppleSpeechSessionRef session_ref, const int16_t *samples, size_t count);

/// Rotates to a fresh recognition request so the session can run indefinitely
/// without buffer accumulation or hitting duration limits.
void apple_speech_rotate_request(AppleSpeechSessionRef session_ref);

/// Stops and frees the speech recognition session.
void apple_speech_stop(AppleSpeechSessionRef session_ref);

#ifdef __cplusplus
}
#endif

#endif // APPLE_SPEECH_H

