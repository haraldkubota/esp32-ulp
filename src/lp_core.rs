//! Loads and starts the ESP32-C5 LP core ("ULP").
//!
//! esp-hal 1.2 only ships an LP core driver for the ESP32-C6. The C5's LP core
//! is controlled through the same PMU/LP_AON/LPPERI bits, so this follows
//! ESP-IDF's `ulp_lp_core_load_binary()` and `ulp_lp_core_run()` for the C5.
//!
//! The firmware itself lives in `lp/`; build.rs compiles it into a flat LP SRAM
//! image and exports the addresses of its `ULP_*` statics.

use esp_hal::peripherals::{LP_AON, LP_CLKRST, LP_CORE, LP_PERI, PCR, PMU};

/// Pointers to the variables shared with the LP firmware.
mod shared {
    include!(concat!(env!("OUT_DIR"), "/lp_symbols.rs"));
}

/// The LP firmware, as a flat image of LP SRAM.
pub static IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/lp_core.bin"));

const LP_RAM_START: *mut u32 = 0x5000_0000 as *mut u32;
const LP_RAM_WORDS: usize = 16 * 1024 / 4;

// PMU_LP_CPU_PWR1.WAKEUP_EN bit for "woken by the HP CPU".
const WAKEUP_SOURCE_HP_CPU: u16 = 1 << 0;

/// Why the LP core stopped running its firmware.
#[derive(Clone, Copy, defmt::Format)]
pub struct Trap {
    pub mcause: u32,
    pub mepc: u32,
}

/// The running LP core.
pub struct LpCore<'d> {
    _lp_core: LP_CORE<'d>,
}

impl<'d> LpCore<'d> {
    /// Loads the LP firmware into LP SRAM and starts it.
    pub fn start(lp_core: LP_CORE<'d>) -> Self {
        stop();
        open_access_permissions();
        load_image();

        // Clock the LP core from XTAL_D2 (half the 40/48 MHz crystal) rather
        // than the default RC_FAST oscillator, so the firmware's `mcycle`-based
        // timing is accurate. This changes LP_FAST_CLK behind esp-hal's back;
        // nothing else in this program uses it.
        LP_CLKRST::regs()
            .lp_clk_conf()
            .modify(|_, w| unsafe { w.fast_clk_sel().bits(1) }); // 1 = XTAL_D2
        let xtal_mhz = u32::from(PCR::regs().sysclk_conf().read().clk_xtal_freq().bits());
        unsafe { shared::ULP_CYCLES_PER_SEC.write_volatile(xtal_mhz * 1_000_000 / 2) };

        run();
        Self { _lp_core: lp_core }
    }

    /// The LP core's seconds counter. Only the LP core writes it.
    pub fn counter(&self) -> u32 {
        unsafe { shared::ULP_COUNTER.read_volatile() }
    }

    /// The trap that stopped the LP firmware, if any.
    pub fn trap(&self) -> Option<Trap> {
        let mepc = unsafe { shared::ULP_TRAP_MEPC.read_volatile() };
        (mepc != 0).then(|| Trap {
            mcause: unsafe { shared::ULP_TRAP_MCAUSE.read_volatile() },
            mepc,
        })
    }
}

/// Stops the LP core by disabling its wakeup sources and putting it to sleep.
fn stop() {
    let pmu = PMU::regs();
    pmu.lp_cpu_pwr1()
        .modify(|_, w| unsafe { w.lp_cpu_wakeup_en().bits(0) });
    pmu.lp_cpu_pwr1()
        .modify(|_, w| w.lp_cpu_sleep_req().set_bit());
}

/// Fills LP SRAM with the firmware image and zeroes the rest, which also
/// zeroes the firmware's `.bss`.
fn load_image() {
    let (words, _) = IMAGE.as_chunks::<4>();
    for i in 0..LP_RAM_WORDS {
        let word = words.get(i).map_or(0, |bytes| u32::from_le_bytes(*bytes));
        unsafe { LP_RAM_START.add(i).write_volatile(word) };
    }
}

