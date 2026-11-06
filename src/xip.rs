//! cortex-m-rt's `__pre_init`: set the stack limit (what the HAL's
//! `set-msplim` feature did, which Cargo.toml turns off so this crate owns
//! the hook).
//!
//! Every board executes in place from the external NOR. Rev A (the
//! BodyV6.13 engineering sample, never shipped) once copied its code and
//! fonts to PSRAM here to keep NOR fetches out of its speaker amplifier;
//! that is gone, and PSRAM now holds only zeroed runtime tables
//! (`.psram_bss`, psram.x).

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn __pre_init() {
    core::arch::naked_asm!("ldr r0, =_stack_end", "msr MSPLIM, r0", "bx lr");
}
