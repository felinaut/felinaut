// Compile the SDK's public Objective-C protocols without importing its unrelated
// C++ wrapper templates into the application's Objective-C++ implementation.
#import "include/cef_application_mac.h"
#include <stdbool.h>

Protocol *felinaut_cef_application_protocol(void) {
    return @protocol(CefAppProtocol);
}

bool felinaut_cef_has_application_protocols(id application) {
    return [application conformsToProtocol:@protocol(CefAppProtocol)] &&
        [application conformsToProtocol:@protocol(CrAppControlProtocol)] &&
        [application conformsToProtocol:@protocol(CrAppProtocol)];
}
