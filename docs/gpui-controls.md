# GPUI control coverage for Felinaut

**Status:** source-level inventory and porting gate, reviewed on 12 November 2026. This is not a native keyboard/accessibility test report or a claim that Windows/Linux is supported. The baseline is the published **GPUI 0.2.2** crate pinned in [`Cargo.toml`](../Cargo.toml), not current Zed source with a similar manifest version.

## Conclusion

GPUI has the rendering, layout, text-input, event, and focus primitives needed to construct Felinaut's current chrome. **It does not ship a complete standard widget set.** Felinaut already supplies much of the control behavior itself.

That makes keeping GPUI plausible, but “we can draw it” is not a sufficient acceptance criterion. We need consistent keyboard behavior, focus, IME, accessible semantics/actions, popup placement, scaling, and native integration on each target platform. The pinned crate's accessibility infrastructure is a significant unresolved limitation, including for the current macOS chrome—not just a future Linux issue.

## Three layers that must not be confused

1. **Published `gpui 0.2.2`:** core elements such as `div`, text, images, canvas, lists, scrolling, input/focus machinery, and platform services.
2. **Zed's higher-level controls:** separate workspace crates such as `ui` and `ui_input`, built on Zed's evolving GPUI and other editor services. These are not automatically included by adding the published GPUI crate.
3. **Felinaut's controls:** our address input, toolbar, sidebar, selectors, audio slider, history suggestions, download list, and auxiliary windows, composed from the primitives.

Current Zed `ui` code has different dependency/API assumptions and declares **GPL-3.0-or-later**. GPUI itself has a different license. Do not copy or import editor components under the assumption that they are all covered by GPUI's license or are compatible with our pin. Review the exact component's license, dependencies, and API compatibility before considering reuse.

## Inventory

“Present” below means source exists, not that every interaction is accessible or that it has been tested on another OS.

