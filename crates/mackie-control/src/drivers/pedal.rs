//! The two USB pedals, through `xgodev/pedal-drivers`. What they have in common: they are often unplugged.
//!
//! A pedal that is not connected must not bring the bridge down, and must be picked up again once it is
//! plugged in -- so the connection is opened on first use, dropped on any failure, and tried again on the
//! next operation.

use hotone_ampero2::device::Ampero;
use hotone_ampero2::{patch, protocol as amp};
use mvave_mk300::device::MVave;
use mvave_mk300::mk300::OFF_VOL;
use mvave_mk300::protocol::SPACE_PRESET;
use pedal_core::midi::Transport;

use super::{Driver, Result, Unsupported};

pub type Connect<C> = Box<dyn FnMut() -> std::result::Result<C, String> + Send>;

struct Pedal<C> {
    label: &'static str,
    connect: Connect<C>,
    client: Option<C>,
}

impl<C> Pedal<C> {
    fn new(label: &'static str, connect: Connect<C>) -> Self {
        Pedal { label, connect, client: None }
    }

    fn with<T>(&mut self, operation: impl FnOnce(&mut C) -> std::result::Result<T, String>) -> Result<T> {
        if self.client.is_none() {
            let c = (self.connect)().map_err(|e| Unsupported(format!("{} is not connected ({e})", self.label)))?;
            self.client = Some(c);
        }
        let result = operation(self.client.as_mut().expect("connected above"));
        result.map_err(|e| {
            self.client = None; // try a fresh connection next time
            Unsupported(format!("{}: {e}", self.label))
        })
    }
}

fn to_percent(value: f64) -> u16 {
    (value.clamp(0.0, 1.0) * 100.0).round() as u16
}

// -- M-VAVE MK-300 ------------------------------------------------------------------------------------------

/// The few calls the driver needs from the MK-300.
pub trait Mk300Client: Send {
    fn volume(&mut self) -> std::result::Result<f64, String>;
    fn set_volume(&mut self, v: u16) -> std::result::Result<(), String>;
    fn preset_names(&mut self) -> std::result::Result<Vec<String>, String>;
    fn load_preset(&mut self, index: usize) -> std::result::Result<(), String>;
    fn current(&mut self) -> std::result::Result<usize, String>;
}

impl<T: Transport + Send + 'static> Mk300Client for MVave<T> {
    fn volume(&mut self) -> std::result::Result<f64, String> {
        Ok(f64::from(self.read_preset().map_err(|e| e.to_string())?.volume()))
    }

    fn set_volume(&mut self, v: u16) -> std::result::Result<(), String> {
        self.set_u16(OFF_VOL as u32, i64::from(v), SPACE_PRESET).map_err(|e| e.to_string())
    }

    fn preset_names(&mut self) -> std::result::Result<Vec<String>, String> {
        MVave::preset_names(self).map_err(|e| e.to_string())
    }

    fn load_preset(&mut self, index: usize) -> std::result::Result<(), String> {
        MVave::load_preset(self, index as u32).map_err(|e| e.to_string())
    }

    fn current(&mut self) -> std::result::Result<usize, String> {
        self.current_preset_index().map(usize::from).map_err(|e| e.to_string())
    }
}

/// M-VAVE MK-300 over USB SysEx.
///
/// Everything here is read back from the pedal's edit buffer, so a fader on it takes over like any other.
/// The presets (160) are its scenes.
pub struct Mk300 {
    pedal: Pedal<Box<dyn Mk300Client>>,
    /// Reading the 160 names takes a second on the pedal (measured 2026-09-22): read them once, not on
    /// every arrow press.
    names: Option<Vec<String>>,
}

impl Mk300 {
    pub fn new() -> Self {
        Self::with(Box::new(|| {
            MVave::open(None).map(|d| Box::new(d) as Box<dyn Mk300Client>).map_err(|e| e.to_string())
        }))
    }

    pub fn with(connect: Connect<Box<dyn Mk300Client>>) -> Self {
        Mk300 { pedal: Pedal::new("MK-300", connect), names: None }
    }
}

impl Default for Mk300 {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver for Mk300 {
    fn name(&self) -> &str {
        "mk300"
    }

    fn read(&mut self, _target: &str) -> Result<Option<f64>> {
        self.pedal.with(|c| c.volume()).map(|v| Some(v / 100.0))
    }

