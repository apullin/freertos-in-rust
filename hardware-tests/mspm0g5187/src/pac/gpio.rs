//! GPIO and IOMUX register definitions for the MSPM0G5187.
//!
//! These addresses and offsets were recovered from the hardware-tested
//! application, whose definitions were based on TI's MSPM0 SDK headers.

use core::ptr::write_volatile;

pub const GPIOA_BASE: u32 = 0x400A_0000;
pub const GPIOB_BASE: u32 = 0x400A_2000;
pub const IOMUX_BASE: u32 = 0x4042_8000;
pub const IOMUX_PINCM_OFFSET: u32 = 0x4;

pub mod iomux {
    pub const PC_CONNECTED: u32 = 0x0000_0080;
    pub const GPIO_FUNC: u32 = 0x0000_0001;
}

pub mod pincm {
    pub const PA17: u32 = 38; // PINCM39
    pub const PA24: u32 = 53; // PINCM54
    pub const PB13: u32 = 29; // PINCM30
}

pub mod regs {
    pub const GPRCM_PWREN: u32 = 0x0800;
    pub const DOUTSET31_0: u32 = 0x1290;
    pub const DOUTCLR31_0: u32 = 0x12A0;
    pub const DOESET31_0: u32 = 0x12D0;
}

pub mod pins {
    pub const PIN13: u32 = 1 << 13;
    pub const PIN17: u32 = 1 << 17;
    pub const PIN24: u32 = 1 << 24;
}

pub mod pwren {
    pub const KEY: u32 = 0x2600_0000;
    pub const ENABLE: u32 = 0x0000_0001;
}

#[inline(always)]
pub fn write_reg(base: u32, offset: u32, value: u32) {
    // Safety: callers use only the fixed MSPM0G5187 MMIO bases and aligned
    // 32-bit register offsets defined in this module.
    unsafe { write_volatile((base + offset) as *mut u32, value) };
}
