// Developer-only bootstrap for the pinned GPUI 0.2.2 application class.
// No singleton class replacement, superclass mutation or delegate replacement.
#import <AppKit/AppKit.h>
#import <objc/message.h>
#import <objc/runtime.h>
#include <cstring>
#include <string>
#include <pthread.h>
#include <dispatch/dispatch.h>
#include <cmath>
#ifdef FELINAUT_CEF_RUNTIME
extern "C" Protocol *felinaut_cef_application_protocol();
extern "C" bool felinaut_cef_has_application_protocols(id application);
#endif

namespace {
Class probe_class = Nil;
Class gpui_class = Nil;
BOOL handling_event = NO;
bool close_requested = false;
bool closing_allowed = false;
NSView *container = nil;
NSView *container_parent = nil;

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
#ifdef FELINAUT_CEF_RUNTIME
    Protocol *protocol = felinaut_cef_application_protocol();
    if (!protocol || !class_addProtocol(candidate, protocol)) {
        objc_disposeClassPair(candidate);
        return 10;
    }
#endif
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

extern "C" bool felinaut_cef_verify_protocols() {
#ifdef FELINAUT_CEF_RUNTIME
    return owns_application() && felinaut_cef_has_application_protocols(NSApp);
#else
    return false;
#endif
}

extern "C" bool felinaut_cef_defer_cleanup(void (*callback)()) {
    if (!owns_application() || !callback) return false;
    // Run outside the serial main GCD queue/GPUI task. CEF shutdown may need
    // native-loop work serviced by that queue while draining its own threads.
    CFRunLoopPerformBlock(CFRunLoopGetMain(), kCFRunLoopCommonModes, ^{
        @autoreleasepool { callback(); }
    });
    CFRunLoopWakeUp(CFRunLoopGetMain());
    return true;
}

extern "C" void *felinaut_cef_attach_container(void *parent_pointer) {
    if (!owns_application() || !parent_pointer || container) return nullptr;
    NSView *parent = static_cast<NSView *>(parent_pointer);
    if (![parent isKindOfClass:[NSView class]] || ![parent window]) return nullptr;
    container_parent = [parent retain];
    container = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, 1, 1)];
    [container setHidden:YES];
    [parent addSubview:container];
    return container;
}

extern "C" bool felinaut_cef_resize_container(double x, double y, double width, double height) {
    if (!owns_application() || !container || ![container_parent window] ||
        !std::isfinite(x) || !std::isfinite(y) || !std::isfinite(width) || !std::isfinite(height) ||
        width < 0 || height < 0 || width > 16384 || height > 16384) return false;
    if (width == 0 || height == 0) {
        [container setHidden:YES];
        return true;
    }
    const double native_y = [container_parent isFlipped] ? y :
        [container_parent bounds].size.height - y - height;
    [container setFrame:NSMakeRect(x, native_y, width, height)];
    for (NSView *child in [container subviews]) [child setFrame:[container bounds]];
    [container setHidden:NO];
    return true;
}

extern "C" bool felinaut_cef_resize_window(double width, double height) {
    if (!owns_application() || !container || ![container_parent window] ||
        !std::isfinite(width) || !std::isfinite(height) || width < 100 || height < 100 ||
        width > 16384 || height > 16384) return false;
    [[container_parent window] setContentSize:NSMakeSize(width, height)];
    return true;
}

