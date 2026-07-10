#![no_std]
#![no_main]
#![allow(non_snake_case, static_mut_refs)]

use core::ffi::c_void;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr;

use freertos_in_rust::kernel::tasks::{
    ulTaskNotifyTake, vTaskDelay, vTaskNotifyGiveFromISR, vTaskStartScheduler, vTaskSuspend,
    xTaskCreateStatic, xTaskGetTickCount, StaticTask_t,
};
use freertos_in_rust::port::{
    portENTER_CRITICAL, portEXIT_CRITICAL, portYIELD_FROM_ISR, ullPortReadMtime,
    uxPortGetTicklessSleepCount, vPortSetExternalInterruptHandler, xPortGetLastCompletedSleepTicks,
    xPortGetLastSuppressedTicks, xPortIsInsideInterrupt,
};
use freertos_in_rust::types::*;
use freertos_qemu_test_common::Tap;
use riscv_rt::entry;

const MTIME_ADDR: usize = 0x0200_BFF8;
const UART0_BASE: usize = 0x1001_3000;
const UART_TXDATA: usize = UART0_BASE;
const UART_TXCTRL: usize = UART0_BASE + 0x08;
const UART_IE: usize = UART0_BASE + 0x10;

const PLIC_BASE: usize = 0x0C00_0000;
const PLIC_UART0_SOURCE: u32 = 3;
const PLIC_ENABLE_MMODE_HART0: usize = PLIC_BASE + 0x2000;
const PLIC_THRESHOLD_MMODE_HART0: usize = PLIC_BASE + 0x20_0000;
const PLIC_CLAIM_MMODE_HART0: usize = PLIC_BASE + 0x20_0004;

const STACK_WORDS: usize = 256;
const PERIODIC_TEST_TICKS: TickType_t = 1_000;
const TICKLESS_TEST_TICKS: TickType_t = 100;

static mut HIGH_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut HIGH_TCB: StaticTask_t = StaticTask_t::new();
static mut HIGH_HANDLE: TaskHandle_t = ptr::null_mut();

static mut LOW_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut LOW_TCB: StaticTask_t = StaticTask_t::new();
static mut LOW_HANDLE: TaskHandle_t = ptr::null_mut();

static mut MTIME_ROLLOVER_OK: bool = false;
static mut TASK_CRITICAL_REPORTED_ISR: BaseType_t = pdTRUE;
static mut EXTERNAL_CAUSE: UBaseType_t = 0;
static mut EXTERNAL_SOURCE: u32 = 0;
static mut EXTERNAL_REPORTED_ISR: BaseType_t = pdFALSE;

struct Uart;

impl Write for Uart {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            unsafe {
                while ptr::read_volatile(UART_TXDATA as *const u32) & 0x8000_0000 != 0 {}
                ptr::write_volatile(UART_TXDATA as *mut u32, byte as u32);
            }
        }
        Ok(())
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    let mut uart = Uart;
    let _ = writeln!(uart, "not ok 1 - panic");
    let _ = writeln!(uart, "1..1");
    let _ = writeln!(uart, "FREERUSTOS_TEST_RESULT=FAIL");
    loop {
        unsafe { core::arch::asm!("wfi", options(nostack)) }
    }
}

unsafe fn write_mtime_near_low_rollover() {
    let low = MTIME_ADDR as *mut u32;
    let high = (MTIME_ADDR + 4) as *mut u32;

    ptr::write_volatile(low, 0);
    ptr::write_volatile(high, 0);
    ptr::write_volatile(low, u32::MAX - 15);
}

fn test_mtime_rollover_read() -> bool {
    unsafe { write_mtime_near_low_rollover() };

    let mut previous = ullPortReadMtime();
    let mut iterations = 0usize;
    while iterations < 2_000_000 {
        let current = ullPortReadMtime();
        if current < previous {
            return false;
        }
        if current >> 32 != 0 {
            return true;
        }
        previous = current;
        iterations += 1;
    }
    false
}