/// Resets the LP core and wakes it up, so it starts the firmware from its
/// reset vector.
fn run() {
    let (pmu, lp_aon, lp_peri) = (PMU::regs(), LP_AON::regs(), LP_PERI::regs());

    // Keep the LP core off the debug bus (its reset state; ESP-IDF enables
    // it here for LP core debugging, which we don't need). This register
    // lives in the LP domain and survives the system reset probe-rs does,
    // so set it explicitly rather than rely on its reset value.
    lp_peri
        .cpu()
        .modify(|_, w| w.lpcore_dbgm_unavaliable().set_bit());

    // Reset the LP core and enable its clock.
    lp_peri
        .reset_en()
        .modify(|_, w| w.lp_cpu_reset_en().set_bit());
    lp_peri
        .reset_en()
        .modify(|_, w| w.lp_cpu_reset_en().clear_bit());
    lp_peri.clk_en().modify(|_, w| w.lp_cpu_ck_en().set_bit());

    lp_aon.lpcore().modify(|_, w| w.disable().clear_bit());

    // Serve LP SRAM from the LP bus, so the LP core can keep using it while
    // the HP system sleeps.
    lp_aon
        .lpbus()
        .modify(|_, w| w.fast_mem_mux_sel().clear_bit());
    lp_aon
        .lpbus()
        .modify(|_, w| w.fast_mem_mux_sel_update().set_bit());

    // Stall and reset the LP core when it goes to sleep, so the next wakeup
    // starts the firmware from its reset vector.
    pmu.lp_cpu_pwr0().modify(|_, w| {
        w.lp_cpu_slp_stall_en().set_bit();
        w.lp_cpu_slp_reset_en().set_bit()
    });

    pmu.lp_cpu_pwr1()
        .modify(|_, w| unsafe { w.lp_cpu_wakeup_en().bits(WAKEUP_SOURCE_HP_CPU) });
    pmu.hp_lp_cpu_comm().write(|w| w.hp_trigger_lp().set_bit());
}

// Access permission management (APM) registers, with ESP-IDF's addresses from
// soc/esp32c5/register/soc/*_reg.h. (The esp32c5 PAC puts LP_APM0 at LP_APM's
// address, and esp-hal doesn't expose LP_APM and CPU_APM at all.)
const HP_APM_FUNC_CTRL: *mut u32 = (0x6009_9000 + 0xc4) as *mut u32;
const LP_APM0_FUNC_CTRL: *mut u32 = (0x6009_9800 + 0xc4) as *mut u32;
const LP_APM_FUNC_CTRL: *mut u32 = (0x600b_3800 + 0xc4) as *mut u32;
const CPU_APM_FUNC_CTRL: *mut u32 = (0x6009_a000 + 0xc4) as *mut u32;
// One TEE_Mn_MODE_CTRL register per bus master.
const TEE_M0_MODE_CTRL: *mut u32 = 0x6009_8000 as *mut u32;
// The LP core's mode, as seen by the LP-side filters.
const LP_TEE_M0_MODE_CTRL: *mut u32 = 0x600b_3400 as *mut u32;
// APM_MASTER_MASK_ALL: the bus masters present on the ESP32-C5.
const APM_MASTERS: u32 = 0x03ce_017f;
const TEE_MODE: u32 = 0;

/// Does what ESP-IDF's `bootloader_init_mem()` does at app start-up: disables
/// the APM access filters and puts all bus masters into TEE mode.
///
/// After power-up only the HP CPU is in TEE mode. All other masters, the LP
/// core included, start in REE2 mode, and the APM filters deny them access to
/// everything. esp-hal 1.2 doesn't undo this on the ESP32-C5, so without it
/// the LP core can't even fetch its first instruction from LP SRAM, let alone
/// reach the RMT.
fn open_access_permissions() {
    for reg in [
        HP_APM_FUNC_CTRL,
        LP_APM0_FUNC_CTRL,
        LP_APM_FUNC_CTRL,
        CPU_APM_FUNC_CTRL,
    ] {
        unsafe { reg.write_volatile(0) };
    }
    for master in (0..32).filter(|m| APM_MASTERS & (1 << m) != 0) {
        unsafe { TEE_M0_MODE_CTRL.add(master).write_volatile(TEE_MODE) };
    }
    unsafe { LP_TEE_M0_MODE_CTRL.write_volatile(TEE_MODE) };
}
