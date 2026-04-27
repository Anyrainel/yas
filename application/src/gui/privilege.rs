pub fn ensure_admin_for_action() -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        yas::utils::ensure_admin()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(())
    }
}
