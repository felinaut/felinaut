// External CEF message pumping only: this file has no CEF SDK dependency.
#import <AppKit/AppKit.h>
#import <CoreFoundation/CoreFoundation.h>
#include <dispatch/dispatch.h>
#include <pthread.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <limits>
#include <mutex>
#include <thread>

extern "C" bool felinaut_cef_pump_start(void (*callback)());
extern "C" void felinaut_cef_pump_schedule(int64_t delay_ms);
extern "C" bool felinaut_cef_pump_stop();

namespace {
using Clock = std::chrono::steady_clock;
using Callback = void (*)();
using Generation = uintptr_t;
constexpr int64_t max_delay_ms = 24 * 60 * 60 * 1000;

struct Pump {
    std::mutex mutex;
    Generation generation = 0;
    bool active = false;
    bool scheduled = false;
    bool wake_pending = false;
    bool dispatch_pending = false;
    // Survives stop until the callback on the stack has returned.
    bool pumping = false;
    Clock::time_point deadline{};
    Callback callback = nullptr;
    CFRunLoopSourceRef source = nullptr;
    CFRunLoopTimerRef timer = nullptr;
};

// Deliberately process-lifetime: queued blocks never capture a session/CEF pointer.
Pump &pump() {
    static Pump *const state = new Pump;
    return *state;
}

Clock::time_point bounded_deadline(int64_t delay_ms, Clock::time_point now) {
    const auto delay = std::chrono::duration_cast<Clock::duration>(
        std::chrono::milliseconds(std::clamp<int64_t>(delay_ms, 0, max_delay_ms)));
    return now > Clock::time_point::max() - delay ? Clock::time_point::max() : now + delay;
}

void reconcile(Generation generation);
void timer_fired(CFRunLoopTimerRef timer, void *context);

// Called with the mutex held. Neither dispatch_async nor signaling a run-loop
// source invokes its callback synchronously. One queued GCD block per generation
// remains sufficient even if the tracking-mode source runs ahead of that block.
void wake_main(Pump &state) {
    state.wake_pending = true;
    CFRunLoopSourceSignal(state.source);
    CFRunLoopWakeUp(CFRunLoopGetMain());
    if (!state.dispatch_pending) {
        state.dispatch_pending = true;
        const Generation generation = state.generation;
        dispatch_async(dispatch_get_main_queue(), ^{
            {
                auto &current = pump();
                std::lock_guard<std::mutex> lock(current.mutex);
                if (!current.active || current.generation != generation) return;
                current.dispatch_pending = false;
            }
            reconcile(generation);
        });
    }
}

void source_fired(void *context) {
    reconcile(reinterpret_cast<Generation>(context));
}

void discard_timer(Pump &state) {
    if (state.timer) {
        CFRunLoopTimerInvalidate(state.timer);
        CFRelease(state.timer);
        state.timer = nullptr;
    }
}

void reconcile(Generation generation) {
    if (!pthread_main_np()) return;
    auto &state = pump();
    std::unique_lock<std::mutex> lock(state.mutex);
    if (!state.active || state.generation != generation || !state.wake_pending) return;
    state.wake_pending = false;
    discard_timer(state);
    // A nested native loop must not repeatedly arm immediate timers while the
    // outer CEF call is still on the stack. Its return issues the next wakeup.
    if (!state.scheduled || state.pumping) return;

    const auto remaining = state.deadline > Clock::now()
        ? std::chrono::duration<double>(state.deadline - Clock::now()).count() : 0.0;
    CFRunLoopTimerContext context = {};
    // An integer generation, never a borrowed/freed object; zero is not issued.
    context.info = reinterpret_cast<void *>(generation);
    state.timer = CFRunLoopTimerCreate(kCFAllocatorDefault,
        CFAbsoluteTimeGetCurrent() + std::max(0.0, remaining), 0, 0, 0,
        timer_fired, &context);
    if (!state.timer) {
        lock.unlock();
        fprintf(stderr, "CEF pump timer allocation failed; stopping scheduler\n");
        felinaut_cef_pump_stop();
        return;
    }
    CFRunLoopAddTimer(CFRunLoopGetMain(), state.timer, kCFRunLoopCommonModes);
    CFRunLoopAddTimer(CFRunLoopGetMain(), state.timer,
        reinterpret_cast<CFStringRef>(NSEventTrackingRunLoopMode));
}

void timer_fired(CFRunLoopTimerRef timer, void *context) {
    if (!pthread_main_np()) return;
    const auto generation = reinterpret_cast<Generation>(context);
    auto &state = pump();
    Callback callback;
    {
        std::lock_guard<std::mutex> lock(state.mutex);
        if (!state.active || state.generation != generation || state.timer != timer) return;
        discard_timer(state);
        if (!state.scheduled || state.pumping) return;
        // A worker may have replaced the deadline since this timer was armed.
        // The steady clock also protects against an early wall-clock timer fire.
        if (Clock::now() < state.deadline) {
            wake_main(state);
            return;
        }
        state.scheduled = false;
        state.pumping = true;
        callback = state.callback;
    }
    // Only deferred native-loop work calls CEF. No mutex, scheduling callback,
    // GPUI borrow or render callback may surround this call. It must not unwind
    // or throw (the Rust/CEF owner supplies its panic-catching trampoline).
    @autoreleasepool {
        callback();
    }
    {
        std::lock_guard<std::mutex> lock(state.mutex);
        state.pumping = false;
        if (state.active && state.generation == generation && state.scheduled) wake_main(state);
    }
}
} // namespace

