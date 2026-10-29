//! The real QUIC, H.264 decoder and file pages against a private loopback fixture.

use super::*;

pub(super) async fn run(window: &Window, root: &Path) -> Result<Vec<String>, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if !root.join(".casement-ui-fixture").is_file()
        || root.join("client") != window.core().dir
        || glib::user_special_dir(glib::UserDirectory::Downloads).as_ref()
            != Some(&root.join("downloads"))
    {
        return Err("live fixture: отказ от работы вне изолированных профиля и Загрузок".into());
    }
    let mut checks = Vec::new();
    let navigation = window.navigation();
    let computer = window
        .store()
        .find("fixture")
        .ok_or("live fixture: нет подключения")?;
    if computer.host != "127.0.0.1" {
        return Err("live fixture: разрешён только loopback".into());
    }
    if std::env::var("CASEMENT_UI_LATENCY").as_deref() != Ok("1") {
        for kind in ["screen", "audio"] {
            fs::write(root.join(format!("pause-{kind}")), "pause").map_err(|e| e.to_string())?;
            crate::session::open(window, computer.clone());
            wait_until("host provider startup begins", STATE_TIMEOUT, || {
                root.join(format!("{kind}-starting")).is_file()
            })
            .await?;
            navigation.pop();
            wait_until(
                "leaving session cancels host provider startup",
                Duration::from_secs(2),
                || root.join(format!("{kind}-cancelled")).is_file(),
            )
            .await?;
            fs::remove_file(root.join(format!("pause-{kind}"))).map_err(|e| e.to_string())?;
            checks.push(format!(
                "leaving during {kind} startup cancels the real connection and host provider"
            ));
        }

        let virtual_computer = window
            .store()
            .find("fixture-virtual")
            .ok_or("live fixture: нет виртуального подключения")?;
        crate::session::open(window, virtual_computer);
        wait_tag(&navigation, "session", STATE_TIMEOUT).await?;
        let virtual_session = visible_page(&navigation)?;
        let virtual_picture = descendants(virtual_session.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::Picture>().ok())
            .ok_or("live fixture: нет картинки виртуального сеанса")?;
        wait_frame(&virtual_picture).await?;
        let request = fs::read_to_string(root.join("screen-request")).map_err(|e| e.to_string())?;
        if !request.ends_with(" virtual=true")
            || ![
                "1280x720",
                "1600x900",
                "1920x1080",
                "2560x1440",
                "3840x2160",
            ]
            .iter()
            .any(|mode| request.starts_with(mode))
        {
            return Err(format!(
                "live fixture: виртуальный рабочий стол запрошен с неверной геометрией {request:?}"
            ));
        }
        window.widget().set_default_size(700, 540);
        glib::timeout_future(Duration::from_millis(700)).await;
        if root.join("unexpected-resize").exists() {
            return Err("live fixture: размер окна перенастроил живой виртуальный экран".into());
        }
        if active_tag(&navigation).as_deref() != Some("session")
            || virtual_picture.paintable().is_none()
        {
            return Err("live fixture: уменьшение окна разрушило виртуальный сеанс".into());
        }
        window.widget().set_default_size(900, 660);
        tooltip_button(virtual_session.upcast_ref(), "Завершить сеанс")?.emit_clicked();
        wait_tag(&navigation, "home", STATE_TIMEOUT).await?;
        checks
            .push("virtual desktop keeps one standard mode while the client window resizes".into());
    }
    let connection_files = crate::session::open_for_check(window, computer);
    wait_tag(&navigation, "session", STATE_TIMEOUT).await?;
    let session = visible_page(&navigation)?;
    let picture = descendants(session.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Picture>().ok())
        .ok_or("live fixture: нет картинки сеанса")?;
    wait_frame(&picture).await?;
    let frame_updates = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let updates = frame_updates.clone();
    picture
        .paintable()
        .ok_or("live fixture: нет decoded paintable")?
        .connect_invalidate_contents(move |_| updates.set(updates.get() + 1));
    let saved = window
        .store()
        .find("fixture")
        .ok_or("live fixture: запись исчезла")?;
    if saved.fingerprint.is_empty() || saved.last_connected == 0 || saved.os_display_name.is_empty()
    {
        return Err("live fixture: успешный вход не сохранил ключ, ОС и время подключения".into());
    }
    checks.push(
        "real loopback login persists first-use key, operating system and connection time".into(),
    );

    // The same signal as pointer entry, confined to our private GTK widget and fake host.
    picture.allocate(800, 600, -1, None);
    let controllers = picture.observe_controllers();
    let motion = (0..controllers.n_items())
        .filter_map(|i| controllers.item(i))
        .find_map(|controller| controller.downcast::<gtk::EventControllerMotion>().ok())
        .ok_or("live fixture: нет контроллера указателя")?;
    motion.emit_by_name::<()>("enter", &[&400f64, &300f64]);
    let cursor = picture
        .cursor()
        .and_then(|cursor| cursor.texture())
        .ok_or("live fixture: локальный курсор не скрыт над декодированным видео")?;
    let mut pixel = [255u8; 4];
    cursor.download(&mut pixel, 4);
    if cursor.width() != 1 || cursor.height() != 1 || pixel[3] != 0 {
        return Err("live fixture: курсор над видео не прозрачен".into());
    }
    checks.push("local cursor is transparent over a real decoded frame".into());

    if std::env::var("CASEMENT_UI_LATENCY").as_deref() == Ok("1") {
        checks.extend(
            super::latency::run(
                window,
                &picture,
                &connection_files().ok_or("latency: missing file sender")?,
                &root,
            )
            .await?,
        );
        navigation.pop();
        wait_tag(&navigation, "home", STATE_TIMEOUT).await?;
        return Ok(checks);
    }

    tooltip_button(session.upcast_ref(), "Файлы")?.emit_clicked();
    wait_tag(&navigation, "remote-files", STATE_TIMEOUT).await?;
    let files = visible_page(&navigation)?;
    wait_row(files.upcast_ref(), "visible.txt").await?;
    if row(files.upcast_ref(), ".hidden.txt").is_ok() {
        return Err("live files: скрытый файл показан без включения переключателя".into());
    }
    let hidden = tooltip_button(files.upcast_ref(), "Скрытые файлы")?
        .downcast::<gtk::ToggleButton>()
        .map_err(|_| "live files: не ToggleButton")?;
    hidden.set_active(true);
    wait_row(files.upcast_ref(), ".hidden.txt").await?;
    hidden.set_active(false);
    checks.push("remote listing and hidden-file toggle use the real file service".into());

    row(files.upcast_ref(), "folder")?.emit_by_name::<()>("activated", &[]);
    wait_row(files.upcast_ref(), "nested.txt").await?;
    tooltip_button(files.upcast_ref(), "На уровень выше")?.emit_clicked();
    wait_row(files.upcast_ref(), "visible.txt").await?;
    let path = descendants(files.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Entry>().ok())
        .ok_or("live files: нет строки пути")?;
    path.set_text("../outside");
    path.emit_activate();
    if !path.has_css_class("error") || row(files.upcast_ref(), "visible.txt").is_err() {
        return Err("live files: недопустимый путь изменил список или не отмечен".into());
    }
    path.set_text("");
    path.emit_activate();
    wait_row(files.upcast_ref(), "visible.txt").await?;
    checks.push("remote folder navigation and invalid-path recovery preserve the page".into());

    let download_row = row(files.upcast_ref(), "visible.txt")?;
    tooltip_button(download_row.upcast_ref(), "Скачать")?.emit_clicked();
    wait_until("real file download", STATE_TIMEOUT, || {
        fs::read(root.join("downloads/visible.txt")).ok().as_deref() == Some(b"download fixture\n")
    })
    .await?;
    checks.push("downloaded file reaches private Downloads with the original contents".into());

    find_button(&descendants(files.upcast_ref()), "Отправить")
        .ok_or("live files: нет кнопки отправки")?
        .emit_clicked();
    wait_tag(&navigation, "local-files", STATE_TIMEOUT).await?;
    let picker = visible_page(&navigation)?;
    for name in ["upload-a.txt", "upload-b.txt"] {
        wait_row(picker.upcast_ref(), name).await?;
        let check = descendants(row(picker.upcast_ref(), name)?.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::CheckButton>().ok())
            .ok_or("live picker: нет отметки файла")?;
        check.set_active(true);
    }
    find_button(&descendants(picker.upcast_ref()), "Отправить")
        .ok_or("live picker: нет подтверждения")?
        .emit_clicked();
    wait_tag(&navigation, "remote-files", STATE_TIMEOUT).await?;
    wait_until("sequential real uploads", STATE_TIMEOUT, || {
        fs::read(root.join("uploads/upload-a.txt")).ok().as_deref() == Some(b"first upload\n")
            && fs::read(root.join("uploads/upload-b.txt")).ok().as_deref()
                == Some(b"second upload\n")
    })
    .await?;
    if visible_page(&navigation)? != files {
        return Err("live files: после выбора файлов не восстановлена исходная страница".into());
    }
    checks.push(
        "page picker selects two files and sequential uploads preserve their contents".into(),
    );
    let cancel_row = row(files.upcast_ref(), "cancel.bin")?;
    tooltip_button(cancel_row.upcast_ref(), "Скачать")?.emit_clicked();
    wait_until("download has partial data", STATE_TIMEOUT, || {
        has_partial(&root)
    })
    .await?;
    tooltip_button(cancel_row.upcast_ref(), "Отменить скачивание")?.emit_clicked();
    wait_until("cancel removes partial download", STATE_TIMEOUT, || {
        !has_partial(&root)
    })
    .await?;
    if root.join("downloads/cancel.bin").exists() {
        return Err("live files: cancelled download was published".into());
    }
    tooltip_button(cancel_row.upcast_ref(), "Скачать")?.emit_clicked();
    wait_until("second download has partial data", STATE_TIMEOUT, || {
        has_partial(&root)
    })
    .await?;
    navigation.pop();
    wait_tag(&navigation, "session", STATE_TIMEOUT).await?;
    wait_until(
        "leaving files removes partial download",
        STATE_TIMEOUT,
        || !has_partial(&root),
    )
    .await?;
    if root.join("downloads/cancel.bin").exists() {
        return Err("live files: leaving the page did not cancel the download".into());
    }
    checks.push(
        "cancel and leaving Files both stop in-flight downloads and clean partial files".into(),
    );
    let before = frame_updates.get();
    wait_until(
        "video keeps updating after file pages",
        STATE_TIMEOUT,
        || frame_updates.get() > before,
    )
    .await?;
    checks.push("covering the session with file pages keeps the video alive".into());

    tooltip_button(session.upcast_ref(), "Монитор того компьютера")?.emit_clicked();
    wait_tag(&navigation, "monitors", STATE_TIMEOUT).await?;
    let monitors = visible_page(&navigation)?;
    let second = row(monitors.upcast_ref(), "Второй экран")?;
    let select = descendants(second.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::CheckButton>().ok())
        .ok_or("live monitors: нет переключателя")?;
    select.set_active(true);
    wait_tag(&navigation, "session", STATE_TIMEOUT).await?;
    wait_until("second monitor", STATE_TIMEOUT, || {
        fs::read_to_string(root.join("shown-monitor"))
            .ok()
            .as_deref()
            == Some("fixture-2")
    })
    .await?;
    wait_frame(&picture).await?;
    checks.push("monitor selection restarts the real stream on the selected monitor".into());

    tooltip_button(session.upcast_ref(), "Завершить сеанс")?.emit_clicked();
    wait_tag(&navigation, "home", STATE_TIMEOUT).await?;
    if picture.paintable().is_some() || picture.cursor().is_some() {
        return Err("live session: завершение оставило кадр или скрытый курсор".into());
    }
    checks.push("disconnect returns home and releases the video and local cursor".into());
    Ok(checks)
}

fn row(root: &gtk::Widget, title: &str) -> Result<adw::ActionRow, String> {
    descendants(root)
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.title() == title)
        .ok_or_else(|| format!("live UI: не найдена строка «{title}»"))
}

async fn wait_row(root: &gtk::Widget, title: &str) -> Result<(), String> {
    wait_until(title, STATE_TIMEOUT, || row(root, title).is_ok()).await
}

fn tooltip_button(root: &gtk::Widget, tooltip: &str) -> Result<gtk::Button, String> {
    descendants(root)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.tooltip_text().as_deref() == Some(tooltip))
        .ok_or_else(|| format!("live UI: не найдена кнопка «{tooltip}»"))
}

async fn wait_frame(picture: &gtk::Picture) -> Result<(), String> {
    wait_until("decoded video frame", STATE_TIMEOUT, || {
        picture
            .paintable()
            .is_some_and(|frame| frame.intrinsic_width() == 640 && frame.intrinsic_height() == 360)
    })
    .await
}

fn has_partial(root: &Path) -> bool {
    fs::read_dir(root.join("downloads")).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".casement-")
                && entry.metadata().is_ok_and(|m| m.len() > 0)
        })
    })
}
