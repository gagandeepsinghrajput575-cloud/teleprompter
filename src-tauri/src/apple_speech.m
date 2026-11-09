// apple_speech.m - Objective-C bridge for Apple on-device Speech Recognition.
#import <Foundation/Foundation.h>
#import <Speech/Speech.h>
#import <AVFoundation/AVFoundation.h>
#include "apple_speech.h"

@interface AppleSpeechSession : NSObject
@property (nonatomic, strong) SFSpeechRecognizer *recognizer;
@property (nonatomic, strong) SFSpeechAudioBufferRecognitionRequest *currentRequest;
@property (nonatomic, strong) SFSpeechRecognitionTask *currentTask;
@property (nonatomic, strong) AVAudioFormat *audioFormat;
@property (nonatomic, copy) NSString *latestTranscript;
@property (nonatomic, assign) AppleSpeechTranscriptCallback transcriptCallback;
@property (nonatomic, assign) AppleSpeechStatusCallback statusCallback;
@property (nonatomic, assign) void *userData;
@property (nonatomic, assign) BOOL isStopping;
@property (nonatomic, strong) dispatch_queue_t syncQueue;
@end

@implementation AppleSpeechSession

- (instancetype)initWithTranscriptCallback:(AppleSpeechTranscriptCallback)transcriptCb
                            statusCallback:(AppleSpeechStatusCallback)statusCb
                                  userData:(void *)userData {
    self = [super init];
    if (self) {
        _transcriptCallback = transcriptCb;
        _statusCallback = statusCb;
        _userData = userData;
        _isStopping = NO;
        _latestTranscript = @"";
        _syncQueue = dispatch_queue_create("com.stealthprompter.apple_speech", DISPATCH_QUEUE_SERIAL);
        
        NSLocale *locale = [NSLocale localeWithLocaleIdentifier:@"en-US"];
        _recognizer = [[SFSpeechRecognizer alloc] initWithLocale:locale];
        if (!_recognizer) {
            NSLog(@"APPLE_SPEECH_ERROR: Failed to initialize SFSpeechRecognizer with en-US");
            return nil;
        }
        
        _audioFormat = [[AVAudioFormat alloc] initWithCommonFormat:AVAudioPCMFormatInt16
                                                        sampleRate:16000.0
                                                          channels:1
                                                       interleaved:NO];
        if (!_audioFormat) {
            NSLog(@"APPLE_SPEECH_ERROR: Failed to initialize AVAudioFormat 16kHz Int16");
            return nil;
        }
        
        [self startNewTask];
    }
    return self;
}

- (void)startNewTask {
    if (self.isStopping) return;
    
    SFSpeechAudioBufferRecognitionRequest *request = [[SFSpeechAudioBufferRecognitionRequest alloc] init];
    request.shouldReportPartialResults = YES;
    if (self.recognizer.supportsOnDeviceRecognition) {
        request.requiresOnDeviceRecognition = YES;
    }
    
    self.currentRequest = request;
    self.latestTranscript = @"";
    
    __weak typeof(self) weakSelf = self;
    self.currentTask = [self.recognizer recognitionTaskWithRequest:request resultHandler:^(SFSpeechRecognitionResult * _Nullable result, NSError * _Nullable error) {
        AppleSpeechSession *strongSelf = weakSelf;
        if (!strongSelf || strongSelf.isStopping) return;
        
        if (error) {
            // Ignore normal cancellation/ending codes when audio ends or request rotates
            NSInteger code = error.code;
            if (code == 216 || code == 301 || [error.domain isEqualToString:@"kAFAssistantErrorDomain"]) {
                return;
            }
            if (strongSelf.statusCallback) {
                strongSelf.statusCallback(1, [error.localizedDescription UTF8String], strongSelf.userData);
            }
            return;
        }
        
        if (result) {
            NSString *formatted = result.bestTranscription.formattedString;
            BOOL isFinal = result.isFinal;
            strongSelf.latestTranscript = formatted;
            
            if (formatted.length > 0 && strongSelf.transcriptCallback) {
                strongSelf.transcriptCallback([formatted UTF8String], isFinal, strongSelf.userData);
            }
        }
    }];
    
    if (self.statusCallback) {
        self.statusCallback(0, "Apple On-Device STT listening", self.userData);
    }
}

