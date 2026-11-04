use crate::{DisplayItem, OutputPreferences, SettingsWindow};
use rix_config::OutputProfile;
use rix_ipc::{IpcClient, IpcRequest, IpcResponse, OutputInfo, OutputState};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Model {
    outputs: Vec<OutputInfo>,
    pending: Option<u64>,
    polling: bool,
}

fn strings(values: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}

fn resolutions(output: &OutputInfo) -> Vec<(u32, u32)> {
    let mut sizes = Vec::new();
    for mode in &output.modes {
        let size = (mode.width, mode.height);
        if !sizes.contains(&size) {
            sizes.push(size);
        }
    }
    sizes
}

fn selected_modes(window: &SettingsWindow, model: &Model) -> Vec<rix_config::OutputMode> {
    let ui = window.global::<OutputPreferences>();
    let Some(output) = model.outputs.get(ui.get_selected_output() as usize) else {
        return vec![];
    };
    let Some(size) = resolutions(output)
        .get(ui.get_selected_resolution() as usize)
        .copied()
    else {
        return vec![];
    };
    output
        .modes
        .iter()
        .copied()
        .filter(|m| (m.width, m.height) == size)
        .collect()
}

fn select_resolution(window: &SettingsWindow, model: &Model) {
    let ui = window.global::<OutputPreferences>();
    let modes = selected_modes(window, model);
    let current = model
        .outputs
        .get(ui.get_selected_output() as usize)
        .map(|o| o.current.mode);
    ui.set_rates(strings(modes.iter().map(|m| {
        let hz = format!("{:.3}", m.refresh_millihz as f64 / 1000.);
        format!(
            "{} {}",
            hz.trim_end_matches('0')
                .trim_end_matches('.')
                .replace('.', ","),
            crate::i18n::tr("Hz")
        )
    })));
    ui.set_selected_rate(modes.iter().position(|m| Some(*m) == current).unwrap_or(0) as i32);
}

fn update_display_items(window: &SettingsWindow, model: &Model) {
    let ui = window.global::<OutputPreferences>();
    let items: Vec<DisplayItem> = model
        .outputs
        .iter()
        .enumerate()
        .map(|(idx, o)| {
            let hz = ((o.current.mode.refresh_millihz + 500) / 1000).to_string();
            let mode_text = format!(
                "{}×{} @ {}Hz",
                o.current.mode.width, o.current.mode.height, hz
            );
            DisplayItem {
                id: idx as i32,
                name: o.name.clone().into(),
                description: o.description.clone().into(),
                mode_text: mode_text.into(),
                is_primary: o.current.primary,
                is_mirrored: o.current.mirror_of.is_some(),
                mirror_name: o.current.mirror_of.clone().unwrap_or_default().into(),
            }
        })
        .collect();
    ui.set_display_items(ModelRc::new(VecModel::from(items)));
}

fn select_output(window: &SettingsWindow, model: &Model) {
    let ui = window.global::<OutputPreferences>();
    let Some(output) = model.outputs.get(ui.get_selected_output() as usize) else {
        ui.set_resolutions(strings([]));
        ui.set_rates(strings([]));
        return;
    };
    let sizes = resolutions(output);
    ui.set_resolutions(strings(sizes.iter().map(|&(w, h)| {
        let recommended = output
            .preferred
            .is_some_and(|m| (m.width, m.height) == (w, h));
        format!(
            "{w} × {h}{}",
            if recommended {
                crate::i18n::tr(" · recommended")
            } else {
                SharedString::default()
            }
        )
    })));
    ui.set_selected_resolution(
        sizes
            .iter()
            .position(|&s| s == (output.current.mode.width, output.current.mode.height))
            .unwrap_or(0) as i32,
    );
    ui.set_is_primary(output.current.primary);

    let mut mirror_options = vec![crate::i18n::tr("Extend desktop").to_string()];
    let mut mirror_names = vec![None];
    for other in &model.outputs {
        if other.name != output.name {
            mirror_options.push(format!("{} {}", crate::i18n::tr("Mirror"), other.name));
            mirror_names.push(Some(other.name.clone()));
        }
    }
    ui.set_mirror_options(strings(mirror_options));
    let mirror_idx = output
        .current
        .mirror_of
        .as_ref()
        .and_then(|m| {
            mirror_names
                .iter()
                .position(|name| name.as_ref() == Some(m))
        })
        .unwrap_or(0);
    ui.set_selected_mirror_index(mirror_idx as i32);

    ui.set_flipped(output.current.flipped);
    ui.set_scale_percent(output.current.scale_percent as f32);
    ui.set_rotation(i32::from(output.current.rotation) / 90);
    ui.set_automatic_position(
        output.current.position.is_none() && output.current.mirror_of.is_none(),
    );
    ui.set_position_x(output.current.position.map_or(0, |p| p.x));
    ui.set_position_y(output.current.position.map_or(0, |p| p.y));
    select_resolution(window, model);
}

