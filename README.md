# ESP32-C5 LP core (ULP) demo

After reading [this](https://www.xda-developers.com/esp32-secret-processor-works-while-everything-else-sleeps/),
I looked it up and the ESP-C5 I have seems to have an low power (LP) core too.

So I made this little Embassy program that loads Rust firmware into the ESP32-C5's LP core
and make it do something. The LP core
- cycles the WS2812B on GPIO27 through a colour wheel and
- increments a counter once per second.

The high power (HP) core only prints that counter; it never writes it.

Run with `cargo run --release` (uses probe-rs).

Runs well on my ESP32-C5-A-N8R4 dev kit from WeAct.

## Files

- `src/main.rs`: HP app. Sets up RMT channel 0 for the LED (20 MHz tick),
  sends one "off" frame, starts the LP core, prints `ULP counter: N` every
  second, or the LP core's `mcause`/`mepc` if it trapped.

LP core:

- `src/lp_core.rs`: LP core driver (esp-hal 1.2 has none for the C5). Opens
  the APM, selects the LP clock, copies the image into LP SRAM, resets and
  wakes the LP core, reads the shared variables.
- `build.rs`: builds `lp/` with a nested `cargo build`, flattens the ELF into
  `OUT_DIR/lp_core.bin`, and writes the address of every `ULP_*` symbol to
  `OUT_DIR/lp_symbols.rs`.
- `lp/src/main.rs`: LP firmware. Startup assembly, main loop, trap handler,
  WS2812 frames written straight into the HP RMT's channel RAM.
- `lp/lp.x`: LP memory layout.

## Implementation notes

- **Memory:** 16 KiB LP SRAM at `0x5000_0000`. The LP core boots at
  `0x5000_0080`. The first 0x80 bytes are a `nop` sled (as in esp-lp-hal),
  so a boot at `0x5000_0000` also works. The trap table has its own
  256-byte-aligned section, because `mtvec` requires that alignment.
- **No `.bss` init on the LP side:** the HP core zeroes all of LP SRAM before
  copying the image.
- **Shared variables:** `#[no_mangle]` statics named `ULP_*` in the LP
  firmware. The HP side reaches them through the generated
  `shared::ULP_*` addresses.
- **Timing:** LP_FAST_CLK is switched to XTAL_D2 (ESP-IDF's default on the
  C5). The HP core stores `xtal / 2` in `ULP_CYCLES_PER_SEC`. The LP core
  runs 50 frames/s on absolute `mcycle` deadlines, so it doesn't drift.
- **LED:** the LP core is too slow to bit-bang WS2812. The HP core configures
  RMT channel 0 once, and the LP core fills its RAM (`0x6000_6400`) and
  starts each transmission. One frame takes about 0.31 ms.

## Pitfalls

- **APM:** after reset only the HP CPU is in TEE mode. The LP core starts in
  REE2 and is denied everything, even fetching its first instruction. The
  symptom: the PMU reports the LP core awake, but it never runs.
  `open_access_permissions()` does what ESP-IDF's `bootloader_init_mem()`
  does, with register addresses from ESP-IDF, because the PAC puts `LP_APM0`
  at `LP_APM`'s address.
- **LP-domain registers survive probe-rs resets:** only a power cycle clears
  them. So registers such as `LPPERI.cpu` (LP debug-bus enable) are set
  explicitly instead of trusting their reset values.
- **Nested cargo in `build.rs`:** strip `CARGO_*`, `__CARGO*` and
  `RUSTFLAGS`. rust-analyzer sets
  `__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS=nightly`. If the nested
  build inherits it, it enables `[unstable] build-std` from
  `.cargo/config.toml` and fails, and rust-analyzer then reports
  "`OUT_DIR` not set".
- **Keep the RMT channel alive:** the HP must not drop `_led_channel` while
  the LP core uses it.
- **Sampling artifact:** the HP ticker and the LP frames share the crystal,
  so they stay phase-locked. HP samples therefore always land at the same
  point in the LP loop, which is not a bug.

## AI Disclosure

Claude Code created most of this project. I would have never figured out the
RISC-V assembler code it created. It also did a good job debugging the access
issues the LP core had initially (see src/lp_cores.rs line 156-160).
