// Captures this Mac's own audio *output* (the prospect's side of a dialer call) with
// ScreenCaptureKit, and hands 16 kHz mono int16 frames to Rust.
//
// Additive by design: the microphone stream (cpal) and Pulse Dialer are untouched. We only
// listen to what macOS is already playing, then transcribe it on a second, isolated recogniser.
// Needs the "Screen & System Audio Recording" permission; until it is granted macOS returns an
// error which we report (the app keeps working with the microphone only).

#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreAudio/CoreAudio.h>
#import <math.h>
#import <stdint.h>
#import <stdlib.h>
#import <string.h>

typedef void (*tp_audio_cb)(const int16_t *samples, int count);
typedef void (*tp_status_cb)(int status, const char *message);

static tp_audio_cb gAudioCb = NULL;
static tp_status_cb gStatusCb = NULL;

@interface TPAudioSink : NSObject <SCStreamOutput>
@property(nonatomic, strong) SCStream *stream;
@end

static TPAudioSink *gSink = nil;

static void tp_report(int status, NSString *message) {
    const char *utf8 = message.UTF8String ?: "";
    if (gStatusCb != NULL) {
        gStatusCb(status, utf8);
    } else {
        NSLog(@"[system-audio] status=%d %@", status, message);
    }
}

/// Convert one ScreenCaptureKit audio buffer to mono int16 and forward it.
/// Handles float32 and int16, interleaved and non-interleaved, because SCK's layout is not
/// guaranteed across macOS versions even when we ask for 16 kHz mono.
static void tp_emit(CMSampleBufferRef sampleBuffer) {
    if (gAudioCb == NULL || !CMSampleBufferIsValid(sampleBuffer)) {
        return;
    }
    CMFormatDescriptionRef format = CMSampleBufferGetFormatDescription(sampleBuffer);
    if (format == NULL) {
        return;
    }
    const AudioStreamBasicDescription *asbd =
        CMAudioFormatDescriptionGetStreamBasicDescription(format);
    if (asbd == NULL) {
        return;
    }

    size_t listSize = sizeof(AudioBufferList) + sizeof(AudioBuffer) * 4;
    AudioBufferList *buffers = (AudioBufferList *)calloc(1, listSize);
    if (buffers == NULL) {
        return;
    }
    CMBlockBufferRef block = NULL;
    OSStatus status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sampleBuffer, NULL, buffers, listSize, NULL, NULL,
        kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, &block);
    if (status != noErr || buffers->mNumberBuffers == 0) {
        if (block != NULL) {
            CFRelease(block);
        }
        free(buffers);
        return;
    }

    UInt32 channels = asbd->mChannelsPerFrame > 0 ? asbd->mChannelsPerFrame : 1;
    UInt32 bufferCount = buffers->mNumberBuffers;
    bool isFloat = (asbd->mFormatFlags & kAudioFormatFlagIsFloat) != 0;
    size_t bytesPerSample = isFloat ? sizeof(float) : sizeof(int16_t);

    size_t frames;
    if (bufferCount > 1) {
        // Non-interleaved: one buffer per channel.
        frames = buffers->mBuffers[0].mDataByteSize / bytesPerSample;
    } else {
        // Interleaved (or mono): every sample of a frame lives in one buffer.
        size_t frameBytes = bytesPerSample * (size_t)channels;
        frames = frameBytes > 0 ? buffers->mBuffers[0].mDataByteSize / frameBytes : 0;
    }
    if (frames == 0) {
        if (block != NULL) {
            CFRelease(block);
        }
        free(buffers);
        return;
    }

    int16_t *out = (int16_t *)malloc(frames * sizeof(int16_t));
    if (out == NULL) {
        if (block != NULL) {
            CFRelease(block);
        }
        free(buffers);
        return;
    }

    for (size_t i = 0; i < frames; i++) {
        float sum = 0.0f;
        int counted = 0;
        if (bufferCount > 1) {
            for (UInt32 b = 0; b < bufferCount && b < 4; b++) {
                const void *data = buffers->mBuffers[b].mData;
                if (data == NULL) {
                    continue;
                }
                if (isFloat) {
                    sum += ((const float *)data)[i];
                } else {
                    sum += (float)((const int16_t *)data)[i] / 32768.0f;
                }
                counted++;
            }
        } else {
            const void *data = buffers->mBuffers[0].mData;
            if (data == NULL) {
                continue;
            }
            for (UInt32 c = 0; c < channels; c++) {
                size_t idx = i * (size_t)channels + c;
                if (isFloat) {
                    sum += ((const float *)data)[idx];
                } else {
                    sum += (float)((const int16_t *)data)[idx] / 32768.0f;
                }
                counted++;
            }
        }
        if (counted == 0) {
            counted = 1;
        }
        float mono = sum / (float)counted;
        if (mono > 1.0f) {
            mono = 1.0f;
        } else if (mono < -1.0f) {
            mono = -1.0f;
        }
        out[i] = (int16_t)lrintf(mono * 32767.0f);
    }

    gAudioCb(out, (int)frames);
    free(out);
    if (block != NULL) {
        CFRelease(block);
    }
    free(buffers);
}

