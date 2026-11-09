//! Bare digit keys (1–9, 0) as global objection triggers, without holding Control.
//!
//! This is an Accessibility event tap so it can be *conditional*: a digit is only swallowed while
//! a listening session is active and the user has armed the mode. Every other keystroke, and every
//! modifier combination, passes straight through untouched — typing numbers elsewhere is unchanged.
//!
//! Needs the Accessibility permission (`kTCCServiceAccessibility`). With ad-hoc signing macOS
//! re-asks after each rebuild, like Speech.

#import <ApplicationServices/ApplicationServices.h>
#import <Foundation/Foundation.h>
#include <pthread.h>
#include <stdint.h>

typedef int (*tp_digit_cb)(int digit); // 1 = handled, swallow the keystroke

static CFMachPortRef gTap = NULL;
static CFRunLoopSourceRef gSource = NULL;
static CFRunLoopRef gRunLoop = NULL;
static tp_digit_cb gCb = NULL;

/// ANSI key codes for the top row and the numeric keypad.
static int digit_for_keycode(int64_t code) {
    switch (code) {
        case 18: return 1;
        case 19: return 2;
        case 20: return 3;
        case 21: return 4;
        case 23: return 5;
        case 22: return 6;
        case 26: return 7;
        case 28: return 8;
        case 25: return 9;
        case 29: return 0;
        case 83: return 1;
        case 84: return 2;
        case 85: return 3;
        case 86: return 4;
        case 87: return 5;
        case 88: return 6;
        case 89: return 7;
        case 91: return 8;
        case 92: return 9;
        case 82: return 0;
        default: return -1;
    }
}

static CGEventRef tap_callback(CGEventTapProxy proxy, CGEventType type, CGEventRef event,
                               void *refcon) {
    (void)proxy;
    (void)refcon;
    if (type == kCGEventTapDisabledByTimeout || type == kCGEventTapDisabledByUserInput) {
        if (gTap != NULL) {
            CGEventTapEnable(gTap, true);
        }
        return event;
    }
    if (type != kCGEventKeyDown || gCb == NULL) {
        return event;
    }
    if (CGEventGetIntegerValueField(event, kCGKeyboardEventAutorepeat) != 0) {
        return event;
    }
    // Any real modifier means the user is doing something else (⌘1 in a browser, ⌃C, …).
    CGEventFlags interesting = kCGEventFlagMaskCommand | kCGEventFlagMaskControl |
                               kCGEventFlagMaskAlternate | kCGEventFlagMaskShift;
    if ((CGEventGetFlags(event) & interesting) != 0) {
        return event;
    }
    int digit = digit_for_keycode(CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode));
    if (digit < 0) {
        return event;
    }
    if (gCb(digit) == 1) {
        return NULL; // handled: swallow so the digit is not typed into the dialer
    }
    return event;
}

static void *run_loop_thread(void *unused) {
    (void)unused;
    gRunLoop = CFRunLoopGetCurrent();
    CFRunLoopAddSource(gRunLoop, gSource, kCFRunLoopCommonModes);
    CGEventTapEnable(gTap, true);
    CFRunLoopRun();
    return NULL;
}

/// 0 = running, 2 = needs Accessibility permission, 3 = tap could not be installed.
int tp_bare_keys_start(tp_digit_cb cb) {
    if (gTap != NULL) {
        gCb = cb;
        return 0;
    }
    gCb = cb;

    NSDictionary *options = @{(__bridge NSString *)kAXTrustedCheckOptionPrompt : @YES};
    if (!AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)options)) {
        return 2;
    }

    gTap = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault,
                            CGEventMaskBit(kCGEventKeyDown), tap_callback, NULL);
    if (gTap == NULL) {
        return 3;
    }
    gSource = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, gTap, 0);
    pthread_t thread;
    if (pthread_create(&thread, NULL, run_loop_thread, NULL) != 0) {
        CFRelease(gSource);
        gSource = NULL;
        CFRelease(gTap);
        gTap = NULL;
        return 3;
    }
    pthread_detach(thread);
    return 0;
}

void tp_bare_keys_stop(void) {
    gCb = NULL;
    if (gTap != NULL) {
        CGEventTapEnable(gTap, false);
    }
    if (gRunLoop != NULL) {
        CFRunLoopStop(gRunLoop);
    }
    if (gSource != NULL) {
        CFRelease(gSource);
        gSource = NULL;
    }
    if (gTap != NULL) {
        CFRelease(gTap);
        gTap = NULL;
    }
}
