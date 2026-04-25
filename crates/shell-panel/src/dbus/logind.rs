use tracing::warn;

// ── zbus proxy ───────────────────────────────────────────────────────────────

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Logind {
    async fn power_off(&self, interactive: bool) -> zbus::Result<()>;
    async fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    async fn suspend(&self, interactive: bool) -> zbus::Result<()>;
    async fn lock_sessions(&self) -> zbus::Result<()>;
}

// ── Actions ──────────────────────────────────────────────────────────────────

async fn with_logind<F, Fut>(f: F)
where
    F: FnOnce(LogindProxy<'static>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    match zbus::Connection::system().await {
        Ok(conn) => match LogindProxy::new(&conn).await {
            Ok(proxy) => f(proxy).await,
            Err(e) => warn!("logind proxy: {e}"),
        },
        Err(e) => warn!("logind system bus: {e}"),
    }
}

/// Lock all login sessions.
pub async fn lock_session() {
    with_logind(|p| async move {
        let _ = p.lock_sessions().await;
    })
    .await;
}

/// Initiate a polite system shutdown.
pub async fn shutdown() {
    with_logind(|p| async move {
        let _ = p.power_off(true).await;
    })
    .await;
}

/// Initiate a polite system reboot.
pub async fn reboot() {
    with_logind(|p| async move {
        let _ = p.reboot(true).await;
    })
    .await;
}

/// Suspend the system.
pub async fn suspend() {
    with_logind(|p| async move {
        let _ = p.suspend(true).await;
    })
    .await;
}