unsafe fn configure_uart_external_interrupt() {
    ptr::write_volatile(
        (PLIC_BASE + (PLIC_UART0_SOURCE as usize * 4)) as *mut u32,
        1,
    );
    ptr::write_volatile(PLIC_ENABLE_MMODE_HART0 as *mut u32, 1 << PLIC_UART0_SOURCE);
    ptr::write_volatile(PLIC_THRESHOLD_MMODE_HART0 as *mut u32, 0);

    /* Enable UART TX and set a one-byte TX watermark. An empty FIFO then
     * asserts source 3 until the ISR disables UART_IE. */
    ptr::write_volatile(UART_TXCTRL as *mut u32, 1 | (1 << 16));
    ptr::write_volatile(UART_IE as *mut u32, 1);
}

extern "C" fn external_interrupt(uxCause: UBaseType_t) -> BaseType_t {
    unsafe {
        EXTERNAL_CAUSE = uxCause;
        EXTERNAL_REPORTED_ISR = xPortIsInsideInterrupt();

        let source = ptr::read_volatile(PLIC_CLAIM_MMODE_HART0 as *const u32);
        EXTERNAL_SOURCE = source;
        ptr::write_volatile(UART_IE as *mut u32, 0);

        let mut xHigherPriorityTaskWoken = pdFALSE;
        vTaskNotifyGiveFromISR(HIGH_HANDLE, &mut xHigherPriorityTaskWoken);

        if source != 0 {
            ptr::write_volatile(PLIC_CLAIM_MMODE_HART0 as *mut u32, source);
        }

        /* Return false deliberately: only the request written by
         * portYIELD_FROM_ISR() can make the common epilogue switch tasks. */
        portYIELD_FROM_ISR(xHigherPriorityTaskWoken);
        pdFALSE
    }
}

