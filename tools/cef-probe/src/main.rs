use felinaut_cef_probe::{self as native, Mode};
use gpui::{prelude::*, *};
use std::{cell::Cell, process::ExitCode, rc::Rc, time::Duration};

struct ProbeWindow;
impl Render for ProbeWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_8()
            .bg(rgb(0x0b1026))
            .text_color(rgb(0xf7f6ff))
            .child("Felinaut CEF lifecycle probe — GPUI host only; no web engine loaded")
    }
}

fn fail(message: &str) -> ! {
    eprintln!("FAIL: {message}");
    std::process::exit(1);
}

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let mode = match Mode::parse(&arguments) {
        Ok(mode) => mode,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    if mode == Mode::ExistingApplication {
        if !native::create_conflicting_application() {
            fail("Could not establish the deliberately conflicting application");
        }
        match native::bootstrap() {
            Err(error) if error.contains("already exists") => {
                println!("PASS: existing NSApplication refused without replacement");
                return ExitCode::SUCCESS;
            }
            _ => fail("An existing application singleton was not rejected"),
        }
    }
    if let Err(error) = native::bootstrap() {
        fail(error);
    }
    let passed = Rc::new(Cell::new(false));
    let completed = passed.clone();
    Application::new().run(move |cx| {
        if !native::verify_application() {
            fail("Subclass, inherited GPUI state or original GPUI delegate was lost");
        }
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(900.), px(600.)),
                    cx,
                ))),
                ..Default::default()
            },
            |_, cx| cx.new(|_| ProbeWindow),
        )
        .unwrap_or_else(|_| fail("Could not open the native GPUI probe window"));
        cx.activate(true);
        cx.on_app_quit(|_| async {
            println!("PASS: original GPUI application termination callback preserved");
        })
        .detach();
        cx.spawn(async move |_| {
            Timer::after(Duration::from_secs(12)).await;
            fail("Native lifecycle exceeded its 12-second watchdog");
        })
        .detach();
        cx.spawn(async move |cx| {
            Timer::after(Duration::from_millis(900)).await;
            if !native::verify_event_scope() {
                fail("Nested/exception send-event scope did not restore its state");
            }
            println!("PASS: inherited GPUI application state, delegate and scoped native event dispatch");
            cx.update(|cx| cx.quit()).unwrap();
            for _ in 0..100 {
                if native::close_requested() {
                    break;
                }
                Timer::after(Duration::from_millis(10)).await;
            }
            if !native::close_requested() {
                fail("GPUI quit did not reach the subclass termination barrier");
            }
            Timer::after(Duration::from_millis(200)).await;
            if !native::verify_application() {
                fail("Application/delegate was destroyed before the termination barrier opened");
            }
            completed.set(true);
            println!("PASS: asynchronous termination held until explicit release; no CEF runtime or sandbox claim");
            if !native::finish_termination() {
                fail("Could not release the application termination barrier");
            }
        })
        .detach();
    });
    if passed.get() {
        ExitCode::SUCCESS
    } else {
        fail("Native application exited before lifecycle checks completed");
    }
}