- (void)feedAudioSamples:(const int16_t *)samples count:(size_t)count {
    if (self.isStopping || !samples || count == 0) return;
    
    dispatch_sync(self.syncQueue, ^{
        if (self.isStopping || !self.currentRequest) return;
        
        AVAudioPCMBuffer *buffer = [[AVAudioPCMBuffer alloc] initWithPCMFormat:self.audioFormat
                                                                 frameCapacity:(AVAudioFrameCount)count];
        if (!buffer) return;
        
        buffer.frameLength = (AVAudioFrameCount)count;
        if (buffer.int16ChannelData && buffer.int16ChannelData[0]) {
            memcpy(buffer.int16ChannelData[0], samples, count * sizeof(int16_t));
            @try {
                [self.currentRequest appendAudioPCMBuffer:buffer];
            } @catch (NSException *exception) {
                NSLog(@"APPLE_SPEECH_EXCEPTION on append: %@", exception.reason);
            }
        }
    });
}

- (void)rotateRequest {
    if (self.isStopping) return;
    
    dispatch_async(self.syncQueue, ^{
        if (self.isStopping) return;
        
        SFSpeechAudioBufferRecognitionRequest *oldRequest = self.currentRequest;
        
        // Start fresh task and request for upcoming speech
        [self startNewTask];
        
        // Finish old request gracefully so its final result is emitted if any
        if (oldRequest) {
            [oldRequest endAudio];
        }
    });
}

- (void)stop {
    self.isStopping = YES;
    dispatch_sync(self.syncQueue, ^{
        if (self.currentRequest) {
            [self.currentRequest endAudio];
            self.currentRequest = nil;
        }
        if (self.currentTask) {
            [self.currentTask cancel];
            self.currentTask = nil;
        }
    });
}

@end

int apple_speech_permission_status(void) {
    @autoreleasepool {
        SFSpeechRecognizerAuthorizationStatus status = [SFSpeechRecognizer authorizationStatus];
        switch (status) {
            case SFSpeechRecognizerAuthorizationStatusNotDetermined: return 0;
            case SFSpeechRecognizerAuthorizationStatusDenied: return 1;
            case SFSpeechRecognizerAuthorizationStatusRestricted: return 2;
            case SFSpeechRecognizerAuthorizationStatusAuthorized: return 3;
            default: return -1;
        }
    }
}

int apple_speech_request_permission(double timeout_seconds) {
    @autoreleasepool {
        int current = apple_speech_permission_status();
        if (current != 0) { // NotDetermined = 0
            return current;
        }
        
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);
        
        [SFSpeechRecognizer requestAuthorization:^(SFSpeechRecognizerAuthorizationStatus status) {
            (void)status;
            dispatch_semaphore_signal(sem);
        }];
        
        int64_t timeout_ns = (int64_t)(timeout_seconds * NSEC_PER_SEC);
        dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, timeout_ns));
        
        return apple_speech_permission_status();
    }
}

AppleSpeechSessionRef apple_speech_start(
    AppleSpeechTranscriptCallback on_transcript,
    AppleSpeechStatusCallback on_status,
    void *user_data
) {
    @autoreleasepool {
        AppleSpeechSession *session = [[AppleSpeechSession alloc] initWithTranscriptCallback:on_transcript
                                                                              statusCallback:on_status
                                                                                    userData:user_data];
        if (!session) return NULL;
        return (__bridge_retained void *)session;
    }
}

void apple_speech_feed_audio(AppleSpeechSessionRef session_ref, const int16_t *samples, size_t count) {
    if (!session_ref) return;
    @autoreleasepool {
        AppleSpeechSession *session = (__bridge AppleSpeechSession *)session_ref;
        [session feedAudioSamples:samples count:count];
    }
}

void apple_speech_rotate_request(AppleSpeechSessionRef session_ref) {
    if (!session_ref) return;
    @autoreleasepool {
        AppleSpeechSession *session = (__bridge AppleSpeechSession *)session_ref;
        [session rotateRequest];
    }
}

void apple_speech_stop(AppleSpeechSessionRef session_ref) {
    if (!session_ref) return;
    @autoreleasepool {
        AppleSpeechSession *session = (__bridge_transfer AppleSpeechSession *)session_ref;
        [session stop];
    }
}