extern "C" fn high_task(_parameter: *mut c_void) {
    let notification = unsafe { ulTaskNotifyTake(pdTRUE, portMAX_DELAY) };

    let mie_after_external: usize;
    unsafe {
        core::arch::asm!("csrr {mie}, mie", mie = out(reg) mie_after_external, options(nomem, nostack));
        core::arch::asm!("csrs mie, {mtie}", mtie = in(reg) 0x80, options(nomem, nostack));
    }

    let periodic_tick_start = xTaskGetTickCount();
    let periodic_mtime_start = ullPortReadMtime();
    // Safety: this is the live high-priority task with the scheduler running.
    unsafe { vTaskDelay(PERIODIC_TEST_TICKS) };
    let periodic_ticks = xTaskGetTickCount().wrapping_sub(periodic_tick_start);
    let periodic_counts = ullPortReadMtime().wrapping_sub(periodic_mtime_start);

    unsafe { vTaskSuspend(LOW_HANDLE) };

    let sleeps_before = uxPortGetTicklessSleepCount();
    let tickless_tick_start = xTaskGetTickCount();
    let tickless_mtime_start = ullPortReadMtime();
    // Safety: this remains the live high-priority task after suspending the
    // low task; the scheduler itself is not suspended.
    unsafe { vTaskDelay(TICKLESS_TEST_TICKS) };
    let tickless_ticks = xTaskGetTickCount().wrapping_sub(tickless_tick_start);
    let tickless_counts = ullPortReadMtime().wrapping_sub(tickless_mtime_start);
    let sleeps_after = uxPortGetTicklessSleepCount();

    let expected_periodic_counts = periodic_ticks as u64 * 32_768 / 1_000;
    let expected_tickless_counts = tickless_ticks as u64 * 32_768 / 1_000;
    let periodic_error = periodic_counts.abs_diff(expected_periodic_counts);
    let tickless_error = tickless_counts.abs_diff(expected_tickless_counts);

    let mut tap = Tap::new(Uart);
    tap.diagnostic(format_args!(
        "periodic ticks={} mtime_counts={} error={}",
        periodic_ticks, periodic_counts, periodic_error
    ));
    tap.diagnostic(format_args!(
        "tickless ticks={} mtime_counts={} sleeps={} suppressed={} completed={}",
        tickless_ticks,
        tickless_counts,
        sleeps_after.wrapping_sub(sleeps_before),
        xPortGetLastSuppressedTicks(),
        xPortGetLastCompletedSleepTicks()
    ));
    tap.check(
        unsafe { MTIME_ROLLOVER_OK },
        "MTIME read is monotonic across low-word rollover",
    );
    tap.check(
        unsafe { TASK_CRITICAL_REPORTED_ISR == pdFALSE },
        "masked task critical section is not reported as ISR context",
    );
    tap.check(
        notification == 1,
        "external ISR notified the blocked high-priority task",
    );
    tap.check(
        unsafe { EXTERNAL_CAUSE == 11 && EXTERNAL_SOURCE == PLIC_UART0_SOURCE },
        "machine-external trap dispatches the claimed PLIC source",
    );
    tap.check(
        unsafe { EXTERNAL_REPORTED_ISR == pdTRUE },
        "external callback is reported as ISR context",
    );
    tap.check(
        mie_after_external & 0x80 == 0,
        "deferred ISR yield switches tasks before any timer interrupt",
    );
    tap.check(
        periodic_ticks >= PERIODIC_TEST_TICKS
            && periodic_ticks <= PERIODIC_TEST_TICKS + 2
            && periodic_error <= 128,
        "fractional periodic ticks preserve the 32.768 kHz cadence",
    );
    tap.check(
        sleeps_after > sleeps_before && xPortGetLastSuppressedTicks() >= 90,
        "idle path enters a multi-tick suppressed sleep",
    );
    tap.check(
        tickless_ticks >= TICKLESS_TEST_TICKS
            && tickless_ticks <= TICKLESS_TEST_TICKS + 20
            && xPortGetLastCompletedSleepTicks() == TICKLESS_TEST_TICKS - 1,
        "tickless wake steps complete ticks and catches up wake latency",
    );
    tap.check(
        tickless_error <= 128,
        "tickless sleep preserves fractional MTIME accounting",
    );
    let _ = tap.finish(10);

    loop {
        unsafe { core::arch::asm!("wfi", options(nostack)) }
    }
}

extern "C" fn low_task(_parameter: *mut c_void) {
    portENTER_CRITICAL();
    unsafe {
        TASK_CRITICAL_REPORTED_ISR = xPortIsInsideInterrupt();
    }
    portEXIT_CRITICAL();

    /* Prove that only the external-interrupt epilogue can schedule high_task. */
    unsafe {
        core::arch::asm!("csrc mie, {mtie}", mtie = in(reg) 0x80, options(nomem, nostack));
        configure_uart_external_interrupt();
    }

    loop {
        core::hint::spin_loop();
    }
}

#[entry]
fn main() -> ! {
    unsafe {
        MTIME_ROLLOVER_OK = test_mtime_rollover_read();
        vPortSetExternalInterruptHandler(Some(external_interrupt));

        LOW_HANDLE = xTaskCreateStatic(
            low_task,
            b"rv-low\0".as_ptr(),
            LOW_STACK.len(),
            ptr::null_mut(),
            1,
            LOW_STACK.as_mut_ptr(),
            &mut LOW_TCB,
        );
        HIGH_HANDLE = xTaskCreateStatic(
            high_task,
            b"rv-high\0".as_ptr(),
            HIGH_STACK.len(),
            ptr::null_mut(),
            2,
            HIGH_STACK.as_mut_ptr(),
            &mut HIGH_TCB,
        );

        if LOW_HANDLE.is_null() || HIGH_HANDLE.is_null() {
            panic!("task creation failed");
        }
    }

    // Safety: startup is single-threaded, both task contexts are installed,
    // and no kernel interrupt is enabled until the port starts the scheduler.
    unsafe { vTaskStartScheduler() };
    panic!("scheduler returned")
}
