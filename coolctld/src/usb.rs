//! LM360 USB device: connect/claim, raw frame writes, and the fixed opaque commands.
//! Ported from the validated Python reference (`/usr/local/bin/deepcool-lm`).

use std::time::Duration;

use rusb::{DeviceHandle, GlobalContext};

use crate::protocol;

const VENDOR_ID: u16 = 0x3633;
const PRODUCT_ID: u16 = 0x0026;
const EP_OUT: u8 = 0x01;
const INTERFACE: u8 = 0;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Lm360 {
    handle: DeviceHandle<GlobalContext>,
}

impl Lm360 {
    /// Find the LM360 by VID/PID, detach any kernel driver, and claim the interface.
    pub fn connect() -> rusb::Result<Self> {
        for device in rusb::devices()?.iter() {
            let desc = device.device_descriptor()?;
            if desc.vendor_id() != VENDOR_ID || desc.product_id() != PRODUCT_ID {
                continue;
            }

            let handle = device.open()?;

            if handle.kernel_driver_active(INTERFACE).unwrap_or(false) {
                // Best-effort: some platforms don't need or allow this.
                let _ = handle.detach_kernel_driver(INTERFACE);
            }

            let config = device.config_descriptor(0)?;
            handle.set_active_configuration(config.number())?;
            handle.claim_interface(INTERFACE)?;

            return Ok(Self { handle });
        }
        Err(rusb::Error::NoDevice)
    }

    /// Repeatedly attempts to connect until it succeeds, sleeping `retry_interval`
    /// between attempts. Used to recover from a physical USB disconnect without
    /// needing a manual daemon restart.
    pub fn connect_retrying(retry_interval: Duration) -> Self {
        loop {
            match Self::connect() {
                Ok(device) => return device,
                Err(_) => std::thread::sleep(retry_interval),
            }
        }
    }

    fn write(&self, data: &[u8]) -> rusb::Result<usize> {
        self.handle.write_bulk(EP_OUT, data, WRITE_TIMEOUT)
    }

    /// Send a full frame: 13-byte header, then the 320x240 RGB565 framebuffer (153,600 bytes).
    pub fn send_frame(&self, framebuffer: &[u8]) -> rusb::Result<()> {
        self.write(&protocol::FRAME_HEADER)?;
        self.write(framebuffer)?;
        Ok(())
    }

    pub fn init_query(&self) -> rusb::Result<()> {
        self.write(&protocol::INIT_QUERY)?;
        Ok(())
    }

    pub fn brightness_up(&self) -> rusb::Result<()> {
        self.write(&protocol::BRIGHTNESS_UP)?;
        Ok(())
    }

    pub fn brightness_down(&self) -> rusb::Result<()> {
        self.write(&protocol::BRIGHTNESS_DOWN)?;
        Ok(())
    }

    pub fn zen_mode_toggle(&self) -> rusb::Result<()> {
        self.write(&protocol::ZEN_MODE_TOGGLE)?;
        Ok(())
    }
}

impl Drop for Lm360 {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(INTERFACE);
    }
}