extern "C" bool felinaut_cef_click(double x, double y) {
    if (!owns_application() || !container || ![container window] ||
        !std::isfinite(x) || !std::isfinite(y) || x < 0 || y < 0 ||
        x >= [container bounds].size.width || y >= [container bounds].size.height) return false;
    NSWindow *window = [container window];
    [window makeKeyAndOrderFront:nil];
    const NSPoint local = NSMakePoint(x, [container isFlipped] ? y : [container bounds].size.height - y);
    const NSPoint location = [container convertPoint:local toView:nil];
    const NSPoint in_parent = [container convertPoint:local toView:[container superview]];
    NSView *target = [[container hitTest:in_parent] retain];
    if (!target || target == container || ![target isDescendantOf:container]) { [target release]; return false; }
    fprintf(stderr, "CEF native pointer target %s, container %.0fx%.0f, window visible %d\n",
        object_getClassName(target), [container bounds].size.width, [container bounds].size.height, [window isVisible]);
    @try {
        [window makeFirstResponder:target];
        event_scope(^{
            for (NSEventType type : {NSEventTypeMouseMoved, NSEventTypeLeftMouseDown, NSEventTypeLeftMouseUp}) {
                NSEvent *event = [NSEvent mouseEventWithType:type location:location modifierFlags:0
                    timestamp:[[NSProcessInfo processInfo] systemUptime] windowNumber:[window windowNumber]
                    context:nil eventNumber:0 clickCount:(type == NSEventTypeMouseMoved ? 0 : 1) pressure:1.0];
                if (type == NSEventTypeMouseMoved) [target mouseMoved:event];
                else if (type == NSEventTypeLeftMouseDown) [target mouseDown:event];
                else [target mouseUp:event];
            }
        });
    } @catch (NSException *) { return false; }
      @finally { [target release]; }
    return true;
}

extern "C" bool felinaut_cef_type_ascii(const char *text) {
    if (!owns_application() || !container || ![container window] || !text || std::strlen(text) > 128)
        return false;
    for (const unsigned char *p = reinterpret_cast<const unsigned char *>(text); *p; ++p)
        if (*p < 32 || *p > 126) return false;
    id responder = [[[container window] firstResponder] retain];
    if (![responder isKindOfClass:[NSView class]] || ![static_cast<NSView *>(responder) isDescendantOf:container] ||
        ![responder respondsToSelector:@selector(insertText:replacementRange:)]) {
        [responder release]; return false;
    }
    // Deliberately narrow US-layout alphabet/space fixture, not general IME automation.
    constexpr unsigned short codes[] = {0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6};
    for (const char *p = text; *p; ++p) {
        if (*p != ' ' && !(*p >= 'a' && *p <= 'z') && !(*p >= 'A' && *p <= 'Z')) {
            [responder release]; return false;
        }
    }
    NSWindow *window = [container window];
    @try {
        event_scope(^{
            for (const char *p = text; *p; ++p) {
                const bool shifted = *p >= 'A' && *p <= 'Z';
                const char lower = shifted ? *p - 'A' + 'a' : *p;
                const unsigned short code = *p == ' ' ? 49 : codes[lower - 'a'];
                NSString *characters = [NSString stringWithFormat:@"%c", *p];
                NSString *unmodified = [NSString stringWithFormat:@"%c", lower];
                for (NSEventType type : {NSEventTypeKeyDown, NSEventTypeKeyUp}) {
                    NSEvent *event = [NSEvent keyEventWithType:type location:NSZeroPoint
                        modifierFlags:(shifted ? NSEventModifierFlagShift : 0)
                        timestamp:[[NSProcessInfo processInfo] systemUptime]
                        windowNumber:[window windowNumber] context:nil characters:characters
                        charactersIgnoringModifiers:unmodified isARepeat:NO keyCode:code];
                    if (type == NSEventTypeKeyDown) [responder keyDown:event];
                    else [responder keyUp:event];
                }
            }
        });
    } @catch (NSException *) { return false; }
      @finally { [responder release]; }
    return true;
}

extern "C" bool felinaut_cef_release_container() {
    if (!owns_application()) return false;
    [container removeFromSuperview];
    [container release];
    container = nil;
    [container_parent release];
    container_parent = nil;
    return true;
}

extern "C" bool felinaut_cef_create_conflicting_application() {
    if (!pthread_main_np() || NSApp) return false;
    [NSApplication sharedApplication];
    return NSApp != nil;
}
