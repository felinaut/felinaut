// GPUI 0.2.2 bootstrap and per-page AppKit ownership. No delegate replacement.
#import <AppKit/AppKit.h>
#import <objc/message.h>
#import <objc/runtime.h>
#include <cstring>
#include <string>
#include <pthread.h>
#include <dispatch/dispatch.h>
#include <cmath>
#include <dlfcn.h>
#ifdef FELINAUT_CEF_RUNTIME
extern "C" Protocol *felinaut_cef_application_protocol();
extern "C" bool felinaut_cef_has_application_protocols(id application);
#endif

namespace {
Class host_class = Nil;
Class gpui_class = Nil;
BOOL handling_event = NO;
bool close_requested = false;
bool closing_allowed = false;
BOOL is_handling(id, SEL) { return handling_event; }
void set_handling(id, SEL, BOOL value) { handling_event = value; }
void send_event(id self, SEL command, NSEvent *event) {
    const BOOL previous = handling_event;
    handling_event = YES;
    @try {
        struct objc_super parent = {self, gpui_class};
        reinterpret_cast<void (*)(struct objc_super *, SEL, NSEvent *)>(objc_msgSendSuper)(
            &parent, command, event);
    } @finally { handling_event = previous; }
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
    if (!pthread_main_np() || !NSApp || !host_class) return false;
    for (Class current = object_getClass(NSApp); current; current = class_getSuperclass(current))
        if (current == host_class) return true;
    return false;
}
struct Container {
    NSView *parent;
    NSView *view;
    NSWindow *window;
    bool detached = false;
    bool visible = false;
    bool nonempty = false;
    Container(NSView *p) : parent([p retain]),
        view([[NSView alloc] initWithFrame:NSMakeRect(0, 0, 1, 1)]),
        window([[p window] retain]) {
        [view setHidden:YES];
        [parent addSubview:view];
    }
    ~Container() {
        [view removeFromSuperview];
        [view release];
        [parent release];
        [window release];
    }
    bool live() const { return !detached && [parent window] == window; }
};
}
extern "C" bool felinaut_cef_is_main_thread() { return pthread_main_np(); }
extern "C" bool felinaut_cef_verify_runtime(const char *path) {
    if (!pthread_main_np() || !path) return false;
    void *image = dlopen(path, RTLD_NOW | RTLD_NOLOAD);
    if (!image) return false;
    auto version = reinterpret_cast<int (*)(int)>(dlsym(image, "cef_version_info"));
    const bool matches = version && version(0) == 156 && version(1) == 0 && version(2) == 26 && version(3) == 3671 &&
        version(4) == 156 && version(5) == 0 && version(6) == 8078 && version(7) == 88;
    dlclose(image);
    return matches;
}
extern "C" int felinaut_cef_bootstrap_application() {
    if (!pthread_main_np()) return 1;
    if (NSApp) return 2;
    gpui_class = objc_getClass("GPUIApplication");
    if (!gpui_class) return 3;
    Ivar platform = class_getInstanceVariable(gpui_class, "platform");
    if (!platform || std::strcmp(ivar_getTypeEncoding(platform), "^v") != 0) return 4;
    if (objc_getClass("FelinautCEFApplication")) return 5;
    Class candidate = objc_allocateClassPair(gpui_class, "FelinautCEFApplication", 0);
    if (!candidate) return 6;
    const std::string getter = std::string(@encode(BOOL)) + "@:";
    const std::string setter = std::string("v@:") + @encode(BOOL);
    if (!class_addMethod(candidate, @selector(isHandlingSendEvent), reinterpret_cast<IMP>(is_handling), getter.c_str()) ||
        !class_addMethod(candidate, @selector(setHandlingSendEvent:), reinterpret_cast<IMP>(set_handling), setter.c_str()) ||
        !class_addMethod(candidate, @selector(sendEvent:), reinterpret_cast<IMP>(send_event), "v@:@") ||
        !class_addMethod(candidate, @selector(terminate:), reinterpret_cast<IMP>(terminate), "v@:@")) {
        objc_disposeClassPair(candidate); return 7;
    }
#ifdef FELINAUT_CEF_RUNTIME
    Protocol *protocol = felinaut_cef_application_protocol();
    if (!protocol || !class_addProtocol(candidate, protocol)) {
        objc_disposeClassPair(candidate); return 10;
    }
#endif
    objc_registerClassPair(candidate);
    host_class = candidate;
    if (NSApp) return 2;
    @try { [host_class sharedApplication]; }
    @catch (NSException *) { return 8; }
    return owns_application() ? 0 : 9;
}
extern "C" bool felinaut_cef_verify_application() {
    if (!owns_application()) return false;
    id delegate = [NSApp delegate];
    if (!delegate || std::strcmp(class_getName(object_getClass(delegate)), "GPUIApplicationDelegate") != 0) return false;
    Ivar inherited = class_getInstanceVariable(host_class, "platform");
    if (!inherited || inherited != class_getInstanceVariable(gpui_class, "platform")) return false;
    void *platform = nullptr;
    std::memcpy(&platform, reinterpret_cast<const char *>(NSApp) + ivar_getOffset(inherited), sizeof(platform));
    return platform != nullptr;
}
extern "C" bool felinaut_cef_verify_protocols() {
#ifdef FELINAUT_CEF_RUNTIME
    return owns_application() && felinaut_cef_has_application_protocols(NSApp);
#else
    return false;
#endif
}
extern "C" bool felinaut_cef_close_requested() { return owns_application() && close_requested; }
extern "C" bool felinaut_cef_finish_termination() {
    if (!owns_application() || !close_requested || closing_allowed) return false;
    closing_allowed = true;
    dispatch_async(dispatch_get_main_queue(), ^{ [NSApp terminate:nil]; });
    return true;
}
extern "C" bool felinaut_cef_defer_cleanup(void (*callback)()) {
    if (!owns_application() || !callback) return false;
    // CEF shutdown must not hold the serial main GCD queue while joining threads.
    CFRunLoopPerformBlock(CFRunLoopGetMain(), kCFRunLoopCommonModes, ^{
        @autoreleasepool { callback(); }
    });
    CFRunLoopWakeUp(CFRunLoopGetMain());
    return true;
}
extern "C" void *felinaut_cef_container_create(void *pointer) {
    if (!owns_application() || !pointer) return nullptr;
    NSView *parent = static_cast<NSView *>(pointer);
    if (![parent isKindOfClass:[NSView class]] || ![parent window]) return nullptr;
    return new Container(parent);
}
extern "C" void *felinaut_cef_container_view(Container *owner) {
    return owns_application() && owner && owner->live() ? owner->view : nullptr;
}
extern "C" bool felinaut_cef_container_bounds(Container *owner, double x, double y, double width, double height) {
    if (!owns_application() || !owner || !owner->live() ||
        !std::isfinite(x) || !std::isfinite(y) || !std::isfinite(width) || !std::isfinite(height) ||
        width < 0 || height < 0 || width > 16384 || height > 16384) return false;
    const double native_y = [owner->parent isFlipped] ? y :
        [owner->parent bounds].size.height - y - height;
    [owner->view setFrame:NSMakeRect(x, native_y, width, height)];
    for (NSView *child in [owner->view subviews]) [child setFrame:[owner->view bounds]];
    owner->nonempty = width > 0 && height > 0;
    [owner->view setHidden:!(owner->visible && owner->nonempty)];
    return true;
}
extern "C" bool felinaut_cef_container_layout(Container *owner) {
    if (!owns_application() || !owner || !owner->live()) return false;
    for (NSView *child in [owner->view subviews]) [child setFrame:[owner->view bounds]];
    return true;
}
extern "C" bool felinaut_cef_container_visible(Container *owner, bool visible) {
    if (!owns_application() || !owner || !owner->live()) return false;
    owner->visible = visible;
    [owner->view setHidden:!(visible && owner->nonempty)];
    return true;
}
extern "C" bool felinaut_cef_container_focus_parent(Container *owner) {
    if (!owns_application() || !owner || !owner->live()) return false;
    return [owner->window makeFirstResponder:owner->parent];
}
extern "C" bool felinaut_cef_container_detach(Container *owner) {
    if (!owns_application() || !owner) return false;
    if (owner->detached) return true;
    owner->detached = true;
    [owner->view setHidden:YES];
    [owner->view removeFromSuperview];
    // DoClose has returned true. Destroy only CEF's native child, never GPUI's
    // window; keep our view, parent AND window retained through OnBeforeClose.
    NSArray *children = [[owner->view subviews] copy];
    for (NSView *child in children) [child removeFromSuperview];
    [children release];
    return true;
}
extern "C" bool felinaut_cef_container_release(Container *owner) {
    if (!owns_application() || !owner) return false;
    delete owner;
    return true;
}