@implementation TPAudioSink
- (void)stream:(SCStream *)stream
    didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
                   ofType:(SCStreamOutputType)type API_AVAILABLE(macos(13.0)) {
    (void)stream;
    if (type == SCStreamOutputTypeAudio) {
        tp_emit(sampleBuffer);
    }
}
@end

/// Start capturing system output. `status` 0 = running, 1 = needs permission, 2 = hard failure.
void tp_system_audio_start(tp_audio_cb on_audio, tp_status_cb on_status) {
    gAudioCb = on_audio;
    gStatusCb = on_status;

    if (@available(macOS 13.0, *)) {
        [SCShareableContent
            getShareableContentWithCompletionHandler:^(SCShareableContent *content,
                                                       NSError *error) {
                if (error != nil || content == nil) {
                    NSString *why = error.localizedDescription
                                        ?: @"no shareable content (Screen Recording permission?)";
                    tp_report(1, why);
                    return;
                }
                SCDisplay *display = content.displays.firstObject;
                if (display == nil) {
                    tp_report(1, @"no display available to attach audio capture");
                    return;
                }

                // Exclude music/entertainment apps (Safari for YouTube techno, Spotify, Apple Music)
                // so the prospect's voice from Chrome/Google Meet/Zoom is pristine and music never bleeds into STT.
                NSMutableArray<SCRunningApplication *> *excludedApps = [NSMutableArray array];
                for (SCRunningApplication *app in content.applications) {
                    NSString *bundleId = app.bundleIdentifier.lowercaseString ?: @"";
                    NSString *appName = app.applicationName.lowercaseString ?: @"";
                    if ([bundleId containsString:@"safari"] ||
                        [bundleId containsString:@"spotify"] ||
                        [bundleId containsString:@"apple.music"] ||
                        [appName containsString:@"spotify"] ||
                        [appName containsString:@"music"]) {
                        [excludedApps addObject:app];
                        NSLog(@"[system-audio] Excluded music app: %@ (%@)", app.applicationName, app.bundleIdentifier);
                    }
                }

                SCContentFilter *filter =
                    [[SCContentFilter alloc] initWithDisplay:display
                                       excludingApplications:excludedApps
                                            exceptingWindows:@[]];

                SCStreamConfiguration *config = [[SCStreamConfiguration alloc] init];
                config.capturesAudio = YES;
                // Keep this NO: audio is attributed to the *responsible* process, so excluding
                // "current process" can silence everything when the app was launched from a
                // terminal/IDE. We never play audio ourselves, so there is nothing to feed back.
                config.excludesCurrentProcessAudio = NO;
                config.sampleRate = 48000;
                config.channelCount = 2;
                // Video is required by the API but we never attach a video output, so keep the
                // surface tiny and the frame rate minimal: this is an audio tap only.
                config.width = 2;
                config.height = 2;
                config.showsCursor = NO;
                config.minimumFrameInterval = CMTimeMake(1, 2);

                TPAudioSink *sink = [[TPAudioSink alloc] init];
                SCStream *stream = [[SCStream alloc] initWithFilter:filter
                                                      configuration:config
                                                           delegate:nil];
                NSError *addError = nil;
                [stream addStreamOutput:sink
                                   type:SCStreamOutputTypeAudio
                     sampleHandlerQueue:dispatch_queue_create(
                                             "com.gaganrajput11.stealthprompter.system-audio",
                                             DISPATCH_QUEUE_SERIAL)
                                  error:&addError];
                if (addError != nil) {
                    tp_report(2, addError.localizedDescription);
                    return;
                }

                sink.stream = stream;
                gSink = sink;

                [stream startCaptureWithCompletionHandler:^(NSError *startError) {
                    if (startError != nil) {
                        tp_report(1, startError.localizedDescription);
                    } else {
                        tp_report(0, @"capturing system audio");
                    }
                }];
            }];
    } else {
        tp_report(2, @"system audio capture requires macOS 13 or newer");
    }
}

void tp_system_audio_stop(void) {
    gAudioCb = NULL;
    TPAudioSink *sink = gSink;
    gSink = nil;
    if (sink != nil) {
        [sink.stream stopCaptureWithCompletionHandler:^(NSError *error) {
            (void)error;
        }];
        sink.stream = nil; // break the sink <-> stream retain cycle
    }
}