| Control / service | What pinned GPUI supplies | Felinaut today | Work to validate or supply |
| --- | --- | --- | --- |
| Labels, icons, artwork, layout | Text shaping/painting, SVG/images, canvas, flex/grid-style layout and styling. | Used throughout the UI. | Portable font choices, scaling, contrast, truncation, and semantic labels. Some views currently name `.AppleSystemUIFont`. |
| Buttons and icon buttons | Stateful elements, click/hover/focus handling and styling; no ready-made exported `Button` widget. Focused click handlers have Enter/Space activation support. | Custom toolbar/action buttons in [`src/main.rs`](../src/main.rs), [`src/volume.rs`](../src/volume.rs), and [`src/download_panel.rs`](../src/download_panel.rs). | Deliberate focusability/tab order, visible focus, disabled-state behavior, accessible names/roles/actions, and consistent hit targets. A click handler alone does not establish these. |
| Address/search input | `EntityInputHandler`, shaped text, composition/selection interfaces, clipboard and native input plumbing; an input example rather than a complete exported field widget. | Custom [`src/input.rs`](../src/input.rs), with editing, selection, marked text, and autocomplete presentation. | Cross-platform editing shortcuts, real IMEs, candidate placement, text selection/caret accessibility, and focus handoff to the page. |
| Autocomplete / suggestion list | Layout, list and popup-window primitives; no finished browser combobox. | [`src/suggestions.rs`](../src/suggestions.rs) plus address/browser state; a non-focusing popup keeps typing in the address field. | Combobox/list selection semantics, announcement of suggestions, monitor-edge placement, scaling, and preserving focus above embedded content. Current resize handling includes an AppKit-specific path. |
| Checkboxes, toggles, radio choices | Build from interaction/layout primitives; no exported standard checkbox/radio/switch control. | Custom theme/address-display choices and animation checkbox in [`src/appearance.rs`](../src/appearance.rs). | Per-option keyboard navigation, checked/selected/disabled states, focus rings, and accessible grouping/state/actions. Existing mouse selection is not proof of complete keyboard support. |
| Slider and stepper buttons | Pointer/scroll/key events and painting; no ready-made exported `Slider`. | Custom audio track/knob, step buttons, reset, mute, and presets in [`src/volume.rs`](../src/volume.rs). | Keyboard endpoints/steps, drag cancellation and capture behavior, accessible range/value/increment actions, and input across scale factors. Existing panel shortcuts are useful but do not replace slider semantics. |
| Tabs, pin tiles, split panes, resizers | Element identity, state, drag/drop, layout, pointer input and focus primitives. | Custom sidebar, reordering, pinned tiles, split views, and sidebar resize in [`src/main.rs`](../src/main.rs). | Tab/panel relationships, keyboard equivalents for drag actions, selected-state announcements, focus after close/reorder, and platform drag behavior. |
| Scrollable lists and visible scrollbars | Overflow scrolling, scroll handles, variable-height `list`, and `uniform_list` virtualization. | Sidebar lists and bounded downloads/history lists; download rows are currently rendered eagerly. | Selection/navigation models, scroll-to-focused-item, discoverable/operable scrollbars, and virtualization where actual scale requires it. Scrolling support is not a complete list or scrollbar widget. |
| Trees and data tables | Tree and data-table examples, not exported complete tree/table controls. | Live-folder groups are custom; no general table/tree component is assumed. | If future settings/data browsers need them: selection, expansion, columns/sorting, virtualization, keyboard navigation, and semantic hierarchy/table metadata. Do not build them preemptively. |
| Progress indicators and empty/error states | Drawing, layout, animation, text and events. | Custom download progress/empty states, load failures, and retry controls. | Accessible progress/status changes without excessive announcements, unknown-size handling, keyboard recovery, and reduced-motion behavior. |
| Tooltips and contextual help | Tooltip support exists in core GPUI. | Some controls use status-bar hover descriptions rather than a shared tooltip component. | Keyboard/focus access to help, touch/assistive alternatives, and no credential/private-URL leakage in help text. |
| Popovers, dropdowns, context menus | Anchored/deferred elements, tooltip/lifecycle helpers, window primitives; not a complete general-purpose exported popover/menu control. | Appearance, audio, suggestions and downloads use separate GPUI popup windows. Sidebar menus are application controls; web-link menus have a separate page-side implementation. | Dismissal, focus restoration, keyboard selection, owner-window lifetime, and correct layering over the webview. In-window deferred painting alone cannot solve native child-view stacking. |
| Application menus | `Menu`, `MenuItem`, and menu registration; native macOS menus are implemented. | Browser menus/shortcuts registered in [`src/main.rs`](../src/main.rs). | Actual OS backend support, platform conventions, command enablement, and shortcuts while content owns focus. An API's presence is not feature parity on every platform. |
| Dialogs and file selection | Prompt and open/save-path APIs; native macOS dialog implementations exist. | About is a custom window. Downloads currently use a fixed destination, not a Save As chooser. | OS-specific implementation/dependencies, cancellation, async lifetime, parent ownership, and GTK-free Linux requirements if future chooser/permission UI is added. |
| Web-content surface | Core rendering/surface primitives are not a browser engine or a portable embedded-browser widget. | Wry child `WKWebView`, outside GPUI's normal painted element tree. | Backend-specific presentation/input, popup stacking, native accessibility-tree integration, and clean focus boundaries; see the [porting plan](porting-plan.md). |

Website HTML controls are rendered and handled by the web engine. Adding a GPUI checkbox or text field does not replace website controls, provide a WPE input bridge, or solve content accessibility automatically.

## Keyboard and accessibility are separate gates

### Focus and keyboard support

Published GPUI has `FocusHandle`, focus tracking, tab-stop/index/group APIs, and `Window::focus_next()` / `focus_prev()`. Its tab-stop example explicitly binds Tab/Shift-Tab traversal, and focus handles do not become tab stops by default.

We therefore need to audit each Felinaut control, not just confirm the framework methods exist. A popup's single focus handle plus Escape/arrow shortcuts may make some actions usable, without making every checkbox, row action, or toolbar button reachable. Verify action activation, traversal, focus restoration, and current-focus visibility in the real app.

### Semantic accessibility on the current pin

Published GPUI 0.2.2 does **not** include current upstream's semantic accessibility-tree/AccessKit interface for GPU-drawn controls. Its `Element` contract exposes layout, prepaint, and paint without the newer accessibility-node hooks. Native menus/dialogs and the embedded native webview are separate surfaces; this is not a blanket assertion that every part of the application is inaccessible.

IME support, keyboard focus, contrast, and Reduce Motion are valuable, but none substitutes for screen-reader roles, names, values, selected/checked states, hierarchy, and accessible actions.

Current upstream GPUI includes AccessKit-based infrastructure. That does not automatically make our custom controls accessible, and importing the newer components does not guarantee complete semantic coverage either. A deliberate GPUI update/backport or another reviewed integration would be required to close the framework gap. Evaluate it as its own compatibility change, with macOS regressions and real assistive-technology tests—not as an incidental dependency bump in the engine port.

