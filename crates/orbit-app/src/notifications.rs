//! Desktop delivery only. No notification history, settings, or platform code in core.
use iced::futures::{SinkExt, StreamExt};
use orbit_core::agent::{
    Harness,
    notifications::{Event, Kind},
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::watch;
use zbus::{
    Connection, Proxy,
    proxy::{OwnerChangedStream, SignalStream},
    zvariant::Value,
};

type Desired = HashMap<String, Event>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    pub event: Event,
    pub token: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Message {
    Open(Open),
    Unavailable(String),
}

#[derive(Clone)]
pub struct Client(Arc<watch::Sender<Desired>>);
impl Default for Client {
    fn default() -> Self {
        Self(Arc::new(watch::channel(HashMap::new()).0))
    }
}
impl std::hash::Hash for Client {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}
impl Client {
    pub fn sync(&self, harness: &Harness, focused: bool, selected: Option<&str>) {
        let events = harness.take_notifications();
        self.0.send_if_modified(|desired| {
            let before = desired.clone();
            desired.retain(|session, event| {
                should_notify(focused, selected, session) && harness.notification_is_current(event)
            });
            for event in events {
                if should_notify(focused, selected, &event.session_id) {
                    desired.insert(event.session_id.clone(), event);
                }
            }
            *desired != before
        });
    }
    pub fn contains(&self, event: &Event) -> bool {
        self.0.borrow().get(&event.session_id) == Some(event)
    }
    pub fn subscription(&self) -> iced::Subscription<Message> {
        iced::Subscription::run_with(self.clone(), run)
    }
}
fn should_notify(focused: bool, selected: Option<&str>, session: &str) -> bool {
    !focused || selected != Some(session)
}
fn reason(kind: &Kind) -> &'static str {
    match kind {
        Kind::Question(_) => "Answer needed",
        Kind::Finished => "Agent finished",
        Kind::Failed => "Agent needs attention — an error occurred",
    }
}

struct Desktop {
    proxy: Proxy<'static>,
    signals: SignalStream<'static>,
    owners: OwnerChangedStream<'static>,
}

struct Shown {
    event: Event,
    id: u32,
    notified_at: zbus::message::Sequence,
    token: Option<String>,
}

impl Shown {
    fn matches(&self, id: u32, signal: &zbus::Message) -> bool {
        // A queued click or close may predate Notify's replacement of the same ID.
        self.id == id && signal.recv_position() > self.notified_at
    }
}