extern "C" bool felinaut_cef_pump_start(void (*callback)()) {
    if (!pthread_main_np() || !callback) return false;
    auto &state = pump();
    std::lock_guard<std::mutex> lock(state.mutex);
    if (state.active || state.pumping || state.generation == std::numeric_limits<Generation>::max()) {
        return false;
    }
    const auto generation = state.generation + 1;
    CFRunLoopSourceContext context = {};
    context.info = reinterpret_cast<void *>(generation);
    context.perform = source_fired;
    auto source = CFRunLoopSourceCreate(kCFAllocatorDefault, 0, &context);
    if (!source) return false;
    state.generation = generation;
    state.callback = callback;
    state.source = source;
    state.scheduled = false;
    state.wake_pending = false;
    state.dispatch_pending = false;
    state.active = true;
    CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
    CFRunLoopAddSource(CFRunLoopGetMain(), source,
        reinterpret_cast<CFStringRef>(NSEventTrackingRunLoopMode));
    return true;
}

extern "C" void felinaut_cef_pump_schedule(int64_t delay_ms) {
    auto &state = pump();
    std::lock_guard<std::mutex> lock(state.mutex);
    if (!state.active) return;
    // Replacement, not min(existing, requested): the latest serialized CEF
    // notification wins, including moving an immediate task to a later time.
    state.deadline = bounded_deadline(delay_ms, Clock::now());
    state.scheduled = true;
    wake_main(state);
}

extern "C" bool felinaut_cef_pump_stop() {
    if (!pthread_main_np()) return false;
    auto &state = pump();
    std::lock_guard<std::mutex> lock(state.mutex);
    state.active = false;
    state.callback = nullptr;
    state.scheduled = false;
    state.deadline = {};
    state.wake_pending = false;
    state.dispatch_pending = false;
    discard_timer(state);
    if (state.source) {
        CFRunLoopSourceInvalidate(state.source);
        CFRelease(state.source);
        state.source = nullptr;
    }
    // Stale blocks retain only the old generation. Even after a restart they
    // cannot select the new callback. A callback already on the stack may finish.
    return true;
}

