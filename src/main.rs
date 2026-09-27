//! Starts the ESP32-C5's LP core ("ULP") and watches it work.
//!
//! The LP core firmware (see `lp/`) cycles the WS2812B on GPIO27 through a
//! colour wheel and counts seconds. The HP core only sets up the RMT channel
//! for the LED, loads the LP firmware, and then prints the LP core's counter
//! once a second.

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

mod lp_core;

use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker};
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::Level;
use esp_hal::gpio::interconnect::PeripheralOutput;
use esp_hal::peripherals::RMT;
use esp_hal::rmt::{Channel, PulseCode, Rmt, Tx, TxChannelConfig, TxChannelCreator};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use lp_core::LpCore;
use panic_rtt_target as _;

// This creates a default app-descriptor required by the esp-idf bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    rtt_target::rtt_init_defmt!();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    info!("Embassy initialized!");

    // The LP core sends the LED frames through this channel, so it must stay
    // alive (and configured) for as long as the LP core runs.
    let _led_channel = ws2812_channel(peripherals.RMT, peripherals.GPIO27);

    let lp_core = LpCore::start(peripherals.LP_CORE);
    info!("LP core started with a {} byte image", lp_core::IMAGE.len());

    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        ticker.next().await;
        match lp_core.trap() {
            None => info!("ULP counter: {}", lp_core.counter()),
            Some(trap) => error!("LP core trapped: {}", trap),
        }
    }
}

/// Configures RMT channel 0 to drive a WS2812B on `pin` with a 20 MHz tick
/// (the timing the LP firmware's pulse codes assume) and switches the LED off.
///
/// Sending one frame from here also leaves the channel's transmit settings in
/// the state the LP firmware expects when it restarts transmissions itself.
fn ws2812_channel<'d>(rmt: RMT<'d>, pin: impl PeripheralOutput<'d>) -> Channel<'d, Blocking, Tx> {
    let rmt = Rmt::new(rmt, Rate::from_mhz(80)).expect("Failed to initialize RMT");
    let channel = rmt
        .channel0
        .configure_tx(
            &TxChannelConfig::default()
                .with_clk_divider(4)
                .with_idle_output(true)
                .with_idle_output_level(Level::Low),
        )
        .expect("Failed to configure RMT TX channel")
        .with_pin(pin);

    // 24 zero bits (LED off), then the reset/latch pulse that ends the frame.
    let mut off = [PulseCode::new(Level::High, 8, Level::Low, 17); 25];
    off[24] = PulseCode::new(Level::Low, 5600, Level::Low, 0);

    channel
        .transmit(&off)
        .and_then(|transaction| transaction.wait())
        .map_err(|(e, _)| e)
        .expect("WS2812 transmit failed")
}
