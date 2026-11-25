use super::*;

impl App {
    pub(super) fn disk_location(&self) -> PathBuf {
        let path = &self.tabs[self.active].path;
        if path == Path::new("trash:") {
            home()
        } else {
            path.clone()
        }
    }

    pub(super) fn refresh_disk(&mut self) {
        let path = self.disk_location();
        if self.disk_request.is_some()
            || (self.disk_path.as_ref() == Some(&path) && Instant::now() < self.disk_refresh_due)
        {
            return;
        }
        self.disk_next_request = self.disk_next_request.wrapping_add(1);
        let request = self.disk_next_request;
        self.disk_request = Some(request);
        self.disk_refresh_due = Instant::now() + Duration::from_secs(15);
        let sender = self.sender.clone();

        tokio::task::spawn_blocking(move || {
            let result = storage::disk_usage(&path).map_err(|error| {
                tracing::debug!(path = %path.display(), %error, "Cannot read disk capacity");
                error.to_string()
            });
            let disks = storage::mounted_disks();
            let _ = sender.send(Message::MountedDisks(disks));
            let _ = sender.send(Message::Disk {
                request,
                path,
                result,
            });
        });
    }

    pub(super) fn render_disk(&mut self, window: &FilesWindow) {
        let drive_icon = self.icon("drive-harddisk");
        window.set_disk_icon(drive_icon.clone());
        let active_loc = self.disk_location();
        let disk_items: Vec<DiskItem> = self
            .disks
            .iter()
            .map(|disk| {
                let name = if disk.mount_path == Path::new("/") {
                    i18n::tr("System disk")
                } else {
                    disk.mount_path
                        .file_name()
                        .unwrap_or(disk.mount_path.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                        .into()
                };
                DiskItem {
                    name,
                    mount: disk.mount_path.to_string_lossy().into_owned().into(),
                    detail: translated(
                        "{free} free of {total}",
                        &[
                            ("free", size(disk.available_bytes).replace(' ', "\u{00a0}")),
                            ("total", size(disk.total_bytes).replace(' ', "\u{00a0}")),
                        ],
                    )
                    .into(),
                    usage: format!("{:.0}%", disk.usage_ratio * 100.).into(),
                    ratio: disk.usage_ratio as f32,
                    ready: true,
                    icon: drive_icon.clone(),
                    active: active_loc.starts_with(&disk.mount_path),
                }
            })
            .collect();
        window.set_disks(model(disk_items));
        let current = self.disk_path.as_ref() == Some(&self.disk_location());
        if let Some(disk) = self.disk_usage.as_ref().filter(|_| current) {
            window.set_disk_name(if disk.mount_path == Path::new("/") {
                i18n::tr("System disk")
            } else {
                translated(
                    "Disk · {name}",
                    &[(
                        "name",
                        disk.mount_path
                            .file_name()
                            .unwrap_or(disk.mount_path.as_os_str())
                            .to_string_lossy()
                            .into_owned(),
                    )],
                )
                .into()
            });
            window.set_disk_mount(disk.mount_path.to_string_lossy().into_owned().into());
            window.set_disk_detail(
                translated(
                    "{free} free of {total}",
                    &[
                        ("free", size(disk.available_bytes).replace(' ', "\u{00a0}")),
                        ("total", size(disk.total_bytes).replace(' ', "\u{00a0}")),
                    ],
                )
                .into(),
            );
            window.set_disk_ratio(disk.usage_ratio as f32);
            window.set_disk_usage(format!("{:.0}%", disk.usage_ratio * 100.).into());
            window.set_disk_ready(true);
        } else {
            window.set_disk_name(i18n::tr("Disk"));
            window.set_disk_mount("".into());
            window.set_disk_detail(i18n::tr(if current {
                "Disk usage unavailable"
            } else {
                "Reading disk usage…"
            }));
            window.set_disk_ratio(0.);
            window.set_disk_usage("".into());
            window.set_disk_ready(false);
        }
    }

    pub(super) fn disk_target_path(&self) -> PathBuf {
        if self.disk_path.as_ref() == Some(&self.disk_location()) {
            if let Some(disk) = &self.disk_usage {
                return disk.mount_path.clone();
            }
        }
        self.disk_location()
    }

    pub(super) fn open_disk(&mut self) {
        let path = self.disk_target_path();
        self.navigate(path);
    }

    pub(super) fn disk_path_at(&self, index: usize) -> Option<PathBuf> {
        self.disks.get(index).map(|d| d.mount_path.clone())
    }

    pub(super) fn open_disk_at(&mut self, index: usize) {
        if let Some(path) = self.disk_path_at(index) {
            self.navigate(path);
        }
    }

    pub(super) fn action_on_disk(&mut self, action: &str, index: usize) {
        let Some(path) = self.disk_path_at(index) else {
            return;
        };
        match action {
            "open-tab" => self.add_tab(path),
            "terminal" => {
                let sender = self.sender.clone();
                tokio::spawn(async move {
                    if let Err(e) = opening::open_terminal(path).await {
                        let _ = sender.send(Message::Error(e));
                    }
                });
            }
            "copy-path" => {
                let sender = self.sender.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(e) =
                        rix_files::clipboard::path_text(&[path]).and_then(rix_files::clipboard::publish_text)
                    {
                        let _ = sender.send(Message::Error(e));
                    }
                });
            }
            "properties" => self.show_paths_properties(vec![path]),
            _ => {}
        }
    }
}
