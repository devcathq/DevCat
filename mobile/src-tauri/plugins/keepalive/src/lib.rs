//! Keeps DevCat Mobile running while it's in the background on Android: a foreground service
//! with a quiet "DevCat is connected" notification (android/, `KeepAliveService`). Android
//! otherwise freezes the app soon after it leaves the screen, and with it the connections to
//! the computers. Elsewhere (iOS, desktop) there's nothing to start: `supported()` is false.
//!
//! Driven from the app's Rust side only; the web page gets no command of this plugin.
#[cfg(target_os = "android")]
use serde::Deserialize;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime};

pub struct KeepAlive<R: Runtime> {
    #[cfg(target_os = "android")]
    handle: tauri::plugin::PluginHandle<R>,
    _r: std::marker::PhantomData<fn() -> R>,
}

#[cfg(target_os = "android")]
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Battery {
    #[serde(default)]
    unrestricted: bool,
}

impl<R: Runtime> KeepAlive<R> {
    pub fn supported(&self) -> bool {
        cfg!(target_os = "android")
    }

    #[cfg(target_os = "android")]
    fn run<T: serde::de::DeserializeOwned>(&self, cmd: &str) -> Result<T, String> {
        self.handle.run_mobile_plugin::<T>(cmd, serde_json::json!({})).map_err(|e| e.to_string())
    }

    /// Start the service (call while the app is on screen: Android refuses to start one from
    /// the background).
    pub fn start(&self) -> Result<(), String> {
        #[cfg(target_os = "android")]
        return self.run::<serde_json::Value>("start").map(|_| ());
        #[cfg(not(target_os = "android"))]
        Ok(())
    }

    pub fn stop(&self) -> Result<(), String> {
        #[cfg(target_os = "android")]
        return self.run::<serde_json::Value>("stop").map(|_| ());
        #[cfg(not(target_os = "android"))]
        Ok(())
    }

    /// Whether Android's battery optimization leaves DevCat alone (otherwise it may still cut
    /// the network now and then while the phone sleeps).
    pub fn battery_unrestricted(&self) -> bool {
        #[cfg(target_os = "android")]
        return self.run::<Battery>("battery").map(|b| b.unrestricted).unwrap_or(false);
        #[cfg(not(target_os = "android"))]
        true
    }

    /// Open Android's battery optimization settings, where the user can exempt DevCat.
    pub fn open_battery_settings(&self) -> Result<(), String> {
        #[cfg(target_os = "android")]
        return self.run::<serde_json::Value>("settings").map(|_| ());
        #[cfg(not(target_os = "android"))]
        Ok(())
    }
}

pub trait KeepAliveExt<R: Runtime> {
    fn keep_alive(&self) -> &KeepAlive<R>;
}

impl<R: Runtime, T: Manager<R>> KeepAliveExt<R> for T {
    fn keep_alive(&self) -> &KeepAlive<R> {
        self.state::<KeepAlive<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("devcat-keepalive")
        .setup(|app, _api| {
            #[cfg(target_os = "android")]
            let handle = _api.register_android_plugin("dev.devcat.keepalive", "KeepAlivePlugin")?;
            app.manage(KeepAlive::<R> {
                #[cfg(target_os = "android")]
                handle,
                _r: std::marker::PhantomData,
            });
            Ok(())
        })
        .build()
}
