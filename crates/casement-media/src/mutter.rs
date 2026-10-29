//! org.gnome.Mutter.RemoteDesktop + org.gnome.Mutter.ScreenCast: one session that records
//! a monitor (the primary one unless asked otherwise) into PipeWire and accepts pointer and
//! keyboard events.

use std::collections::HashMap;

use futures_util::StreamExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const RD_NAME: &str = "org.gnome.Mutter.RemoteDesktop";
const SC_NAME: &str = "org.gnome.Mutter.ScreenCast";
/// Cursor drawn into the picture (1); 2 would send it as metadata.
const CURSOR_HIDDEN: u32 = 0;
const CURSOR_EMBEDDED: u32 = 1;

pub struct Session {
    connection: Connection,
    remote: Proxy<'static>,
    stream_path: String,
    pub node_id: u32,
    /// The session's monitors and the connector recorded ("" for a virtual monitor).
    pub monitors: Vec<casement_net::screen::Monitor>,
    pub connector: String,
}

type Monitor = (
    (String, String, String, String),
    Vec<(
        String,
        i32,
        i32,
        f64,
        f64,
        Vec<f64>,
        HashMap<String, OwnedValue>,
    )>,
    HashMap<String, OwnedValue>,
);
type Logical = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<(String, String, String, String)>,
    HashMap<String, OwnedValue>,
);

/// The monitors of this session, primary first among equals by position in Mutter's list.
async fn monitors(connection: &Connection) -> zbus::Result<Vec<casement_net::screen::Monitor>> {
    let display = Proxy::new(
        connection,
        "org.gnome.Mutter.DisplayConfig",
        "/org/gnome/Mutter/DisplayConfig",
        "org.gnome.Mutter.DisplayConfig",
    )
    .await?;
    let (_serial, monitors, logical, _props): (
        u32,
        Vec<Monitor>,
        Vec<Logical>,
        HashMap<String, OwnedValue>,
    ) = display.call("GetCurrentState", &()).await?;
    let primary: Option<&String> = logical
        .iter()
        .find(|l| l.4)
        .and_then(|l| l.5.first())
        .map(|spec| &spec.0);
    let in_use: Vec<&String> = logical
        .iter()
        .flat_map(|l| l.5.iter().map(|spec| &spec.0))
        .collect();
    let text = |props: &HashMap<String, OwnedValue>, key: &str| {
        props
            .get(key)
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
    };
    let flag = |props: &HashMap<String, OwnedValue>, key: &str| {
        props
            .get(key)
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false)
    };
    Ok(monitors
        .iter()
        // Connected but switched off monitors cannot be recorded.
        .filter(|m| in_use.contains(&&m.0 .0))
        .map(|(spec, modes, props)| {
            let (width, height) = modes
                .iter()
                .find(|mode| flag(&mode.6, "is-current"))
                .map(|mode| (mode.1.max(0) as u32, mode.2.max(0) as u32))
                .unwrap_or((0, 0));
            let name = text(props, "display-name")
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("{} {}", spec.1, spec.2).trim().to_string());
            casement_net::screen::Monitor {
                id: spec.0.clone(),
                name,
                width,
                height,
                primary: primary == Some(&spec.0),
            }
        })
        .collect())
}