#[derive(Default)]
struct Delivery {
    owner: Option<zbus::names::OwnedUniqueName>,
    shown: HashMap<String, Shown>,
    delivered: HashMap<String, u64>,
}
impl Desktop {
    async fn connect(connection: Connection, delivery: &mut Delivery) -> zbus::Result<Self> {
        let name = "org.freedesktop.Notifications";
        let watched = Proxy::new_owned(
            connection.clone(),
            name,
            "/org/freedesktop/Notifications",
            name,
        )
        .await?;
        let owners = watched.receive_owner_changed().await?;
        let bus = zbus::fdo::DBusProxy::new(&connection).await?;
        let name = name.try_into().expect("static bus name");
        let owner = match bus.get_name_owner(name).await {
            Ok(owner) => owner,
            Err(zbus::fdo::Error::NameHasNoOwner(_)) => {
                bus.start_service_by_name(
                    "org.freedesktop.Notifications"
                        .try_into()
                        .expect("static bus name"),
                    0,
                )
                .await?;
                bus.get_name_owner(
                    "org.freedesktop.Notifications"
                        .try_into()
                        .expect("static bus name"),
                )
                .await?
            }
            Err(error) => return Err(error.into()),
        };
        // Pin calls/signals to this unique owner, so an ID from a replacement
        // daemon can never be mistaken for an old conversation's notification.
        let proxy = Proxy::new_owned(
            connection,
            owner.clone(),
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .await?;
        let signals = proxy.receive_all_signals().await?;
        if delivery.owner.as_ref() != Some(&owner) {
            delivery.shown.clear();
            delivery.owner = Some(owner);
        } else {
            // Positions are connection-local. Don't pair signals across a disconnect.
            for shown in delivery.shown.values_mut() {
                shown.notified_at = Default::default();
                shown.token = None;
            }
        }
        Ok(Self {
            proxy,
            signals,
            owners,
        })
    }
    async fn sync(&mut self, desired: &Desired, delivery: &mut Delivery) -> zbus::Result<()> {
        let obsolete: Vec<_> = delivery
            .shown
            .keys()
            .filter(|id| !desired.contains_key(*id))
            .cloned()
            .collect();
        for session in obsolete {
            let id = delivery.shown[&session].id;
            self.proxy
                .call::<_, _, ()>("CloseNotification", &(id,))
                .await?;
            delivery.shown.remove(&session);
        }
        delivery
            .delivered
            .retain(|session, _| desired.contains_key(session));
        for (session, event) in desired {
            if delivery.delivered.get(session) == Some(&event.id) {
                continue;
            }
            let replaces = delivery.shown.get(session).map_or(0, |shown| shown.id);
            let hints = HashMap::from([
                ("desktop-entry", Value::from(orbit_core::APP_ID)),
                ("urgency", Value::from(1_u8)),
            ]);
            let reply = self
                .proxy
                .call_method(
                    "Notify",
                    &(
                        "Orbit",
                        replaces,
                        orbit_core::APP_ID,
                        &event.title,
                        reason(&event.kind),
                        vec!["default", "Open conversation"],
                        hints,
                        -1_i32,
                    ),
                )
                .await?;
            delivery.shown.insert(
                session.clone(),
                Shown {
                    event: event.clone(),
                    id: reply.body().deserialize()?,
                    notified_at: reply.recv_position(),
                    token: None,
                },
            );
            delivery.delivered.insert(session.clone(), event.id);
        }
        Ok(())
    }
}

// The notification protocol sends the optional token *before* the action.
// Keep it on that live delivery, never in a process-wide "last token" slot.
impl Delivery {
    fn signal(&mut self, signal: zbus::Message) -> zbus::Result<Option<Open>> {
        match signal.header().member().map(|m| m.as_str()) {
            Some("ActivationToken") => {
                let (id, token): (u32, String) = signal.body().deserialize()?;
                if let Some(shown) = self.shown.values_mut().find(|s| s.matches(id, &signal)) {
                    shown.token = (!token.is_empty()).then_some(token);
                }
            }
            Some("ActionInvoked") => {
                let (id, action): (u32, String) = signal.body().deserialize()?;
                // Orbit posts non-resident notifications: invoking an action consumes it.
                if let Some((_, shown)) =
                    self.shown.extract_if(|_, s| s.matches(id, &signal)).next()
                    && action == "default"
                {
                    return Ok(Some(Open {
                        event: shown.event,
                        token: shown.token,
                    }));
                }
            }
            Some("NotificationClosed") => {
                let (id, _reason): (u32, u32) = signal.body().deserialize()?;
                self.shown.retain(|_, s| !s.matches(id, &signal));
            }
            _ => {}
        }
        Ok(None)
    }
}

async fn bounded<T>(
    future: impl std::future::Future<Output = zbus::Result<T>>,
) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .map_err(|_| "Desktop notification service timed out".to_owned())?
        .map_err(|e| e.to_string())
}
fn run(client: &Client) -> iced::futures::stream::BoxStream<'static, Message> {
    let mut changes = client.0.subscribe();
    Box::pin(iced::stream::channel(
        16,
        async move |mut output: iced::futures::channel::mpsc::Sender<Message>| {
            let mut delivery = Delivery::default();
            let mut warned = false;
            loop {
                while changes.borrow().is_empty() && delivery.shown.is_empty() {
                    if changes.changed().await.is_err() {
                        return;
                    }
                }
                let result = async {
                let mut desktop = bounded(async { Desktop::connect(Connection::session().await?, &mut delivery).await }).await?;
                loop {
                    let desired = changes.borrow_and_update().clone();
                    bounded(desktop.sync(&desired, &mut delivery)).await?;
                    tokio::select! {
                        changed = changes.changed() => {
                            if changed.is_err() { return Ok::<_, String>(()); }
                        }
                        signal = desktop.signals.next() => {
                            let signal = signal.ok_or("Desktop notification connection closed")?;
                            if let Some(open) = delivery.signal(signal).map_err(|e| e.to_string())?
                                && changes.borrow().get(&open.event.session_id) == Some(&open.event)
                                && output.send(Message::Open(open)).await.is_err()
                            { return Ok(()); }
                        }
                        _ = desktop.owners.next() => return Err("Desktop notification service restarted".into()),
                    }
                }
            }.await;
                match result {
                    Ok(()) => return,
                    Err(error) => {
                        if !warned {
                            warned = true;
                            let message = format!(
                                "Desktop notifications unavailable: {error}. Orbit will retry automatically. Check that your desktop notification service and session D-Bus are running; agent work is unaffected."
                            );
                            eprintln!("{message}");
                            if output.send(Message::Unavailable(message)).await.is_err() {
                                return;
                            }
                        }
                        // Keep the latest live events, but never replay successfully delivered ones.
                        // The view can withdraw an answered/stopped/deleted event during this delay.
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                }
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_suppression_requires_both_focus_and_matching_conversation() {
        assert!(!should_notify(true, Some("main-a"), "main-a"));
        assert!(should_notify(true, Some("main-b"), "main-a"));
        assert!(should_notify(false, Some("main-a"), "main-a"));
        assert!(should_notify(false, Some("main-b"), "main-a"));
        assert!(should_notify(true, None, "main-a"));
        assert!(should_notify(true, Some("child-a"), "main-a"));
    }

    #[tokio::test]
    async fn suppressed_events_are_consumed_and_focus_or_deletion_withdraws_live_events() {
        use orbit_core::store::{Session, Settings, Store};
        let dir = tempfile::tempdir().unwrap();
        let h = Harness::new(Store::open(dir.path().into()).unwrap(), vec![]).unwrap();
        let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
        let client = Client::default();
        for (submission, focused, selected) in [
            ("visible", true, Some(c.id.as_str())),
            ("background", true, Some("other-session")),
            ("unfocused", false, Some(c.id.as_str())),
        ] {
            let mut changes = h.subscribe();
            h.send(
                &c.id,
                submission,
                "Offline request".into(),
                Settings::default(),
            )
            .unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while c.is_executing() {
                    changes.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            client.sync(&h, focused, selected);
            if submission == "visible" {
                assert!(client.0.borrow().is_empty());
                client.sync(&h, false, selected);
                assert!(
                    client.0.borrow().is_empty(),
                    "losing focus must not replay a suppressed event"
                );
            } else {
                assert_eq!(client.0.borrow().len(), 1);
                let event = client.0.borrow()[&c.id].clone();
                assert_eq!(event.kind, Kind::Failed);
                if submission == "background" {
                    client.sync(&h, true, Some(&c.id));
                    assert!(
                        !client.contains(&event),
                        "viewing the conversation withdraws it"
                    );
                } else {
                    h.remove(&c.id).unwrap();
                    client.sync(&h, false, selected);
                    assert!(!client.contains(&event), "deletion invalidates late clicks");
                }
            }
        }
    }

    struct Bus(std::process::Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            self.0.kill().expect("stop private D-Bus");
            self.0.wait().expect("reap private D-Bus");
        }
    }
    #[derive(Debug)]
    struct Call {
        replaces: u32,
        title: String,
        body: String,
        actions: Vec<String>,
        desktop_entry: String,
    }
    struct Fake {
        calls: Arc<std::sync::Mutex<Vec<Call>>>,
        closed: Arc<std::sync::Mutex<Vec<u32>>>,
        fail_close: Arc<std::sync::atomic::AtomicBool>,
    }
    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl Fake {
        #[allow(clippy::too_many_arguments)] // The standard D-Bus method signature.
        fn notify(
            &self,
            app: &str,
            replaces: u32,
            icon: &str,
            title: &str,
            body: &str,
            actions: Vec<String>,
            hints: HashMap<String, zbus::zvariant::OwnedValue>,
            timeout: i32,
        ) -> u32 {
            assert_eq!(app, "Orbit");
            assert_eq!(icon, "orbit");
            assert_eq!(timeout, -1);
            assert_eq!(u8::try_from(&hints["urgency"]).unwrap(), 1);
            let mut calls = self.calls.lock().unwrap();
            calls.push(Call {
                replaces,
                title: title.into(),
                body: body.into(),
                actions,
                desktop_entry: <&str>::try_from(&hints["desktop-entry"]).unwrap().into(),
            });
            if replaces == 0 {
                40 + calls.len() as u32
            } else {
                replaces
            }
        }
        fn close_notification(&self, id: u32) -> zbus::fdo::Result<()> {
            if self
                .fail_close
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                return Err(zbus::fdo::Error::Failed("transient fixture failure".into()));
            }
            self.closed.lock().unwrap().push(id);
            Ok(())
        }
    }
    #[tokio::test]
    async fn native_protocol_replaces_routes_closes_and_does_not_repeat_dismissed_events() {
        use std::io::BufRead;
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("bus.conf");
        // Do not depend on /etc/dbus-1/session.conf, user services or a machine ID
        // in sandboxed package checks. The socket lives in this private directory.
        std::fs::write(
            &config,
            format!(
                r#"<busconfig><type>session</type>
            <listen>unix:dir={}</listen><auth>EXTERNAL</auth>
            <policy context="default"><allow send_destination="*"/>
            <allow receive_sender="*"/><allow own="*"/></policy></busconfig>"#,
                directory.path().display()
            ),
        )
        .unwrap();
        let mut process = std::process::Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("private dbus-daemon (provided by nix develop)");
        let mut address = String::new();
        std::io::BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let _bus = Bus(process);
        assert!(
            !address.trim().is_empty(),
            "private D-Bus did not print its address"
        );
        let calls = Arc::new(std::sync::Mutex::new(vec![]));
        let closed = Arc::new(std::sync::Mutex::new(vec![]));
        let fail_close = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .serve_at(
                "/org/freedesktop/Notifications",
                Fake {
                    calls: calls.clone(),
                    closed: closed.clone(),
                    fail_close: fail_close.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let connection = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();
        let mut delivery = Delivery::default();
        let mut desktop = Desktop::connect(connection, &mut delivery).await.unwrap();
        let mut event = Event {
            id: 7,
            session_id: "background-b".into(),
            title: "Project λ <title>".into(),
            kind: Kind::Question("private-request".into()),
        };
        let mut desired = HashMap::from([(event.session_id.clone(), event.clone())]);
        desktop.sync(&desired, &mut delivery).await.unwrap();
        desktop.sync(&desired, &mut delivery).await.unwrap();
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "refreshes must not repeat delivery"
        );
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls[0].replaces, 0);
            assert_eq!(calls[0].title, "Project λ <title>");
            assert_eq!(calls[0].body, "Answer needed");
            assert_eq!(calls[0].actions, ["default", "Open conversation"]);
            assert_eq!(calls[0].desktop_entry, "orbit");
        }
        // A transient error must retain native IDs when the daemon is unchanged.
        fail_close.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(desktop.sync(&HashMap::new(), &mut delivery).await.is_err());
        drop(desktop);
        let reconnected = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();
        let mut desktop = Desktop::connect(reconnected, &mut delivery).await.unwrap();
        desktop.sync(&desired, &mut delivery).await.unwrap();
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "reconnect must not duplicate delivery"
        );
        // Queue signals for the old event, but process them only after Notify
        // reuses its native ID. Neither the click nor the close belongs to the replacement.
        macro_rules! emit {
            ($name:literal, $body:expr) => {
                server
                    .emit_signal(
                        None::<&str>,
                        "/org/freedesktop/Notifications",
                        "org.freedesktop.Notifications",
                        $name,
                        &$body,
                    )
                    .await
                    .unwrap()
            };
        }
        async fn next(desktop: &mut Desktop, delivery: &mut Delivery) -> Option<Open> {
            let signal = tokio::time::timeout(Duration::from_secs(3), desktop.signals.next())
                .await
                .unwrap()
                .unwrap();
            delivery.signal(signal).unwrap()
        }
        emit!("ActivationToken", (41_u32, "superseded"));
        emit!("ActionInvoked", (41_u32, "default"));
        emit!("NotificationClosed", (41_u32, 2_u32));
        event.id = 8;
        event.kind = Kind::Failed;
        desired.insert(event.session_id.clone(), event.clone());
        desktop.sync(&desired, &mut delivery).await.unwrap();
        assert_eq!(calls.lock().unwrap()[1].replaces, 41);
        assert_eq!(
            calls.lock().unwrap()[1].body,
            "Agent needs attention — an error occurred"
        );
        for _ in 0..3 {
            assert!(next(&mut desktop, &mut delivery).await.is_none());
        }

        let other = Event {
            id: 15,
            session_id: "other".into(),
            ..event.clone()
        };
        desired.insert(other.session_id.clone(), other.clone());
        desktop.sync(&desired, &mut delivery).await.unwrap();
        // A token must neither open a window by itself nor bleed into a different click.
        emit!("ActivationToken", (43_u32, "other-window-token"));
        assert!(next(&mut desktop, &mut delivery).await.is_none());
        emit!("ActionInvoked", (41_u32, "default"));
        assert_eq!(
            next(&mut desktop, &mut delivery).await,
            Some(Open {
                event: event.clone(),
                token: None
            })
        );
        emit!("ActivationToken", (41_u32, "late"));
        assert!(next(&mut desktop, &mut delivery).await.is_none());
        emit!("ActionInvoked", (43_u32, "default"));
        assert_eq!(
            next(&mut desktop, &mut delivery).await,
            Some(Open {
                event: other,
                token: Some("other-window-token".into())
            })
        );
        emit!("ActionInvoked", (41_u32, "default"));
        assert!(next(&mut desktop, &mut delivery).await.is_none());

        event.id = 9;
        event.kind = Kind::Finished;
        desired.insert(event.session_id.clone(), event.clone());
        desktop.sync(&desired, &mut delivery).await.unwrap();
        assert_eq!(calls.lock().unwrap()[3].body, "Agent finished");
        assert_eq!(
            calls.lock().unwrap()[3].replaces,
            0,
            "do not reuse invoked IDs"
        );
        emit!("ActivationToken", (44_u32, "dismissed"));
        assert!(next(&mut desktop, &mut delivery).await.is_none());
        emit!("NotificationClosed", (44_u32, 2_u32));
        assert!(next(&mut desktop, &mut delivery).await.is_none());
        emit!("ActionInvoked", (44_u32, "default"));
        assert!(next(&mut desktop, &mut delivery).await.is_none());
        desktop.sync(&desired, &mut delivery).await.unwrap();
        assert_eq!(
            calls.lock().unwrap().len(),
            4,
            "dismissal must not cause reposting"
        );
        event.id = 10;
        desired.insert(event.session_id.clone(), event);
        desktop.sync(&desired, &mut delivery).await.unwrap();
        assert_eq!(
            calls.lock().unwrap()[4].replaces,
            0,
            "do not reuse dismissed IDs"
        );
        desktop.sync(&HashMap::new(), &mut delivery).await.unwrap();
        assert_eq!(*closed.lock().unwrap(), [45]);
        assert!(delivery.delivered.is_empty());
    }
}
