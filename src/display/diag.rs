//! Bench only (`panel-debug`): talk to the CO5300 directly from the host, to
//! tell a panel that does not answer from one that answers but stays dark.
//!
//! The normal driver only writes, so a dead panel and a live one look the
//! same to it. This adds what it lacks:
//!   * register reads (`0x03` read header, single access, slow clock), with
//!     the line mode, dummy cycles, divider and access type selectable,
//!     because which data line the panel answers on is part of the question;
//!   * TE sampling: the panel's tearing-effect output toggles once per frame
//!     only while its timing controller runs (after sleep-out, with tear-on);
//!   * raw commands, a reset + re-init, and a dump of the LCDC/GPIO registers.
//!
//! The link task posts an [`Op`] and waits for the [`Reply`]; the app task,
//! which owns the panel, runs it between frames (`app::run`).
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use sifli_hal::lcdc::Error;
use sifli_hal::pac;
use sifli_hal::pac::lcdc::vals::{SingleAccessType, SpiAccessLen, SpiLineMode, SpiRdMode};

use super::{qspi_header, Co5300, CMD_DISPLAY_ON};

/// The `0x03` read prefix (see `QSPI_WRITE_CMD`).
const QSPI_READ: u32 = 0x03 << 24;
/// TE: LCDC1 TE is PA2.
const TE_BIT: u32 = 1 << 2;

#[derive(Clone, Copy)]
pub enum Op {
    /// Read `len` (1..=4) bytes of register `reg`.
    Read { reg: u8, len: u8, line: u8, dummy: u8, div: u8, data: bool },
    /// Write `reg` with the first `n` bytes of `params`.
    Cmd { reg: u8, params: [u8; 4], n: u8 },
    /// Count TE edges for `ms` milliseconds.
    Te { ms: u32 },
    /// Pulse reset (`low_ms` low), rerun the init sequence and turn the display on.
    Init { low_ms: u32 },
    /// Raw LCDC and GPIO registers, then the PA2 (TE) and PA9 (reset) pads.
    Regs,
}

#[derive(Clone, Copy, Default)]
pub struct Reply {
    /// 0 ok, 1 no panel (init failed at boot), 2 LCDC timeout, 3 bad parameter.
    pub err: u8,
    pub n: u8,
    pub vals: [u32; 10],
}

impl Reply {
    fn ok(vals: &[u32]) -> Self {
        let mut r = Self { n: vals.len() as u8, ..Self::default() };
        r.vals[..vals.len()].copy_from_slice(vals);
        r
    }
    fn err(err: u8) -> Self {
        Self { err, ..Self::default() }
    }
}

pub static REQUEST: Signal<CriticalSectionRawMutex, Op> = Signal::new();
pub static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static REPLY: Signal<CriticalSectionRawMutex, Reply> = Signal::new();

/// Called by the link task: run `op` on the app task, or time out.
pub async fn request(op: Op) -> Option<Reply> {
    REPLY.reset();
    REQUEST.signal(op);
    WAKE.signal(());
    embassy_time::with_timeout(Duration::from_millis(2500), REPLY.wait())
        .await
        .ok()
}

/// Called by the app task with whatever panel it holds.
pub async fn run(display: Option<&mut Co5300>, op: Op) -> Reply {
    if let Op::Regs = op {
        return Reply::ok(&regs());
    }
    if let Op::Te { ms } = op {
        return te(ms.clamp(1, 300));
    }
    let Some(panel) = display else {
        return Reply::err(1);
    };
    let result = match op {
        Op::Read { reg, len, line, dummy, div, data } => {
            if !(1..=4).contains(&len) || line > 6 || line == 3 {
                return Reply::err(3);
            }
            panel.read_reg(reg, len, line, dummy, div, data).map(|v| Reply::ok(&v))
        }
        Op::Cmd { reg, params, n } => panel
            .cmd(reg, &params[..usize::from(n.min(4))])
            .map(|()| Reply::ok(&[])),
        Op::Init { low_ms } => panel.reinit(low_ms.clamp(1, 1000)).await.map(|()| Reply::ok(&[])),
        Op::Regs | Op::Te { .. } => unreachable!(),
    };
    result.unwrap_or_else(|_| Reply::err(2))
}

