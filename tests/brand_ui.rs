//! Opt-in native UI test: FELINAUT_NATIVE_TEST=1 cargo test --test brand_ui

#[allow(dead_code, unused_imports)]
#[path = "../src/about.rs"]
mod about;
#[allow(dead_code, unused_imports)]
#[path = "../src/appearance.rs"]
mod appearance;
#[allow(dead_code, unused_imports)]
#[path = "../src/art.rs"]
mod art;
#[allow(dead_code, unused_imports)]
#[path = "../src/brand.rs"]
mod brand;
#[allow(dead_code, unused_imports)]
#[path = "../src/theme.rs"]
mod theme;

use gpui::{prelude::*, *};
use std::{cell::Cell, rc::Rc, time::Duration};
use theme::{Theme, ThemeSettings};

pub const DISPLAY_VERSION: &str = "0.1 beta";

struct Parent;
impl Render for Parent {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .child(art::backdrop(theme::palette(cx), 0.).absolute().inset_0())
            .child(
                div()
                    .relative()
                    .w(px(280.))
                    .h(px(210.))
                    .child(brand::mascot().absolute().inset_0().size_full())
                    .child(brand::exploration_box().absolute().inset_0().size_full()),
            )
    }
}

async fn frame() {
    Timer::after(Duration::from_millis(150)).await;
}

fn other_window(parent: WindowHandle<Parent>, cx: &App) -> AnyWindowHandle {
    let others: Vec<_> = cx
        .windows()
        .into_iter()
        .filter(|handle| handle.window_id() != parent.window_id())
        .collect();
    assert_eq!(others.len(), 1, "expected exactly one auxiliary window");
    others[0]
}

fn main() {
    if std::env::var_os("FELINAUT_NATIVE_TEST")
        .or_else(|| std::env::var_os("FELIFOX_NATIVE_TEST"))
        .as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        println!("Native brand UI test skipped; set FELINAUT_NATIVE_TEST=1 to run.");
        return;
    }

    let passed = Rc::new(Cell::new(false));
    let result = passed.clone();
    Application::new().with_assets(brand::BrandAssets).run(move |cx| {
        cx.set_global(ThemeSettings { theme: Theme::default(), animate: true, ..Default::default() });
        let parent = cx.open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None, size(px(800.), px(600.)), cx,
            ))),
            titlebar: None,
            ..Default::default()
        }, |_, cx| cx.new(|_| Parent)).expect("native parent window");
        cx.activate(true);

        cx.spawn(async move |cx| {
            Timer::after(Duration::from_secs(10)).await;
            cx.update(|cx| {
                eprintln!("Native brand UI test timed out");
                cx.quit();
            }).ok();
        }).detach();

        let images = [brand::MASCOT, brand::SYMBOL, brand::BOX].map(|path| {
            Box::pin(ImageAssetLoader::load(Resource::Embedded(path.into()), cx))
                            as std::pin::Pin<Box<dyn std::future::Future<Output = <ImageAssetLoader as Asset>::Output> + Send + 'static>>
        });
        cx.spawn(async move |cx| {
            for image in images {
                let image = image.await.expect("decode embedded SVG through GPUI");
                assert!(image.frame_count() > 0);
                // GPUI 0.2.2's render_size is private; size exposes raster dimensions.
                let size = image.size(0);
                assert!(size.width > DevicePixels(0) && size.height > DevicePixels(0));
                assert!(!image.as_bytes(0).expect("decoded raster bytes").is_empty());
            }
            frame().await;

            // Exercise the same handlers as the rows, not mouse hit-testing.
            for expected in Theme::ALL {
                parent.update(cx, |_, window, cx| appearance::open(window, cx)).unwrap();
                frame().await;
                cx.update(|cx| {
                    other_window(parent, cx);
                    appearance::select_theme_for_test(expected, cx);
                }).unwrap();
                frame().await;
                cx.update(|cx| {
                    assert_eq!(cx.global::<ThemeSettings>().theme, expected, "theme selection handler");
                    assert_eq!(cx.windows().len(), 1, "theme selection dismisses popup");
                    assert!(parent.read(cx).is_ok(), "parent survives selection");
                }).unwrap();
            }

            parent.update(cx, |_, window, cx| appearance::open(window, cx)).unwrap();
            frame().await;
            let popup = cx.update(|cx| other_window(parent, cx)).unwrap();
            cx.update(appearance::toggle_motion_for_test).unwrap();
            frame().await;
            cx.update(|cx| {
                assert!(!cx.global::<ThemeSettings>().animate, "Animate artwork handler");
                assert_eq!(cx.windows().len(), 2, "animation toggle keeps popup open");
            }).unwrap();
            popup.update(cx, |_, window, cx| {
                window.dispatch_action(Box::new(appearance::CloseAppearance), cx);
            }).unwrap();
            frame().await;
            cx.update(|cx| {
                assert_eq!(cx.windows().len(), 1, "CloseAppearance dismisses popup");
                about::open(cx);
            }).unwrap();
            frame().await;
            let about = cx.update(|cx| {
                let handle = other_window(parent, cx);
                about::open(cx);
                assert_eq!(cx.windows().len(), 2, "About is a singleton");
                assert!(other_window(parent, cx) == handle, "About reuses its window");
                cx.update_global::<ThemeSettings, _>(|settings, _| settings.theme = Theme::MidnightParty);
                handle
            }).unwrap();
            frame().await;
            let paused = cx.update(about::animation_phase_for_test).unwrap();
            frame().await;
            assert_eq!(paused, cx.update(about::animation_phase_for_test).unwrap(), "disabled artwork must not advance");
            let reduced = cx.update(|cx| {
                cx.update_global::<ThemeSettings, _>(|settings, _| settings.animate = true);
                theme::system_reduced_motion()
            }).unwrap();
            about.update(cx, |_, window, _| window.activate_window()).unwrap();
            Timer::after(Duration::from_millis(700)).await;
            let advanced = cx.update(about::animation_phase_for_test).unwrap();
            if reduced {
                assert_eq!(paused, advanced, "macOS Reduce Motion must be respected");
            } else {
                assert!(advanced - paused > 0.1, "the actual native artwork timer must visibly advance when enabled: {paused} -> {advanced}");
            }
            cx.update(|cx| cx.update_global::<ThemeSettings, _>(|settings, _| settings.animate = false)).unwrap();
            frame().await;
            let stopped = cx.update(about::animation_phase_for_test).unwrap();
            frame().await;
            assert_eq!(stopped, cx.update(about::animation_phase_for_test).unwrap(), "disabling animation stops the timer");
            about.update(cx, |_, window, cx| {
                window.dispatch_action(Box::new(about::CloseAbout), cx);
            }).unwrap();
            frame().await;
            cx.update(|cx| {
                assert_eq!(cx.windows().len(), 1, "CloseAbout removes only About");
                assert!(parent.read(cx).is_ok(), "parent survives About close");
                result.set(true);
                println!("PASS: SVG rasterization, six-theme art rendering, theme selection, actual animation timer start/stop and Reduce Motion, About lifecycle");
                parent.update(cx, |_, window, _| window.remove_window()).unwrap();
                cx.quit();
            }).unwrap();
        }).detach();
    });
    assert!(passed.get(), "native brand UI test did not finish");
}
