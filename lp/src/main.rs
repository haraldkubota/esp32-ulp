//! Firmware for the ESP32-C5 LP core ("ULP").
//!
//! Runs from LP SRAM and does two things, without any help from the HP core:
//!
//! - increments `ULP_COUNTER` once per second. The HP core only ever reads it.
//! - animates the WS2812B on GPIO27 through a colour wheel. The HP core sets up
//!   RMT channel 0 once (clock, pin, idle level); after that every frame is
//!   written into the RMT channel RAM and started from here. The LP core is
//!   far too slow to bit-bang WS2812 timing, but it can reach all HP
//!   peripherals while the HP system is powered up.
//!
//! All `ULP_*` statics are shared with the HP core, whose build script exports
//! their addresses from this crate's ELF file.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::Relaxed;

use esp32c5::RMT;

/// Seconds since the LP core was started. Written only by the LP core.
#[unsafe(no_mangle)]
static ULP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// `mcycle` ticks per second, i.e. the LP core clock. Set by the HP core
/// before starting us.
#[unsafe(no_mangle)]
static ULP_CYCLES_PER_SEC: AtomicU32 = AtomicU32::new(0);

/// `mcause`/`mepc` of the first trap taken, for the HP core to report.
/// `ULP_TRAP_MEPC` stays 0 until then, since no code lives at address 0.
#[unsafe(no_mangle)]
static ULP_TRAP_MCAUSE: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
static ULP_TRAP_MEPC: AtomicU32 = AtomicU32::new(0);

const FRAMES_PER_SEC: u32 = 50;

/// Peak channel value of the colour wheel; WS2812Bs are painfully bright.
const BRIGHTNESS: u16 = 40;

// The LP core starts at 0x5000_0080 according to ESP-IDF. Like esp-lp-hal, the
// 0x80 bytes before it are a nop sled rather than a vector table, so that a
// start at 0x5000_0000 also ends up in `_reset`. Traps go to `_trap_table`
// instead, which (like any LP core vector table) must be 256-byte aligned.
global_asm!(
    r#"
    .section .init.vector, "ax"
    .option push
    .option norvc
    .rept 32
    nop
    .endr
    .option pop

    .section .init.reset, "ax"
    .global _reset
_reset:
    la   t0, _trap_table
    csrw mtvec, t0
    la   sp, __stack_top
    j    lp_main

    .section .trap_table, "ax"
_trap_table:
    .option push
    .option norvc
    .rept 32
    j _trap
    .endr
    .option pop
_trap:
    la   sp, __stack_top
    j    lp_trap
"#
);

#[unsafe(no_mangle)]
extern "C" fn lp_main() -> ! {
    let cycles_per_frame = ULP_CYCLES_PER_SEC.load(Relaxed) / FRAMES_PER_SEC;

    let mut hue: u8 = 0;
    let mut frame = 0;
    let mut deadline = mcycle();
    loop {
        let (r, g, b) = wheel(hue);
        ws2812_show(r, g, b, cycles_per_frame);
        hue = hue.wrapping_add(1);

        frame += 1;
        if frame == FRAMES_PER_SEC {
            frame = 0;
            ULP_COUNTER.store(ULP_COUNTER.load(Relaxed).wrapping_add(1), Relaxed);
        }

        // Absolute deadlines, so the time spent above doesn't make the
        // counter drift.
        deadline = deadline.wrapping_add(cycles_per_frame);
        while (mcycle().wrapping_sub(deadline) as i32) < 0 {}
    }
}

#[unsafe(no_mangle)]
extern "C" fn lp_trap() -> ! {
    let (mcause, mepc): (u32, u32);
    unsafe {
        asm!("csrr {0}, mcause", "csrr {1}, mepc", out(reg) mcause, out(reg) mepc);
    }
    ULP_TRAP_MCAUSE.store(mcause, Relaxed);
    ULP_TRAP_MEPC.store(mepc, Relaxed);
    idle()
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    idle()
}

fn idle() -> ! {
    loop {
        unsafe { asm!("wfi") };
    }
}

fn mcycle() -> u32 {
    let cycles;
    unsafe { asm!("csrr {0}, mcycle", out(reg) cycles, options(nomem, nostack)) };
    cycles
}

/// Maps `hue` onto a red → green → blue → red wheel.
fn wheel(hue: u8) -> (u8, u8, u8) {
    let segment = (hue / 85).min(2);
    let pos = u16::from(hue - segment * 85);
    let up = (pos * BRIGHTNESS / 85) as u8;
    let down = BRIGHTNESS as u8 - up;
    match segment {
        0 => (down, up, 0),
        1 => (0, down, up),
        _ => (up, 0, down),
    }
}

// RMT channel 0 RAM (48 pulse codes) in the HP RMT peripheral.
const RMT_CH0_RAM: *mut u32 = 0x6000_6400 as *mut u32;

// WS2812B timing in ticks of the 20 MHz RMT clock the HP core configured, as
// RMT pulse codes: bits 0..16 hold length and level of the first half, bits
// 16..32 those of the second half.
const fn pulse(high: u32, low: u32) -> u32 {
    high | 1 << 15 | low << 16
}
const BIT0: u32 = pulse(8, 17); // 0.40 us high, 0.85 us low
const BIT1: u32 = pulse(16, 9); // 0.80 us high, 0.45 us low
const RESET: u32 = 5600; // 0.28 ms low; the zero-length second half ends the transmission

/// Sends one GRB frame to the LED and waits (at most `timeout` cycles) for the
/// RMT to finish.
fn ws2812_show(r: u8, g: u8, b: u8, timeout: u32) {
    let rmt = unsafe { &*RMT::ptr() };

    let grb = (u32::from(g) << 16) | (u32::from(r) << 8) | u32::from(b);
    for i in 0..24 {
        let bit = (grb >> (23 - i)) & 1;
        let code = if bit == 1 { BIT1 } else { BIT0 };
        unsafe { RMT_CH0_RAM.add(i).write_volatile(code) };
    }
    unsafe { RMT_CH0_RAM.add(24).write_volatile(RESET) };

    // Same sequence as esp-hal's `start_tx` for channel 0.
    rmt.int_clr().write(|w| w.ch_tx_end(0).set_bit());
    rmt.ref_cnt_rst().write(|w| unsafe { w.bits(1) });
    rmt.ref_cnt_rst().write(|w| unsafe { w.bits(0) });
    rmt.ch_tx_conf0(0).modify(|_, w| {
        w.mem_rd_rst().set_bit();
        w.apb_mem_rst().set_bit();
        w.tx_start().set_bit()
    });
    rmt.ch_tx_conf0(0).modify(|_, w| w.conf_update().set_bit());

    // A frame takes about 0.31 ms.
    let start = mcycle();
    while !rmt.int_raw().read().ch_tx_end(0).bit() {
        if mcycle().wrapping_sub(start) > timeout {
            break;
        }
    }
}
