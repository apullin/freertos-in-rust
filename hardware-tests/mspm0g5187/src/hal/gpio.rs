//! Safe GPIO operations for the three LP-MSPM0G5187 RGB LED channels.

use crate::pac::gpio::{
    self, iomux, pincm, pins, pwren, regs, GPIOA_BASE, GPIOB_BASE, IOMUX_BASE, IOMUX_PINCM_OFFSET,
};

#[derive(Clone, Copy)]
pub enum Port {
    A,
    B,
}

impl Port {
    #[inline(always)]
    const fn base(self) -> u32 {
        match self {
            Self::A => GPIOA_BASE,
            Self::B => GPIOB_BASE,
        }
    }
}

pub fn enable_power() {
    gpio::write_reg(GPIOA_BASE, regs::GPRCM_PWREN, pwren::KEY | pwren::ENABLE);
    gpio::write_reg(GPIOB_BASE, regs::GPRCM_PWREN, pwren::KEY | pwren::ENABLE);
    cortex_m::asm::delay(1000);
}

#[inline]
fn init_digital_output(port: Port, pin_mask: u32, pincm_index: u32) {
    let offset = IOMUX_PINCM_OFFSET + pincm_index * 4;
    gpio::write_reg(IOMUX_BASE, offset, iomux::PC_CONNECTED | iomux::GPIO_FUNC);
    gpio::write_reg(port.base(), regs::DOESET31_0, pin_mask);
}

pub fn init_pa17_output() {
    init_digital_output(Port::A, pins::PIN17, pincm::PA17);
}

pub fn init_pa24_output() {
    init_digital_output(Port::A, pins::PIN24, pincm::PA24);
}

pub fn init_pb13_output() {
    init_digital_output(Port::B, pins::PIN13, pincm::PB13);
}

#[inline(always)]
pub fn set_high(port: Port, pin_mask: u32) {
    gpio::write_reg(port.base(), regs::DOUTSET31_0, pin_mask);
}

#[inline(always)]
pub fn set_low(port: Port, pin_mask: u32) {
    gpio::write_reg(port.base(), regs::DOUTCLR31_0, pin_mask);
}

pub use pins::{PIN13, PIN17, PIN24};
