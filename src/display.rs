//! CO5300 AMOLED panel over the SF32's LCDC in quad-SPI mode.
//!
//! This talks to `sifli-hal`'s LCDC directly rather than going through the
//! `display-driver` / `display-driver-co5300` crates. Those are what upstream's
//! `lcdc_eg_co5300` example uses, but that example does not build: it imports
//! `display_driver::bus::QspiFlashBus`, which does not exist in the
//! `display-driver 0.1.1` its own lockfile pins. Rather than guess at the bus
//! contract between two crates that are out of step, the panel is driven from
//! the sequence LilyGo actually ships
//! (`customer/peripherals/co5300_t_sf32/co5300.c`).
//!
//! The split of work is:
//!   * the LCDC hardware frames quad-SPI transactions (the `0x02` command and
//!     `0x32` pixel prefixes) and DMAs pixels out of a layer buffer;
//!   * this module sends the panel's own MIPI-DCS commands, and sets the
//!     address window before each pixel transfer -- `send_pixel_data` sets the
//!     *controller's* ROI, not the panel's, so both have to agree or the image
//!     lands in the wrong place.

use embassy_time::Timer;
use sifli_hal::gpio::Output;
use sifli_hal::lcdc::{Error, Lcdc, Spi};
use sifli_hal::peripherals::LCDC1;

use crate::logln;
use crate::ui::damage::Region;
use embedded_graphics::prelude::Dimensions;

mod framebuffer;
#[cfg(feature = "panel-debug")]
pub mod diag;
// Glob: shared `ui`/`deck` code sees the whole framebuffer API as
// `crate::display`, exactly as the consumers that mount framebuffer.rs directly.
pub use framebuffer::*;

// MIPI-DCS / CO5300 commands used here.
const CMD_PAGE: u8 = 0xFE;
const CMD_SPI_MODE: u8 = 0xC4;
const CMD_PIXEL_FORMAT: u8 = 0x3A;
const CMD_TEAR_ON: u8 = 0x35;
const CMD_CTRL_DISPLAY: u8 = 0x53;
const CMD_BRIGHTNESS: u8 = 0x51;
const CMD_HBM_BRIGHTNESS: u8 = 0x63;
const CMD_CASET: u8 = 0x2A;
const CMD_RASET: u8 = 0x2B;
const CMD_SLEEP_OUT: u8 = 0x11;
const CMD_SLEEP_IN: u8 = 0x10;
const CMD_DISPLAY_ON: u8 = 0x29;
const CMD_DISPLAY_OFF: u8 = 0x28;

const CMD_WRITE_RAM: u8 = 0x2C;

const PIXEL_FORMAT_RGB565: u8 = 0x55;

/// Quad-SPI transaction prefixes.
///
/// This panel has no D/C pin: a transaction opens with a one-byte opcode and a
/// 24-bit address, and the command number rides in the middle of that address.
/// So every transfer is a 32-bit header of `[prefix, 0x00, cmd, 0x00]` --
/// `0x02` to write a register, `0x32` to write pixels, `0x03` to read.
///
/// The LCDC does *not* synthesise this; the caller composes the word and sends
/// it four bytes wide. Sending a bare command byte instead is silently
/// accepted by the controller and understood by nothing: transfers complete,
/// report success, and the panel stays asleep and black.
const QSPI_WRITE_CMD: u32 = 0x02 << 24;
const QSPI_WRITE_PIXELS: u32 = 0x32 << 24;

const fn qspi_header(prefix: u32, cmd: u8) -> u32 {
    prefix | ((cmd as u32) << 8)
}

/// The panel's supply enables, held for as long as the display is in use.
/// Nothing reads them back: dropping an `Output` would release the pin and
/// cut the rail, so the struct exists to keep them alive.
#[allow(dead_code)]
pub struct PanelRails {
    /// The AMOLED supply: `LCD_VADD_EN` on the dev board, U8's PANEL_4V
    /// enable (PA10) on Rev A.
    pub power: Output<'static>,
    /// Rev A only: U10's PANEL_LOGIC switch (PA11), the panel's VCI rail. The
    /// dev board's panel logic supply is always on.
    pub logic: Option<Output<'static>>,
}

pub struct Co5300 {
    lcdc: Lcdc<'static, LCDC1, Spi>,
    reset: Output<'static>,
    rails: PanelRails,
}

impl Co5300 {
    /// Reset the panel and run LilyGo's initialisation sequence.
    pub async fn new(
        lcdc: Lcdc<'static, LCDC1, Spi>,
        mut reset: Output<'static>,
        rails: PanelRails,
    ) -> Result<Self, Error> {
        // Allow the newly enabled panel supplies to settle with reset asserted.
        reset.set_low();
        Timer::after_millis(10).await;

        // Active-low, with the panel given time to settle before and after.
        reset.set_high();
        Timer::after_millis(10).await;
        reset.set_low();
        Timer::after_millis(10).await;
        reset.set_high();
        Timer::after_millis(50).await;

        let mut panel = Self {
            lcdc,
            reset,
            rails,
        };

        panel.init_sequence().await?;

        logln!(
            "display: CO5300 initialised ({}x{}, rotation={}deg)",
            WIDTH,
            HEIGHT,
            if crate::orientation::ROTATE_180 {
                180
            } else {
                0
            }
        );
        Ok(panel)
    }