**Release gate:** resolve and validate an accessible chrome/content focus path before calling a new desktop port generally usable. Use VoiceOver on macOS, NVDA on Windows, and Orca on Linux where applicable; record actual limitations and do not infer support from screenshots or successful typing tests.

## Reuse strategy

1. Keep working controls and their pure models; do not replace the UI or add a widget framework merely to get a larger component catalog.
2. Identify shared behavior only where existing controls repeat it: action buttons, choice rows, popup dismissal/focus restoration, list selection, and slider interaction. Small internal helpers may be enough; a complete new design system is not the goal.
3. Evaluate an accessibility-capable GPUI baseline before standardizing wrappers around APIs that will need replacement.
4. Consider external component libraries only after checking the exact GPUI revision, license, dependencies, maintenance, keyboard/accessibility behavior, and actual required controls. No library was selected, added, or validated by this inventory.
5. Keep platform services and web-engine adapters separate from painted controls. A control library cannot solve a missing download backend, WebView2 runtime, WPE renderer, or OS permissions integration.

## Acceptance checklist before the engine port

Create a small **control-gallery smoke harness**, using the actual controls/behaviors rather than screenshots or source-string assertions. Cover a button, input, checkbox/choice group, slider, list, scrolling, popup, and native dialog where available. This is proposed work, not an existing test harness.

- [ ] Map every current control to its focusable elements, actions, state transitions, and accessible semantics; mark missing behavior explicitly.
- [ ] Choose and validate the GPUI/accessibility baseline; account for API changes without changing persisted user data.
- [ ] Unit-test shared interaction/state logic, including disabled/empty states, boundaries, cancellation, stale selection, Unicode conversions, and popup-owner destruction.
- [ ] Test real keyboard/IME, focus order, Enter/Space activation, Escape dismissal, slider limits, and restoration after a popup or native dialog closes.
- [ ] Inspect semantic roles/names/states and execute accessible actions using real platform accessibility tools. Include input selection/caret behavior and the transition into web content.
- [ ] Verify popups above an actual embedded page, scrolling/dragging, fractional scaling, small displays, high-contrast/system settings where supported, and reduced motion.
- [ ] Run on native macOS, Windows, Wayland and X11 environments before asserting coverage. A shell-only harness cannot establish webview layering or content focus behavior.
- [ ] Attach real screenshots/video to visual-change PRs and keep captures free of personal browsing/account data.

This inventory supports keeping GPUI, **conditional on closing control semantics and platform-integration gaps**. It does not establish that switching away from GTK will be faster or that GPUI already has every finished control we need.

## Sources

Versioned published baseline:

- [GPUI 0.2.2 public API](https://docs.rs/gpui/0.2.2/gpui/), [`Element` contract](https://docs.rs/gpui/0.2.2/gpui/trait.Element.html), and [core elements](https://docs.rs/crate/gpui/0.2.2/source/src/elements/).
- [Input example](https://docs.rs/crate/gpui/0.2.2/source/examples/input.rs), [tab-stop example](https://docs.rs/crate/gpui/0.2.2/source/examples/tab_stop.rs), [data-table example](https://docs.rs/crate/gpui/0.2.2/source/examples/data_table.rs), and [tree example](https://docs.rs/crate/gpui/0.2.2/source/examples/tree.rs).
- [Published macOS window/input implementation](https://docs.rs/crate/gpui/0.2.2/source/src/platform/mac/window.rs) and [platform services](https://docs.rs/crate/gpui/0.2.2/source/src/platform/mac/platform.rs).

Newer upstream reference, inspected at Zed revision `498af674abe1f054869a4d77b5a96ed8a3f12008` (not a dependency update):

- [GPUI accessibility guide](https://github.com/zed-industries/zed/blob/498af674abe1f054869a4d77b5a96ed8a3f12008/crates/gpui/src/_accessibility.rs).
- [Zed `ui` component exports](https://github.com/zed-industries/zed/blob/498af674abe1f054869a4d77b5a96ed8a3f12008/crates/ui/src/components.rs) and [manifest/license](https://github.com/zed-industries/zed/blob/498af674abe1f054869a4d77b5a96ed8a3f12008/crates/ui/Cargo.toml).
- [Zed `ui_input` field and editor-factory dependency](https://github.com/zed-industries/zed/blob/498af674abe1f054869a4d77b5a96ed8a3f12008/crates/ui_input/src/input_field.rs).
