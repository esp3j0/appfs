use anyhow::Result;

#[cfg(target_os = "windows")]
pub async fn wait_for_shutdown_signal() -> Result<()> {
    let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
    let mut ctrl_break = tokio::signal::windows::ctrl_break()?;
    let mut ctrl_close = tokio::signal::windows::ctrl_close()?;
    let mut ctrl_logoff = tokio::signal::windows::ctrl_logoff()?;
    let mut ctrl_shutdown = tokio::signal::windows::ctrl_shutdown()?;

    tokio::select! {
        _ = ctrl_c.recv() => {}
        _ = ctrl_break.recv() => {}
        _ = ctrl_close.recv() => {}
        _ = ctrl_logoff.recv() => {}
        _ = ctrl_shutdown.recv() => {}
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub async fn wait_for_shutdown_signal() -> Result<()> {
    tokio::signal::ctrl_c().await?;
    Ok(())
}