    fn write(&mut self, _target: &str, value: f64) -> Result<()> {
        let v = to_percent(value);
        self.pedal.with(|c| c.set_volume(v))
    }

    fn scenes(&mut self) -> Result<Vec<String>> {
        if self.names.is_none() {
            let names = self.pedal.with(|c| c.preset_names())?;
            self.names = Some(names.iter().map(|n| n.trim().to_string()).collect());
        }
        Ok(self.names.clone().unwrap_or_default())
    }

    fn current_scene(&mut self) -> Result<Option<usize>> {
        self.pedal.with(|c| c.current()).map(Some)
    }

    fn load_scene(&mut self, index: usize) -> Result<()> {
        self.scenes()?;
        self.pedal.with(|c| c.load_preset(index))
    }
}

// -- Hotone Ampero II Stage ---------------------------------------------------------------------------------

pub trait AmperoClient: Send {
    fn set_volume(&mut self, v: u16) -> std::result::Result<(), String>;
    fn patch_names(&mut self) -> std::result::Result<Vec<String>, String>;
    fn load_patch(&mut self, index: usize) -> std::result::Result<(), String>;
    fn current(&mut self) -> std::result::Result<usize, String>;
}

impl<T: Transport + Send + 'static> AmperoClient for Ampero<T> {
    fn set_volume(&mut self, v: u16) -> std::result::Result<(), String> {
        self.send(&amp::msg_set_patch_volume(v)).map_err(|e| e.to_string())
    }

    fn patch_names(&mut self) -> std::result::Result<Vec<String>, String> {
        let query = amp::msg_query_inventory("patches").map_err(|e| e.to_string())?;
        let dump = self.request_dump(&query).map_err(|e| e.to_string())?;
        Ok(patch::patch_names(&patch::decode_reply(&dump).map_err(|e| e.to_string())?))
    }

    fn load_patch(&mut self, index: usize) -> std::result::Result<(), String> {
        self.send(&amp::msg_load_patch(index as u32)).map_err(|e| e.to_string())
    }

    fn current(&mut self) -> std::result::Result<usize, String> {
        self.current_patch().map(|i| i as usize).map_err(|e| e.to_string())
    }
}

/// Hotone Ampero II Stage over USB SysEx.
///
/// The patch volume can be written but not read -- the protocol has no query for it. So `read` answers
/// None until the driver has written a value (the bridge then applies a fader at once instead of waiting
/// for takeover), and forgets it when a patch loads, because the volume belongs to the patch. The patches
/// (300) are its scenes.
pub struct Ampero2 {
    pedal: Pedal<Box<dyn AmperoClient>>,
    written: Option<f64>,
    /// 300 names: ask once, not per press.
    names: Option<Vec<String>>,
}

impl Ampero2 {
    pub fn new() -> Self {
        Self::with(Box::new(|| {
            Ampero::open(None).map(|d| Box::new(d) as Box<dyn AmperoClient>).map_err(|e| e.to_string())
        }))
    }

    pub fn with(connect: Connect<Box<dyn AmperoClient>>) -> Self {
        Ampero2 { pedal: Pedal::new("Ampero II", connect), written: None, names: None }
    }
}

impl Default for Ampero2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver for Ampero2 {
    fn name(&self) -> &str {
        "ampero2"
    }

    fn read(&mut self, _target: &str) -> Result<Option<f64>> {
        Ok(self.written)
    }

    fn write(&mut self, _target: &str, value: f64) -> Result<()> {
        let v = to_percent(value);
        self.pedal.with(|c| c.set_volume(v))?;
        self.written = Some(f64::from(v) / 100.0);
        Ok(())
    }

    fn scenes(&mut self) -> Result<Vec<String>> {
        if self.names.is_none() {
            self.names = Some(self.pedal.with(|c| c.patch_names())?);
        }
        Ok(self.names.clone().unwrap_or_default())
    }

    fn current_scene(&mut self) -> Result<Option<usize>> {
        self.pedal.with(|c| c.current()).map(Some)
    }

    fn load_scene(&mut self, index: usize) -> Result<()> {
        self.scenes()?;
        self.pedal.with(|c| c.load_patch(index))?;
        self.written = None;
        Ok(())
    }
}
