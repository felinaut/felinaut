// Developer-only bootstrap for the pinned GPUI 0.2.2 application class.
// No singleton class replacement, superclass mutation or delegate replacement.
#import <AppKit/AppKit.h>
#import <objc/message.h>
#import <objc/runtime.h>
#include <cstring>
#include <string>
#include <pthread.h>
#include <dispatch/dispatch.h>

namespace {
Class probe_class = Nil;
Class gpui_class = Nil;
BOOL handling_event = NO;
bool close_requested = false;
bool closing_allowed = false;

// CEF's public CrAppProtocol/CrAppControlProtocol require these two selectors.
// Actual framework protocol conformance is checked when CEF is integrated;
// this SDK-free checkpoint proves dispatch/lifetime, not a running CEF engine.
BOOL is_handling(id, SEL) { return handling_event; }
void set_handling(id, SEL, BOOL value) { handling_event = value; }

void event_scope(void (^dispatch)(void)) {
    const BOOL previous = handling_event;
    handling_event = YES;
    @try {
        dispatch();
    } @finally {
        // Includes nested dispatch and Objective-C exceptions, not just Rust drops.
        handling_event = previous;
    }
}

void send_event(id self, SEL command, NSEvent *event) {
    event_scope(^{
        struct objc_super parent = {self, gpui_class};
        reinterpret_cast<void (*)(struct objc_super *, SEL, NSEvent *)>(objc_msgSendSuper)(
            &parent, command, event);
    });
}

void terminate(id self, SEL command, id sender) {
    close_requested = true;
    if (closing_allowed) {
        struct objc_super parent = {self, gpui_class};
        reinterpret_cast<void (*)(struct objc_super *, SEL, id)>(objc_msgSendSuper)(
            &parent, command, sender);
    }
}

bool owns_application() {
    if (!pthread_main_np() || !NSApp || !probe_class) return false;
    // AppKit may install an NSKVONotifying subclass after launch. Require actual
    // ancestry, not an exact isa match; never accept a sibling NSApplication.
    for (Class current = object_getClass(NSApp); current; current = class_getSuperclass(current)) {
        if (current == probe_class) return true;
    }
    return false;
}
} // namespace

extern "C" int felinaut_cef_bootstrap_application() {
    if (!pthread_main_np()) return 1;
    // Read the real global: sending sharedApplication here would create it.
    if (NSApp) return 2;
    gpui_class = objc_getClass("GPUIApplication");
    if (!gpui_class) return 3;
    Ivar platform = class_getInstanceVariable(gpui_class, "platform");
    if (!platform || std::strcmp(ivar_getTypeEncoding(platform), "^v") != 0) return 4;
    if (objc_getClass("FelinautCEFProbeApplication")) return 5;
    Class candidate = objc_allocateClassPair(gpui_class, "FelinautCEFProbeApplication", 0);
    if (!candidate) return 6;
    // Match the SDK's BOOL ABI rather than assuming signed char on arm64.
    const std::string getter = std::string(@encode(BOOL)) + "@:";
    const std::string setter = std::string("v@:") + @encode(BOOL);
    if (!class_addMethod(candidate, @selector(isHandlingSendEvent), reinterpret_cast<IMP>(is_handling), getter.c_str()) ||
        !class_addMethod(candidate, @selector(setHandlingSendEvent:), reinterpret_cast<IMP>(set_handling), setter.c_str()) ||
        !class_addMethod(candidate, @selector(sendEvent:), reinterpret_cast<IMP>(send_event), "v@:@") ||
        !class_addMethod(candidate, @selector(terminate:), reinterpret_cast<IMP>(terminate), "v@:@")) {
        objc_disposeClassPair(candidate);
        return 7;
    }
    objc_registerClassPair(candidate);
    probe_class = candidate;
    if (NSApp) return 2;
    @try {
        [probe_class sharedApplication];
    } @catch (NSException *) {
        return 8;
    }
    return owns_application() ? 0 : 9;
}

extern "C" bool felinaut_cef_verify_application() {
    if (!pthread_main_np()) return false;
    if (!owns_application()) {
        fprintf(stderr, "Probe application identity mismatch: %s\n", NSApp ? object_getClassName(NSApp) : "nil");
        return false;
    }
    id delegate = [NSApp delegate];
    if (!delegate || std::strcmp(class_getName(object_getClass(delegate)), "GPUIApplicationDelegate") != 0) {
        fprintf(stderr, "Probe delegate identity mismatch: %s\n", delegate ? object_getClassName(delegate) : "nil");
        return false;
    }
    Ivar inherited = class_getInstanceVariable(probe_class, "platform");
    Ivar original = class_getInstanceVariable(gpui_class, "platform");
    if (!inherited || inherited != original) {
        fprintf(stderr, "Probe inherited platform ivar mismatch\n");
        return false;
    }
    // The inherited pointer is initialized only by GPUI. Never write or dereference it.
    void *platform = nullptr;
    std::memcpy(&platform, reinterpret_cast<const char *>(NSApp) + ivar_getOffset(inherited), sizeof(platform));
    if (!platform) fprintf(stderr, "Probe inherited platform state is null\n");
    return platform != nullptr;
}

extern "C" bool felinaut_cef_verify_event_scope() {
    if (!owns_application()) return false;
    __block bool nested = false;
    __block bool caught = false;
    handling_event = NO;
    event_scope(^{
        event_scope(^{ nested = handling_event == YES; });
        nested = nested && handling_event == YES;
    });
    if (!nested || handling_event != NO) return false;
    handling_event = YES;
    @try {
        event_scope(^{ [NSException raise:@"FelinautScopeProbe" format:@"Intentional scoped-dispatch test"]; });
    } @catch (NSException *exception) {
        caught = [[exception name] isEqualToString:@"FelinautScopeProbe"];
    }
    if (!caught || handling_event != YES) return false;
    handling_event = NO;
    NSEvent *event = [NSEvent otherEventWithType:NSEventTypeApplicationDefined
        location:NSZeroPoint modifierFlags:0 timestamp:0 windowNumber:0
        context:nil subtype:0 data1:0 data2:0];
    [NSApp sendEvent:event];
    return handling_event == NO;
}

extern "C" bool felinaut_cef_close_requested() {
    return owns_application() && close_requested;
}

extern "C" bool felinaut_cef_finish_termination() {
    if (!owns_application() || !close_requested || closing_allowed) return false;
    closing_allowed = true;
    // Called outside any GPUI borrow; normal AppKit termination reaches GPUI's delegate.
    dispatch_async(dispatch_get_main_queue(), ^{ [NSApp terminate:nil]; });
    return true;
}

extern "C" bool felinaut_cef_create_conflicting_application() {
    if (!pthread_main_np() || NSApp) return false;
    [NSApplication sharedApplication];
    return NSApp != nil;
}
