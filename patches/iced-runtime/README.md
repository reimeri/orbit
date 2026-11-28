# iced_runtime 0.14.0: Wayland notification activation

`notification-activation.patch` adds the runtime window action that carries a
desktop notification's activation token. It is paired with the iced-winit
dispatch and winit Wayland backend changes; removing only this patch breaks the
path from a notification click to the existing Orbit window.

See [native window activation](../../docs/notifications.md#native-window-activation)
for the cross-crate contract and its verification. On upgrade, check whether
upstream exposes token activation end to end before carrying these patches forward.
