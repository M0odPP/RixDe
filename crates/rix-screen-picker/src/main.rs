slint::include_modules!();

#[path = "../../../ui/common/skins.rs"]
mod skins;
#[path = "../../../ui/common/bindings.rs"]
mod theme_bindings;

use slint::{ComponentHandle, ModelRc, SharedPixelBuffer, VecModel};
use std::io::{BufRead, Write};
use std::path::PathBuf;

fn load_theme() -> rix_config::ThemeSettings {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("rix");
    std::fs::read(config_dir.join("theme.json"))
        .ok()
        .and_then(|v| serde_json::from_slice::<rix_config::ThemeSettings>(&v).ok())
        .filter(|t| t.validate().is_ok())
        .unwrap_or_default()
}

fn resolve_app_icon(app_id: &str, theme_name: &str) -> slint::Image {
    if app_id.is_empty() {
        return slint::Image::default();
    }
    let mut resolver = rix_assets::IconResolver::default();
    if let Some(path) = resolver.resolve(app_id, theme_name, 48, None) {
        if let Ok(img) = slint::Image::load_from_path(&path) {
            return img;
        }
    }
    let lower = app_id.to_lowercase();
    if lower != app_id {
        if let Some(path) = resolver.resolve(&lower, theme_name, 48, None) {
            if let Ok(img) = slint::Image::load_from_path(&path) {
                return img;
            }
        }
    }
    slint::Image::default()
}

struct RawTarget {
    line: String,
    title: String,
    app_id: String,
    details: String,
    is_window: bool,
}

fn parse_target(line: &str) -> RawTarget {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("Monitor: ") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let name = parts.first().copied().unwrap_or(rest);
        let details = parts.get(1..).map(|p| p.join(" ")).unwrap_or_default();
        RawTarget {
            line: trimmed.to_string(),
            title: name.to_string(),
            app_id: String::new(),
            details,
            is_window: false,
        }
    } else if let Some(rest) = trimmed.strip_prefix("Window: ") {
        let (title, app_id) = if let Some(open_idx) = rest.rfind(" (") {
            if rest.ends_with(')') {
                let title = rest[..open_idx].trim();
                let app = &rest[open_idx + 2..rest.len() - 1];
                (title.to_string(), app.to_string())
            } else {
                (rest.to_string(), String::new())
            }
        } else {
            (rest.to_string(), String::new())
        };
        let details = if !app_id.is_empty() {
            app_id.clone()
        } else {
            String::new()
        };
        RawTarget {
            line: trimmed.to_string(),
            title,
            app_id,
            details,
            is_window: true,
        }
    } else {
        RawTarget {
            line: trimmed.to_string(),
            title: trimmed.to_string(),
            app_id: String::new(),
            details: String::new(),
            is_window: true,
        }
    }
}

fn chunk_rows(items: Vec<PickerItemData>) -> ModelRc<PickerRowData> {
    let mut rows = Vec::new();
    for chunk in items.chunks(3) {
        rows.push(PickerRowData {
            items: ModelRc::new(VecModel::from(chunk.to_vec())),
        });
    }
    ModelRc::new(VecModel::from(rows))
}