fn receive(window: &SettingsWindow, model: &mut Model, state: OutputState) {
    let ui = window.global::<OutputPreferences>();
    ui.set_ready(true);
    ui.set_native(state.native);
    model.pending = state.pending.as_ref().map(|p| p.id);
    ui.set_pending(state.pending.is_some());
    ui.set_confirm_ready(state.pending.as_ref().is_some_and(|p| p.ready));
    ui.set_remaining(state.pending.as_ref().map_or(0, |p| p.remaining_seconds) as i32);
    ui.set_pending_output(state.pending.map_or_else(String::new, |p| p.output).into());
    if model.outputs != state.outputs {
        let selected = model
            .outputs
            .get(ui.get_selected_output() as usize)
            .map(|o| o.name.clone());
        model.outputs = state.outputs;
        update_display_items(window, model);
        crate::panel_editor::set_outputs(
            window,
            &model
                .outputs
                .iter()
                .map(|o| o.name.clone())
                .collect::<Vec<_>>(),
        );
        ui.set_outputs(strings(model.outputs.iter().map(|o| {
            if o.description.is_empty() {
                o.name.clone()
            } else {
                o.description.clone()
            }
        })));
        ui.set_selected_output(
            model
                .outputs
                .iter()
                .position(|o| Some(&o.name) == selected.as_ref())
                .unwrap_or(0) as i32,
        );
        select_output(window, model);
    }
}

