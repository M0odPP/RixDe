use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, Rgb8Pixel, VecModel};
use std::{cell::RefCell, rc::Rc};

slint::slint! {
    import { AudioView, AudioRow, SystemState } from "../../../ui/common/system.slint";
    import { Theme } from "../../../ui/common/theme.slint";
    export { SkinFrame, SkinRenderer } from "../../../ui/common/frame.slint";
    export { SystemState, AudioRow, Theme }
    export { Palette } from "std-widgets.slint";

    export component AudioPopupProbe inherits Window {
        width: 420px;
        height: 500px;
        background: Theme.menu-bg;
        in-out property <bool> full-mode: false;
        out property <length> view-preferred-height: view.preferred-height;
        view := AudioView {
            width: 100%;
            height: 100%;
            full-mode: root.full-mode;
        }
    }

    export component AudioSettingsProbe inherits Window {
        width: 680px;
        height: 700px;
        background: Theme.menu-bg;
        out property <length> card-height: card.height;
        out property <length> view-height: view.height;
        VerticalLayout {
            alignment: start;
            padding: 16px;
            card := Rectangle {
                preferred-height: card-layout.preferred-height;
                min-height: card-layout.preferred-height;
                vertical-stretch: 0;
                card-layout := VerticalLayout {
                    alignment: start;
                    padding: 16px;
                    spacing: 12px;
                    view := AudioView { full-mode: true; }
                }
            }
        }
    }
}

#[path = "../../../ui/common/skins.rs"]
mod skins;
#[path = "../../../ui/common/bindings.rs"]
mod theme_bindings;

struct PrivatePlatform(Rc<MinimalSoftwareWindow>);
impl Platform for PrivatePlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn render(surface: &MinimalSoftwareWindow) -> Vec<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    surface.request_redraw();
    let size = surface.window().size();
    let mut pixels = vec![Rgb8Pixel::default(); size.width as usize * size.height as usize];
    surface.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, size.width as usize);
    });
    pixels
}

fn sample_rows() -> ModelRc<AudioRow> {
    ModelRc::from(Rc::new(VecModel::from(vec![
        AudioRow {
            app_id: "".into(),
            binary: "".into(),
            key: "sink:1".into(),
            name: "Headphones Stereo".into(),
            icon: Default::default(),
            kind: "sink".into(),
            volume: 65,
            muted: false,
            preferred: true,
        },
        AudioRow {
            app_id: "".into(),
            binary: "".into(),
            key: "sink:2".into(),
            name: "Built-in Speaker".into(),
            icon: Default::default(),
            kind: "sink".into(),
            volume: 90,
            muted: false,
            preferred: false,
        },
        AudioRow {
            app_id: "".into(),
            binary: "".into(),
            key: "source:3".into(),
            name: "USB Microphone".into(),
            icon: Default::default(),
            kind: "source".into(),
            volume: 80,
            muted: false,
            preferred: true,
        },
    ])))
}

#[test]
fn volume_popup_renders_audio_devices_and_settings_full_mode_keeps_content_height() {
    let surface = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PrivatePlatform(surface.clone()))).unwrap();

    let popup = AudioPopupProbe::new().unwrap();
    skins::install(popup.global::<SkinRenderer>(), popup.as_weak());
    theme_bindings::apply(
        popup.global::<Theme>(),
        popup.global::<Palette>(),
        &rix_config::ThemeSettings::default(),
    );
    popup.global::<Theme>().set_font_family("DejaVu Sans".into());
    popup.global::<SystemState>().set_audio(sample_rows());

    let selected_default = Rc::new(RefCell::new(None::<String>));
    let observed = selected_default.clone();
    popup.global::<SystemState>().on_audio_default(move |key| {
        *observed.borrow_mut() = Some(key.to_string());
    });

    popup.show().unwrap();
    surface.set_size(slint::PhysicalSize::new(420, 500));
    let pixels = render(&surface);

    let bg = pixels[499 * 420 + 419];
    let non_bg_in_list = (130..480)
        .flat_map(|y| (20..400).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let p = pixels[y * 420 + x];
            p.r.abs_diff(bg.r) > 4 || p.g.abs_diff(bg.g) > 4 || p.b.abs_diff(bg.b) > 4
        })
        .count();
    assert!(
        non_bg_in_list > 4000,
        "Volume popup device list area is empty (only {non_bg_in_list} non-background pixels below y=130)"
    );

    for y in (220..290).step_by(6) {
        let pos = LogicalPosition::new(355.0, y as f32);
        popup
            .window()
            .dispatch_event(WindowEvent::PointerMoved { position: pos });
        popup.window().dispatch_event(WindowEvent::PointerPressed {
            position: pos,
            button: slint::platform::PointerEventButton::Left,
        });
        popup.window().dispatch_event(WindowEvent::PointerReleased {
            position: pos,
            button: slint::platform::PointerEventButton::Left,
        });
        if selected_default.borrow().is_some() {
            break;
        }
    }
    assert_eq!(
        selected_default.borrow().as_deref(),
        Some("sink:2"),
        "Second audio output card in Volume popup must be visible and clickable"
    );

    let settings = AudioSettingsProbe::new().unwrap();
    skins::install(settings.global::<SkinRenderer>(), settings.as_weak());
    theme_bindings::apply(
        settings.global::<Theme>(),
        settings.global::<Palette>(),
        &rix_config::ThemeSettings::default(),
    );
    settings.global::<SystemState>().set_audio(sample_rows());
    settings.show().unwrap();
    surface.set_size(slint::PhysicalSize::new(680, 700));
    let _ = render(&surface);
    assert!(
        settings.get_view_height() > 300.0,
        "AudioView full-mode must size to its audio devices (got {}px)",
        settings.get_view_height()
    );
    assert!(
        settings.get_card_height() >= settings.get_view_height(),
        "SettingsCard must enclose AudioView in full-mode"
    );
}