    /// LilyGo's register sequence, from the page unlock to sleep-out.
    async fn init_sequence(&mut self) -> Result<(), Error> {
        let panel = self;
        // Unlock the command page. LilyGo passes 0x00 here; the SiFli reference
        // panel wants 0x20, and sending the wrong one leaves every command
        // after it writing into the wrong register page.
        panel.cmd(CMD_PAGE, &[0x00])?;
        panel.cmd(CMD_SPI_MODE, &[0x80])?;
        panel.cmd(CMD_PIXEL_FORMAT, &[PIXEL_FORMAT_RGB565])?;
        panel.cmd(CMD_TEAR_ON, &[0x00])?;
        panel.cmd(CMD_CTRL_DISPLAY, &[0x20])?;
        panel.cmd(CMD_BRIGHTNESS, &[0xFF])?;
        panel.cmd(CMD_HBM_BRIGHTNESS, &[0xFF])?;

        panel.set_window(0, 0, WIDTH - 1, HEIGHT - 1)?;

        panel.cmd(CMD_SLEEP_OUT, &[])?;
        Timer::after_millis(60).await;
        // The caller loads the complete splash before making GRAM visible.

        Ok(())
    }

    pub async fn show(&mut self) -> Result<(), Error> {
        self.cmd(CMD_DISPLAY_ON, &[])?;
        Timer::after_millis(70).await;
        Ok(())
    }

    /// Panel brightness; independent of the host's key-light configuration.
    pub fn set_brightness(&mut self, level: u8) -> Result<(), Error> {
        self.cmd(CMD_BRIGHTNESS, &[level])
    }

    /// The app schedules the 120ms settling intervals without blocking input.
    pub fn sleep(&mut self) -> Result<(), Error> {
        self.cmd(CMD_DISPLAY_OFF, &[])?;
        self.cmd(CMD_SLEEP_IN, &[])
    }
    pub fn wake(&mut self) -> Result<(), Error> {
        self.cmd(CMD_SLEEP_OUT, &[])
    }
    pub fn enable(&mut self) -> Result<(), Error> {
        self.cmd(CMD_DISPLAY_ON, &[])
    }

    /// Soft power-off, after `sleep()` has settled: hold the panel in reset
    /// and cut its supplies in the reverse of `board::init`'s order (the
    /// AMOLED rail first, then the logic rail). The pins stay owned, driven
    /// low, until the chip itself powers down. Rev A only: the dev board has
    /// no soft power-off.
    #[cfg_attr(not(feature = "hw-a"), allow(dead_code))]
    pub async fn power_down(&mut self) {
        self.reset.set_low();
        self.rails.power.set_low();
        Timer::after_millis(5).await;
        if let Some(logic) = self.rails.logic.as_mut() {
            logic.set_low();
        }
    }

    /// Rotate and pack a freshly drawn clip, then push it to the panel.
    /// Redraw before another call; the transfer changes the source layout.
    pub async fn write_band(&mut self, band: &mut Band) -> Result<(), Error> {
        let clip = band.bounding_box();
        if clip.size.width == 0 || clip.size.height == 0 {
            return Ok(());
        }
        let region = Region {
            x0: clip.top_left.x as u16,
            y0: clip.top_left.y as u16,
            x1: clip.top_left.x as u16 + clip.size.width as u16,
            y1: clip.top_left.y as u16 + clip.size.height as u16,
        }
        .panel();
        band.orient_for_panel();
        let pixels = band.pack_for_dma();
        self.set_window(region.x0, region.y0, region.x1 - 1, region.y1 - 1)?;

        // Open the pixel transaction, then let the layer DMA stream into it.
        // `send_pixel_data` only configures the controller's ROI and starts the
        // transfer -- the memory-write command in front of it is ours to send,
        // with chip-select held so the DMA continues the same transaction.
        self.lcdc
            .send_cmd(qspi_header(QSPI_WRITE_PIXELS, CMD_WRITE_RAM), 4, true)?;
        self.lcdc
            .send_pixel_data(
                region.x0, region.y0, region.x1 - 1, region.y1 - 1, pixels,
            )
            .await
    }

    fn set_window(&mut self, x0: u16, y0: u16, x1: u16, y1: u16) -> Result<(), Error> {
        self.cmd(
            CMD_CASET,
            &[(x0 >> 8) as u8, x0 as u8, (x1 >> 8) as u8, x1 as u8],
        )?;
        self.cmd(
            CMD_RASET,
            &[(y0 >> 8) as u8, y0 as u8, (y1 >> 8) as u8, y1 as u8],
        )
    }

    /// Send one command with its parameters as a single quad-SPI transaction.
    ///
    /// The header is four bytes (see `qspi_header`); parameters then go one
    /// byte at a time with chip-select held, rather than packed into the 32-bit
    /// write register. That sidesteps having to know which end of the word the
    /// controller shifts out first.
    fn cmd(&mut self, cmd: u8, params: &[u8]) -> Result<(), Error> {
        let hold = !params.is_empty();
        self.lcdc
            .send_cmd(qspi_header(QSPI_WRITE_CMD, cmd), 4, hold)?;
        for (i, byte) in params.iter().enumerate() {
            let last = i + 1 == params.len();
            self.lcdc.send_cmd_data(*byte as u32, 1, !last)?;
        }
        Ok(())
    }
}