pub fn install(window: &SettingsWindow) -> slint::Timer {
    let model = Rc::new(RefCell::new(Model::default()));
    let (tx, mut requests) = tokio::sync::mpsc::channel::<IpcRequest>(4);
    let (results, rx) = std::sync::mpsc::channel();
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let poll = request == IpcRequest::GetOutputs;
            let result: Result<OutputState, String> = async {
                let mut client = IpcClient::connect().await.map_err(|e| e.to_string())?;
                let response = client.send(request).await.map_err(|e| e.to_string())?;
                if let IpcResponse::Outputs(state) = response {
                    Ok(state)
                } else if let IpcResponse::Outputs(state) = client
                    .send(IpcRequest::GetOutputs)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    Ok(state)
                } else {
                    Err(crate::i18n::tr("The compositor returned no display list").to_string())
                }
            }
            .await;
            if results.send((poll, result)).is_err() {
                break;
            }
        }
    });

    let ui = window.global::<OutputPreferences>();
    let weak = window.as_weak();
    let data = model.clone();
    ui.on_select_output(move |idx| {
        if let Some(w) = weak.upgrade() {
            let pref = w.global::<OutputPreferences>();
            if idx >= 0 && (idx as usize) < data.borrow().outputs.len() {
                pref.set_selected_output(idx);
                select_output(&w, &data.borrow());
            }
        }
    });

    let weak = window.as_weak();
    let data = model.clone();
    ui.on_select_resolution(move || {
        if let Some(w) = weak.upgrade() {
            select_resolution(&w, &data.borrow());
        }
    });

    let weak = window.as_weak();
    ui.on_select_mirror(move |idx| {
        if let Some(w) = weak.upgrade() {
            let pref = w.global::<OutputPreferences>();
            pref.set_selected_mirror_index(idx);
            if idx > 0 {
                pref.set_is_primary(false);
                pref.set_automatic_position(false);
            }
        }
    });

    let weak = window.as_weak();
    ui.on_toggle_primary(move || {
        if let Some(w) = weak.upgrade() {
            let pref = w.global::<OutputPreferences>();
            if pref.get_is_primary() {
                pref.set_selected_mirror_index(0);
            }
        }
    });

    let weak = window.as_weak();
    let data = model.clone();
    ui.on_position_left_of(move || {
        if let Some(w) = weak.upgrade() {
            let pref = w.global::<OutputPreferences>();
            let d = data.borrow();
            let cur_idx = pref.get_selected_output() as usize;
            if let Some(other) = d
                .outputs
                .iter()
                .enumerate()
                .find(|(i, _)| *i != cur_idx)
                .map(|(_, o)| o)
            {
                let cur_w = d
                    .outputs
                    .get(cur_idx)
                    .map_or(1920, |o| o.current.mode.width as i32);
                let other_x = other.current.position.map_or(0, |p| p.x);
                let other_y = other.current.position.map_or(0, |p| p.y);
                pref.set_automatic_position(false);
                pref.set_position_x(other_x - cur_w);
                pref.set_position_y(other_y);
            }
        }
    });

    let weak = window.as_weak();
    let data = model.clone();
    ui.on_position_right_of(move || {
        if let Some(w) = weak.upgrade() {
            let pref = w.global::<OutputPreferences>();
            let d = data.borrow();
            let cur_idx = pref.get_selected_output() as usize;
            if let Some(other) = d
                .outputs
                .iter()
                .enumerate()
                .find(|(i, _)| *i != cur_idx)
                .map(|(_, o)| o)
            {
                let other_w = other.current.mode.width as i32;
                let other_x = other.current.position.map_or(0, |p| p.x);
                let other_y = other.current.position.map_or(0, |p| p.y);
                pref.set_automatic_position(false);
                pref.set_position_x(other_x + other_w);
                pref.set_position_y(other_y);
            }
        }
    });

    let weak = window.as_weak();
    let data = model.clone();
    ui.on_position_above(move || {
        if let Some(w) = weak.upgrade() {
            let pref = w.global::<OutputPreferences>();
            let d = data.borrow();
            let cur_idx = pref.get_selected_output() as usize;
            if let Some(other) = d
                .outputs
                .iter()
                .enumerate()
                .find(|(i, _)| *i != cur_idx)
                .map(|(_, o)| o)
            {
                let cur_h = d
                    .outputs
                    .get(cur_idx)
                    .map_or(1080, |o| o.current.mode.height as i32);
                let other_x = other.current.position.map_or(0, |p| p.x);
                let other_y = other.current.position.map_or(0, |p| p.y);
                pref.set_automatic_position(false);
                pref.set_position_x(other_x);
                pref.set_position_y(other_y - cur_h);
            }
        }
    });

    let weak = window.as_weak();
    let data = model.clone();
    ui.on_position_below(move || {
        if let Some(w) = weak.upgrade() {
            let pref = w.global::<OutputPreferences>();
            let d = data.borrow();
            let cur_idx = pref.get_selected_output() as usize;
            if let Some(other) = d
                .outputs
                .iter()
                .enumerate()
                .find(|(i, _)| *i != cur_idx)
                .map(|(_, o)| o)
            {
                let other_h = other.current.mode.height as i32;
                let other_x = other.current.position.map_or(0, |p| p.x);
                let other_y = other.current.position.map_or(0, |p| p.y);
                pref.set_automatic_position(false);
                pref.set_position_x(other_x);
                pref.set_position_y(other_y + other_h);
            }
        }
    });

    let weak = window.as_weak();
    let data = model.clone();
    let sender = tx.clone();
    ui.on_apply(move || {
        let Some(w) = weak.upgrade() else { return };
        let ui = w.global::<OutputPreferences>();
        if ui.get_busy() || ui.get_pending() {
            return;
        }
        let data = data.borrow();
        if let (Some(output), Some(mode)) = (
            data.outputs.get(ui.get_selected_output() as usize),
            selected_modes(&w, &data).get(ui.get_selected_rate() as usize),
        ) {
            let mirror_of = if ui.get_selected_mirror_index() > 0 {
                let mut mirror_names = Vec::new();
                for other in &data.outputs {
                    if other.name != output.name {
                        mirror_names.push(other.name.clone());
                    }
                }
                mirror_names
                    .get((ui.get_selected_mirror_index() - 1) as usize)
                    .cloned()
            } else {
                None
            };
            if sender
                .try_send(IpcRequest::TestOutput {
                    output: output.name.clone(),
                    profile: OutputProfile {
                        mode: *mode,
                        flipped: ui.get_flipped(),
                        scale_percent: (ui.get_scale_percent().round() as u16).clamp(50, 400),
                        rotation: (ui.get_rotation().clamp(0, 3) * 90) as u16,
                        position: (!ui.get_automatic_position() && mirror_of.is_none()).then_some(
                            rix_config::OutputPosition {
                                x: ui.get_position_x(),
                                y: ui.get_position_y(),
                            },
                        ),
                        mirror_of,
                        primary: ui.get_is_primary(),
                    },
                })
                .is_ok()
            {
                ui.set_busy(true);
                ui.set_error("".into());
            }
        }
    });

    for confirm in [true, false] {
        let weak = window.as_weak();
        let data = model.clone();
        let sender = tx.clone();
        let callback = move || {
            let Some(w) = weak.upgrade() else { return };
            let ui = w.global::<OutputPreferences>();
            if ui.get_busy() {
                return;
            }
            if let Some(id) = data.borrow().pending {
                if sender
                    .try_send(if confirm {
                        IpcRequest::ConfirmOutput(id)
                    } else {
                        IpcRequest::RevertOutput(id)
                    })
                    .is_ok()
                {
                    ui.set_busy(true);
                    ui.set_error("".into());
                }
            }
        };
        if confirm {
            ui.on_confirm(callback);
        } else {
            ui.on_revert(callback);
        }
    }

    let weak = window.as_weak();
    let timer = slint::Timer::default();
    let mut last_poll = Instant::now() - Duration::from_secs(2);
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let Some(w) = weak.upgrade() else { return };
            let ui = w.global::<OutputPreferences>();
            let mut model = model.borrow_mut();
            while let Ok((poll, result)) = rx.try_recv() {
                if poll {
                    model.polling = false;
                } else {
                    ui.set_busy(false);
                }
                match result {
                    Ok(state) => receive(&w, &mut model, state),
                    Err(error) => ui.set_error(
                        format!(
                            "{}: {error}",
                            crate::i18n::tr("Could not update the display")
                        )
                        .into(),
                    ),
                }
            }
            if (w.get_active_tab() == 2 || model.pending.is_some())
                && !model.polling
                && !ui.get_busy()
                && last_poll.elapsed() >= Duration::from_secs(1)
                && tx.try_send(IpcRequest::GetOutputs).is_ok()
            {
                model.polling = true;
                last_poll = Instant::now();
            }
        },
    );
    timer
}