namespace {
// Native behavioral tests run only from the explicit fresh-process check mode,
// before NSApplication/GPUI. No test callback or queue captures stack storage.
struct Test {
    enum class Action { Count, Reschedule, Stop } action = Action::Count;
    int calls = 0;
    int depth = 0;
    int max_depth = 0;
    bool ok = true;
    Clock::time_point limit;
    Clock::time_point not_before{};
};
Test test;

bool spin_for(std::chrono::milliseconds duration, CFStringRef mode = kCFRunLoopDefaultMode) {
    const auto until = Clock::now() + duration;
    while (Clock::now() < until) {
        if (Clock::now() >= test.limit) return false;
        CFRunLoopRunInMode(mode, 0.002, true);
    }
    return true;
}

bool await_calls(int count, CFStringRef mode = kCFRunLoopDefaultMode) {
    while (test.calls < count && test.ok) {
        if (Clock::now() >= test.limit) return false;
        CFRunLoopRunInMode(mode, 0.002, true);
    }
    return test.ok && test.calls == count;
}

void test_callback() {
    ++test.calls;
    ++test.depth;
    test.max_depth = std::max(test.max_depth, test.depth);
    test.ok = test.ok && pthread_main_np() && Clock::now() >= test.not_before;
    const auto action = test.action;
    test.action = Test::Action::Count;
    if (action == Test::Action::Reschedule) {
        felinaut_cef_pump_schedule(-1);
        test.ok = spin_for(std::chrono::milliseconds(12)) && test.ok;
    } else if (action == Test::Action::Stop) {
        felinaut_cef_pump_schedule(-1);
        test.ok = felinaut_cef_pump_stop() && test.ok;
        // An in-flight callback prevents restart until its stack has unwound.
        test.ok = !felinaut_cef_pump_start(test_callback) && test.ok;
        felinaut_cef_pump_schedule(-1); // Stopped schedules must be ignored.
        test.ok = spin_for(std::chrono::milliseconds(12)) && test.ok;
    }
    --test.depth;
}

bool run_tests() {
    using namespace std::chrono_literals;
    const auto tracking = reinterpret_cast<CFStringRef>(NSEventTrackingRunLoopMode);
    const auto now = Clock::now();
    if (bounded_deadline(INT64_MIN, now) != now || bounded_deadline(-1, now) != now ||
        bounded_deadline(INT64_MAX, now) != now + std::chrono::milliseconds(max_delay_ms) ||
        bounded_deadline(INT64_MAX, Clock::time_point::max()) != Clock::time_point::max()) return false;
    if (felinaut_cef_pump_start(nullptr) || !felinaut_cef_pump_start(test_callback) ||
        felinaut_cef_pump_start(test_callback)) return false;

    // Coalescing and replacement to a later deadline, not earliest-wins.
    felinaut_cef_pump_schedule(-1);
    felinaut_cef_pump_schedule(45);
    test.not_before = Clock::now() + 90ms;
    felinaut_cef_pump_schedule(90);
    if (test.calls != 0 || !spin_for(25ms) || test.calls != 0 || !await_calls(1) ||
        !spin_for(12ms) || test.calls != 1) return false;

    // Huge positive delays are bounded; the latest negative delay replaces one.
    felinaut_cef_pump_schedule(INT64_MAX);
    test.not_before = {};
    felinaut_cef_pump_schedule(INT64_MIN);
    if (test.calls != 1 || !await_calls(2, tracking)) return false;

    bool worker_ok = false;
    std::thread worker([&worker_ok] {
        worker_ok = !felinaut_cef_pump_start(test_callback) && !felinaut_cef_pump_stop();
        for (int i = 0; i < 1000; ++i) felinaut_cef_pump_schedule(-1);
        test.not_before = Clock::now() + 45ms; // Main is joined, not running the loop.
        felinaut_cef_pump_schedule(45);
    });
    worker.join();
    if (!worker_ok || test.calls != 2 || !spin_for(12ms, tracking) || test.calls != 2 ||
        !await_calls(3, tracking) || !spin_for(12ms, tracking) || test.calls != 3) return false;

    // Replace an already-armed timer, rather than only a queued source request.
    test.not_before = Clock::now() + 80ms;
    felinaut_cef_pump_schedule(80);
    if (!spin_for(8ms)) return false;
    felinaut_cef_pump_schedule(0);
    test.not_before = {};
    if (test.calls != 3 || !await_calls(4)) return false;

    test.action = Test::Action::Reschedule;
    felinaut_cef_pump_schedule(0);
    if (!await_calls(6) || test.max_depth != 1) return false;

    // Stop an armed timer; a new generation must not inherit that deadline.
    felinaut_cef_pump_schedule(20);
    if (!spin_for(5ms) || !felinaut_cef_pump_stop() || !felinaut_cef_pump_start(test_callback) ||
        !spin_for(30ms) || test.calls != 6) return false;

    // Stop with source/GCD work still queued, then restart before draining it.
    felinaut_cef_pump_schedule(-1);
    if (!felinaut_cef_pump_stop() || !felinaut_cef_pump_start(test_callback)) return false;
    test.not_before = Clock::now() + 45ms;
    felinaut_cef_pump_schedule(45);
    if (!spin_for(12ms) || test.calls != 6 || !await_calls(7)) return false;

    test.not_before = {};
    test.action = Test::Action::Stop;
    felinaut_cef_pump_schedule(-1);
    if (!await_calls(8) || !spin_for(30ms) || test.calls != 8 || test.max_depth != 1 || !test.ok) {
        return false;
    }
    // Stop is idempotent. Starting again must be clean and must not auto-pump.
    if (!felinaut_cef_pump_stop() || !felinaut_cef_pump_start(test_callback) ||
        !spin_for(12ms) || test.calls != 8) return false;
    return felinaut_cef_pump_stop();
}
} // namespace

extern "C" bool felinaut_cef_pump_self_test() {
    if (!pthread_main_np() || NSApp) return false;
    {
        auto &state = pump();
        std::lock_guard<std::mutex> lock(state.mutex);
        if (state.active || state.pumping) return false;
    }
    // Prevent accidental nested checks through a native run-loop callback.
    static bool checking = false;
    if (checking) return false;
    checking = true;
    test = Test{};
    test.limit = Clock::now() + std::chrono::milliseconds(1500);
    bool result = false;
    @autoreleasepool {
        try {
            result = run_tests();
        } catch (...) {
            result = false;
        }
        felinaut_cef_pump_stop();
        // Drain stale generation-tagged work without starting a new session.
        result = spin_for(std::chrono::milliseconds(5)) && result;
    }
    checking = false;
    return result && test.ok && test.depth == 0;
}
