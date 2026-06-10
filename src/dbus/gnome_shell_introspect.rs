use std::collections::HashMap;

use zbus::fdo::{self, RequestNameFlags};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{SerializeDict, Type, Value};

use super::Start;

pub struct Introspect {
    to_niri: calloop::channel::Sender<IntrospectToNiri>,
    from_niri: async_channel::Receiver<NiriToIntrospect>,
}

pub enum IntrospectToNiri {
    GetWindows,
}

pub enum NiriToIntrospect {
    Windows(HashMap<u64, WindowProperties>),
}

#[derive(Debug, SerializeDict, Type, Value)]
#[zvariant(signature = "dict")]
pub struct WindowProperties {
    /// Window title.
    pub title: String,
    /// Window app ID.
    ///
    /// This is actually the name of the .desktop file, and Shell does internal tracking to match
    /// Wayland app IDs to desktop files. We don't do that yet, which is the reason why
    /// xdg-desktop-portal-gnome's window list is missing icons.
    #[zvariant(rename = "app-id")]
    pub app_id: String,
}

#[interface(name = "org.gnome.Shell.Introspect")]
impl Introspect {
    async fn get_windows(&self) -> fdo::Result<HashMap<u64, WindowProperties>> {
        if let Err(err) = self.to_niri.send(IntrospectToNiri::GetWindows) {
            warn!("error sending message to niri: {err:?}");
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        match self.from_niri.recv().await {
            Ok(NiriToIntrospect::Windows(windows)) => Ok(windows),
            Err(err) => {
                warn!("error receiving message from niri: {err:?}");
                Err(fdo::Error::Failed("internal error".to_owned()))
            }
        }
    }

    /// Emitted by `Niri::refresh_introspect_windows` when the window list changes.
    #[zbus(signal)]
    pub async fn windows_changed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;
}

impl Introspect {
    pub fn new(
        to_niri: calloop::channel::Sender<IntrospectToNiri>,
        from_niri: async_channel::Receiver<NiriToIntrospect>,
    ) -> Self {
        Self { to_niri, from_niri }
    }
}

impl Start for Introspect {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        let conn = zbus::blocking::Connection::session()?;
        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        conn.object_server()
            .at("/org/gnome/Shell/Introspect", self)?;
        conn.request_name_with_flags("org.gnome.Shell.Introspect", flags)?;

        Ok(conn)
    }
}

/// Order-independent signature of the window list as exposed over Introspect.
///
/// XOR-folds a per-window hash of (id, title, app-id), so reordering windows yields the same
/// signature while any open/close/retitle/re-app-id changes it. Used to decide when to emit
/// [`Introspect::windows_changed`].
pub fn windows_signature<'a>(windows: impl Iterator<Item = (u64, &'a str, &'a str)>) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};

    windows.fold(0, |acc, (id, title, app_id)| {
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        title.hash(&mut hasher);
        app_id.hash(&mut hasher);
        acc ^ hasher.finish()
    })
}

#[cfg(test)]
mod signature_tests {
    use super::*;

    const A: (u64, &str, &str) = (1, "Terminal — ~", "kitty");
    const B: (u64, &str, &str) = (2, "Meeting", "Zoom");
    const C: (u64, &str, &str) = (3, "inbox", "thunderbird");

    #[test]
    fn order_independent() {
        let fwd = windows_signature([A, B, C].into_iter());
        let rev = windows_signature([C, B, A].into_iter());
        assert_eq!(fwd, rev);
    }

    #[test]
    fn add_remove_retitle_change_signature() {
        let base = windows_signature([A, B].into_iter());

        let added = windows_signature([A, B, C].into_iter());
        assert_ne!(base, added);

        let removed = windows_signature([A].into_iter());
        assert_ne!(base, removed);

        let retitled = windows_signature([A, (2, "Meeting — sharing", "Zoom")].into_iter());
        assert_ne!(base, retitled);

        let new_app_id = windows_signature([A, (2, "Meeting", "zoom")].into_iter());
        assert_ne!(base, new_app_id);
    }

    #[test]
    fn empty_is_stable() {
        let a = windows_signature(std::iter::empty());
        let b = windows_signature(std::iter::empty());
        assert_eq!(a, b);
    }
}