fn regs() -> [u32; 10] {
    let l = pac::LCDC1;
    let g = pac::HPSYS_GPIO;
    [
        l.lcd_conf().read().0,
        l.spi_if_conf().read().0,
        l.status().read().0,
        l.lcd_if_conf().read().0,
        l.te_conf().read().0,
        g.dir0().read().0,
        g.dor0().read().0,
        g.doer0().read().0,
        pac::HPSYS_PINMUX.pad_pa0_38(2).read().0,
        pac::HPSYS_PINMUX.pad_pa0_38(9).read().0,
    ]
}

/// Busy-sample PA2: [rising edges, falling edges, samples, level at the end].
fn te(ms: u32) -> Reply {
    let g = pac::HPSYS_GPIO;
    let end = Instant::now() + Duration::from_millis(u64::from(ms));
    let mut last = g.dir0().read().0 & TE_BIT;
    let (mut rise, mut fall, mut samples) = (0u32, 0u32, 0u32);
    while Instant::now() < end {
        let now = g.dir0().read().0 & TE_BIT;
        samples += 1;
        if now != last {
            if now != 0 { rise += 1 } else { fall += 1 }
            last = now;
        }
    }
    Reply::ok(&[rise, fall, samples, u32::from(last != 0)])
}

fn wait_idle() -> Result<(), Error> {
    let l = pac::LCDC1;
    let end = Instant::now() + Duration::from_millis(100);
    while l.status().read().lcd_busy() || l.lcd_single().read().lcd_busy() {
        if Instant::now() > end {
            return Err(Error::Timeout);
        }
    }
    Ok(())
}

impl Co5300 {
    /// One single-access read: [value, divider used, SPI_IF_CONF during the read].
    fn read_reg(&mut self, reg: u8, len: u8, line: u8, dummy: u8, div: u8, data: bool) -> Result<[u32; 3], Error> {
        let l = pac::LCDC1;
        wait_idle()?;
        let saved = l.spi_if_conf().read();
        // Default: the write clock slowed 25x (50 MHz -> 2 MHz); the panel's
        // read cycle is far slower than its write cycle.
        let div = if div == 0 { saved.clk_div().saturating_mul(25).max(2) } else { div.max(2) };
        let len_val = [SpiAccessLen::Bytes1, SpiAccessLen::Bytes2, SpiAccessLen::Bytes3, SpiAccessLen::Bytes4][usize::from(len - 1)];
        l.spi_if_conf().modify(|w| {
            w.set_clk_div(div);
            w.set_line(SpiLineMode::from_bits(line));
            w.set_dummy_cycle(dummy);
            w.set_spi_rd_mode(SpiRdMode::Normal);
            w.set_wr_len(SpiAccessLen::Bytes4);
            w.set_rd_len(len_val);
            w.set_spi_cs_auto_dis(true);
        });
        let during = l.spi_if_conf().read().0;
        l.lcd_wr().write(|w| w.set_data(qspi_header(QSPI_READ, reg)));
        l.lcd_single().write(|w| {
            w.set_rd_trig(true);
            w.set_type_(if data { SingleAccessType::Data } else { SingleAccessType::Command });
        });
        let done = wait_idle();
        let value = l.lcd_rd().read().data();
        l.spi_if_conf().write_value(saved);
        done?;
        Ok([value, u32::from(div), during])
    }

    async fn reinit(&mut self, low_ms: u32) -> Result<(), Error> {
        self.reset.set_low();
        Timer::after_millis(u64::from(low_ms)).await;
        self.reset.set_high();
        Timer::after_millis(50).await;
        self.init_sequence().await?;
        self.cmd(CMD_DISPLAY_ON, &[])?;
        Timer::after_millis(70).await;
        Ok(())
    }
}