impl Session {
    /// Records `monitor` (a connector; "" or an unknown one means the primary monitor), or
    /// with `virtual_size` a new virtual monitor whose size the PipeWire consumer asks for
    /// (the capture pipeline does).
    pub async fn start(virtual_size: Option<(u32, u32)>, monitor: &str) -> zbus::Result<Self> {
        let connection = Connection::session().await?;
        let rd = Proxy::new(
            &connection,
            RD_NAME,
            "/org/gnome/Mutter/RemoteDesktop",
            RD_NAME,
        )
        .await?;
        let rd_path: OwnedObjectPath = rd.call("CreateSession", &()).await?;
        let remote = Proxy::new(
            &connection,
            RD_NAME,
            rd_path,
            "org.gnome.Mutter.RemoteDesktop.Session",
        )
        .await?;
        let session_id: String = remote.get_property("SessionId").await?;

        let sc = Proxy::new(
            &connection,
            SC_NAME,
            "/org/gnome/Mutter/ScreenCast",
            SC_NAME,
        )
        .await?;
        let mut props: HashMap<&str, Value> = HashMap::new();
        props.insert("remote-desktop-session-id", Value::from(session_id));
        let sc_path: OwnedObjectPath = sc.call("CreateSession", &(props,)).await?;
        let cast = Proxy::new(
            &connection,
            SC_NAME,
            sc_path,
            "org.gnome.Mutter.ScreenCast.Session",
        )
        .await?;

        let mut record: HashMap<&str, Value> = HashMap::new();
        // A virtual monitor can idle its frame clock after a short pause. Waiting for
        // its embedded cursor then adds hundreds of milliseconds, so the client draws
        // that cursor locally. Mirrored monitors keep Mutter's accurate cursor shape.
        let cursor_mode = if virtual_size.is_some() {
            CURSOR_HIDDEN
        } else {
            CURSOR_EMBEDDED
        };
        record.insert("cursor-mode", Value::U32(cursor_mode));
        let monitors = monitors(&connection).await.unwrap_or_default();
        let (stream_path, connector): (OwnedObjectPath, String) = match virtual_size {
            Some(_) => (cast.call("RecordVirtual", &(record,)).await?, String::new()),
            None => {
                let connector = monitors
                    .iter()
                    .find(|m| m.id == monitor)
                    .or_else(|| monitors.iter().find(|m| m.primary))
                    .or_else(|| monitors.first())
                    .map(|m| m.id.clone())
                    .ok_or_else(|| zbus::Error::Failure("no monitor is connected".into()))?;
                let path = cast
                    .call("RecordMonitor", &(connector.as_str(), record))
                    .await?;
                (path, connector)
            }
        };
        let stream = Proxy::new(
            &connection,
            SC_NAME,
            stream_path.clone(),
            "org.gnome.Mutter.ScreenCast.Stream",
        )
        .await?;
        let mut added = stream.receive_signal("PipeWireStreamAdded").await?;

        // Starting the remote desktop session starts the linked screen cast too.
        remote.call::<_, _, ()>("Start", &()).await?;
        let message = tokio::time::timeout(std::time::Duration::from_secs(10), added.next())
            .await
            .map_err(|_| zbus::Error::Failure("Mutter did not start the screen stream".into()))?
            .ok_or_else(|| zbus::Error::Failure("the screen stream signal ended".into()))?;
        let (node_id,): (u32,) = message.body().deserialize()?;
        Ok(Self {
            connection,
            remote,
            stream_path: stream_path.to_string(),
            node_id,
            monitors,
            connector,
        })
    }

    pub async fn pointer_motion(&self, x: f64, y: f64) -> zbus::Result<()> {
        self.remote
            .call(
                "NotifyPointerMotionAbsolute",
                &(self.stream_path.as_str(), x, y),
            )
            .await
    }

    pub async fn pointer_button(&self, button: i32, pressed: bool) -> zbus::Result<()> {
        self.remote
            .call("NotifyPointerButton", &(button, pressed))
            .await
    }

    /// axis 0 = vertical, 1 = horizontal; steps > 0 scroll down / right.
    pub async fn scroll_discrete(&self, axis: u32, steps: i32) -> zbus::Result<()> {
        self.remote
            .call("NotifyPointerAxisDiscrete", &(axis, steps))
            .await
    }

    pub async fn key(&self, code: u32, pressed: bool) -> zbus::Result<()> {
        self.remote
            .call("NotifyKeyboardKeycode", &(code, pressed))
            .await
    }

    /// Clipboard: Mutter mirrors the session's selection into the desktop.
    pub async fn enable_clipboard(&self) -> zbus::Result<()> {
        let options: HashMap<&str, Value> = HashMap::new();
        self.remote.call("EnableClipboard", &(options,)).await
    }

    /// Announce that the session (the client) now owns the clipboard with these types.
    pub async fn set_selection(&self, mime_types: &[&str]) -> zbus::Result<()> {
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("mime-types", Value::from(mime_types.to_vec()));
        self.remote.call("SetSelection", &(options,)).await
    }

    pub async fn selection_read(&self, mime_type: &str) -> zbus::Result<std::os::fd::OwnedFd> {
        let fd: zbus::zvariant::OwnedFd = self.remote.call("SelectionRead", &(mime_type,)).await?;
        Ok(fd.into())
    }

    pub async fn selection_write(&self, serial: u32) -> zbus::Result<std::os::fd::OwnedFd> {
        let fd: zbus::zvariant::OwnedFd = self.remote.call("SelectionWrite", &(serial,)).await?;
        Ok(fd.into())
    }

    pub async fn selection_write_done(&self, serial: u32, success: bool) -> zbus::Result<()> {
        self.remote
            .call("SelectionWriteDone", &(serial, success))
            .await
    }

    /// Owner changes (`mime-types`, `session-is-owner`) and paste requests on the host.
    pub async fn clipboard_signals(
        &self,
    ) -> zbus::Result<(
        zbus::proxy::SignalStream<'static>,
        zbus::proxy::SignalStream<'static>,
    )> {
        let owner = self.remote.receive_signal("SelectionOwnerChanged").await?;
        let transfer = self.remote.receive_signal("SelectionTransfer").await?;
        Ok((owner, transfer))
    }

    pub async fn stop(&self) {
        let _ = self.remote.call::<_, _, ()>("Stop", &()).await;
        let _ = &self.connection;
    }
}