fn main() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let lines: Vec<String> = stdin
        .lock()
        .lines()
        .map_while(Result::ok)
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if lines.is_empty() {
        std::process::exit(1);
    }

    let parsed: Vec<RawTarget> = lines.iter().map(|l| parse_target(l)).collect();
    let theme_settings = load_theme();
    let icon_theme = if theme_settings.icon_theme.is_empty() {
        "hicolor"
    } else {
        theme_settings.icon_theme.as_str()
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok();

    let (windows_info, _outputs_info) = rt
        .as_ref()
        .map(|rt| {
            rt.block_on(async {
                let Ok(mut client) = rix_ipc::IpcClient::connect().await else {
                    return (Vec::new(), Vec::new());
                };
                let windows = match client.send(rix_ipc::IpcRequest::ListWindows).await {
                    Ok(rix_ipc::IpcResponse::Windows(w)) => w,
                    _ => Vec::new(),
                };
                let outputs = match client.send(rix_ipc::IpcRequest::GetOutputs).await {
                    Ok(rix_ipc::IpcResponse::Outputs(o)) => o.outputs,
                    _ => Vec::new(),
                };
                (windows, outputs)
            })
        })
        .unwrap_or_default();

    let mut screen_items = Vec::new();
    let mut window_items = Vec::new();

    for target in parsed {
        if !target.is_window {
            screen_items.push(PickerItemData {
                raw_target: target.line.into(),
                title: target.title.into(),
                app_id: String::new().into(),
                details: target.details.into(),
                is_window: false,
                icon: slint::Image::default(),
                preview: slint::Image::default(),
                has_preview: false,
            });
        } else {
            let matched_id = windows_info
                .iter()
                .find(|w| {
                    (!target.app_id.is_empty()
                        && w.app_id.eq_ignore_ascii_case(&target.app_id))
                        || (!target.title.is_empty()
                            && (w.title == target.title || w.title.contains(&target.title)))
                })
                .map(|w| w.id);

            let icon = resolve_app_icon(&target.app_id, icon_theme);

            let (preview_img, has_preview) = if let (Some(rt), Some(id)) = (rt.as_ref(), matched_id) {
                let preview_res = rt.block_on(async {
                    let Ok(mut client) = rix_ipc::IpcClient::connect().await else {
                        return None;
                    };
                    match client
                        .send(rix_ipc::IpcRequest::GetWindowPreview {
                            id,
                            width: 320,
                            height: 180,
                        })
                        .await
                    {
                        Ok(rix_ipc::IpcResponse::WindowPreview {
                            width,
                            height,
                            rgba,
                            ..
                        }) => {
                            if rgba.len() == (width * height * 4) as usize {
                                Some((width, height, rgba))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    }
                });

                if let Some((w, h, rgba)) = preview_res {
                    let pixel_buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&rgba, w, h);
                    (slint::Image::from_rgba8_premultiplied(pixel_buffer), true)
                } else {
                    (slint::Image::default(), false)
                }
            } else {
                (slint::Image::default(), false)
            };

            window_items.push(PickerItemData {
                raw_target: target.line.into(),
                title: target.title.into(),
                app_id: target.app_id.into(),
                details: target.details.into(),
                is_window: true,
                icon,
                preview: preview_img,
                has_preview,
            });
        }
    }

    let window = PickerWindow::new()?;
    window.window().set_size(slint::LogicalSize::new(
        window.get_initial_width(),
        window.get_initial_height(),
    ));

    skins::install(window.global::<SkinRenderer>(), window.as_weak());
    theme_bindings::apply_for_app(
        window.global::<Theme>(),
        window.global::<Palette>(),
        &theme_settings,
    );

    let screen_count = screen_items.len() as i32;
    let window_count = window_items.len() as i32;

    let initial_tab = if screen_count > 0 { 0 } else { 1 };
    let initial_selection = if screen_count > 0 {
        screen_items[0].raw_target.clone()
    } else if window_count > 0 {
        window_items[0].raw_target.clone()
    } else {
        slint::SharedString::default()
    };

    window.set_active_tab(initial_tab);
    window.set_screen_count(screen_count);
    window.set_window_count(window_count);
    window.set_selected_target(initial_selection);
    window.set_screen_rows(chunk_rows(screen_items));
    window.set_window_rows(chunk_rows(window_items));

    window.on_confirmed(move |target| {
        println!("{}", target);
        let _ = std::io::stdout().flush();
        std::process::exit(0);
    });

    window.on_cancelled(move || {
        std::process::exit(1);
    });

    window.run()?;
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    #[test]
    fn parses_monitor_and_window_targets() {
        let monitor = parse_target("Monitor: HDMI-A-1 1920x1080@60Hz");
        assert_eq!(monitor.line, "Monitor: HDMI-A-1 1920x1080@60Hz");
        assert_eq!(monitor.title, "HDMI-A-1");
        assert_eq!(monitor.details, "1920x1080@60Hz");
        assert!(!monitor.is_window);

        let window = parse_target("Window: Minecraft 1.20.1 (minecraft)");
        assert_eq!(window.line, "Window: Minecraft 1.20.1 (minecraft)");
        assert_eq!(window.title, "Minecraft 1.20.1");
        assert_eq!(window.app_id, "minecraft");
        assert_eq!(window.details, "minecraft");
        assert!(window.is_window);

        let fallback = parse_target("Window: Discord");
        assert_eq!(fallback.title, "Discord");
        assert_eq!(fallback.app_id, "");
        assert!(fallback.is_window);
    }

    #[test]
    fn chunks_rows_correctly() {
        let items = vec![
            PickerItemData {
                raw_target: "t1".into(),
                title: "1".into(),
                app_id: "".into(),
                details: "".into(),
                is_window: false,
                icon: slint::Image::default(),
                preview: slint::Image::default(),
                has_preview: false,
            },
            PickerItemData {
                raw_target: "t2".into(),
                title: "2".into(),
                app_id: "".into(),
                details: "".into(),
                is_window: false,
                icon: slint::Image::default(),
                preview: slint::Image::default(),
                has_preview: false,
            },
            PickerItemData {
                raw_target: "t3".into(),
                title: "3".into(),
                app_id: "".into(),
                details: "".into(),
                is_window: false,
                icon: slint::Image::default(),
                preview: slint::Image::default(),
                has_preview: false,
            },
            PickerItemData {
                raw_target: "t4".into(),
                title: "4".into(),
                app_id: "".into(),
                details: "".into(),
                is_window: false,
                icon: slint::Image::default(),
                preview: slint::Image::default(),
                has_preview: false,
            },
        ];
        let rows = chunk_rows(items);
        assert_eq!(rows.row_count(), 2);
        assert_eq!(rows.row_data(0).unwrap().items.row_count(), 3);
        assert_eq!(rows.row_data(1).unwrap().items.row_count(), 1);
    }
}
