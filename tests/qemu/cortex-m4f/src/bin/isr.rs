//! Queue and timer-daemon regressions driven from a real Cortex-M interrupt.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate panic_semihosting;

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicI32, AtomicPtr, AtomicU32, Ordering};

use cortex_m_rt::{entry, exception};
use freertos_in_rust::kernel::queue::{
    vQueueWaitForMessageRestricted, xQueueCreate, xQueueReceive, xQueueSendFromISR, Queue_t,
};
use freertos_in_rust::kernel::tasks::{
    vTaskDelay, vTaskSuspendAll, xTaskGetTickCount, xTaskResumeAll,
};
use freertos_in_rust::kernel::timers::xTimerPendFunctionCallFromISR;
use freertos_in_rust::port::{portYIELD_FROM_ISR, xPortIsInsideInterrupt};
use freertos_in_rust::types::{pdFALSE, pdPASS, BaseType_t, StackType_t, TickType_t, UBaseType_t};
use freertos_qemu_cortex_m4f::{finish, spawn_static, start_scheduler, tap, AlignedTaskBuffer};

const STACK_WORDS: usize = 256;
const QUEUE_LENGTH: UBaseType_t = 1;
const RECEIVER_PRIORITY: UBaseType_t = 3;
const CONTROLLER_PRIORITY: UBaseType_t = 2;
const REPORTER_PRIORITY: UBaseType_t = 4;
const REPORT_TICKS: TickType_t = 30;
const QUEUED_VALUE: u8 = 0xA5;
const CALLBACK_VALUE: u32 = 0x1234_5678;

const NVIC_ISER0: *mut u32 = 0xE000_E100 as *mut u32;
const NVIC_ISPR0: *mut u32 = 0xE000_E200 as *mut u32;
const NVIC_IPR0: *mut u8 = 0xE000_E400 as *mut u8;

// The queue implementation's private constants are -1 for unlocked and 0 for
// locked-but-unmodified. The white-box setup below intentionally establishes
// the exact state produced by prvLockQueue(), then uses the public restricted
// wait path to execute the real prvUnlockQueue().
const QUEUE_UNLOCKED: i8 = -1;
const QUEUE_LOCKED_UNMODIFIED: i8 = 0;

static TEST_QUEUE: AtomicPtr<Queue_t> = AtomicPtr::new(ptr::null_mut());
static RECEIVER_STARTED: AtomicU32 = AtomicU32::new(0);
static RECEIVER_RAN: AtomicU32 = AtomicU32::new(0);
static RECEIVER_VALUE: AtomicU32 = AtomicU32::new(0);
static RECEIVER_ELAPSED: AtomicU32 = AtomicU32::new(u32::MAX);

static ISR_RAN: AtomicU32 = AtomicU32::new(0);
static ISR_NUMBER: AtomicI32 = AtomicI32::new(-1);
static ISR_CONTEXT: AtomicU32 = AtomicU32::new(0);
static ISR_QUEUE_RESULT: AtomicU32 = AtomicU32::new(u32::MAX);
static ISR_TIMER_RESULT: AtomicU32 = AtomicU32::new(u32::MAX);
static ISR_HIGHER_PRIORITY_WOKEN: AtomicU32 = AtomicU32::new(u32::MAX);
static ISR_TX_LOCK_AFTER_SEND: AtomicI32 = AtomicI32::new(i32::MIN);

static RECEIVER_BEFORE_UNLOCK: AtomicU32 = AtomicU32::new(u32::MAX);
static LOCKS_AFTER_UNLOCK: AtomicU32 = AtomicU32::new(0);
static TIMER_CALLBACK_RAN: AtomicU32 = AtomicU32::new(0);
static TIMER_CALLBACK_VALID: AtomicU32 = AtomicU32::new(0);
static CALLBACK_COOKIE: u8 = 0;

static mut RECEIVER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut RECEIVER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut CONTROLLER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut CONTROLLER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

extern "C" fn timer_pended_callback(parameter: *mut c_void, value: u32) {
    let expected = core::ptr::addr_of!(CALLBACK_COOKIE)
        .cast_mut()
        .cast::<c_void>();
    TIMER_CALLBACK_VALID.store(
        (parameter == expected && value == CALLBACK_VALUE) as u32,
        Ordering::Release,
    );
    TIMER_CALLBACK_RAN.store(1, Ordering::Release);
}

/// IRQ0 is software-pended by the controller after it has suspended the
/// scheduler and established the queue-locked state.
#[exception]
unsafe fn DefaultHandler(interrupt_number: i16) {
    ISR_NUMBER.store(interrupt_number as i32, Ordering::Relaxed);
    ISR_CONTEXT.store(
        (xPortIsInsideInterrupt() != pdFALSE) as u32,
        Ordering::Relaxed,
    );

    if interrupt_number == 0 {
        let queue = TEST_QUEUE.load(Ordering::Acquire);
        let value = QUEUED_VALUE;
        let mut higher_priority_task_woken: BaseType_t = pdFALSE;

        let queue_result = xQueueSendFromISR(
            queue,
            core::ptr::addr_of!(value).cast::<c_void>(),
            &mut higher_priority_task_woken,
        );
        let timer_result = xTimerPendFunctionCallFromISR(
            timer_pended_callback,
            core::ptr::addr_of!(CALLBACK_COOKIE)
                .cast_mut()
                .cast::<c_void>(),
            CALLBACK_VALUE,
            &mut higher_priority_task_woken,
        );

        ISR_QUEUE_RESULT.store(queue_result as u32, Ordering::Relaxed);
        ISR_TIMER_RESULT.store(timer_result as u32, Ordering::Relaxed);
        ISR_HIGHER_PRIORITY_WOKEN.store(higher_priority_task_woken as u32, Ordering::Relaxed);
        ISR_TX_LOCK_AFTER_SEND.store((*queue).cTxLock as i32, Ordering::Relaxed);
        ISR_RAN.store(1, Ordering::Release);

        portYIELD_FROM_ISR(higher_priority_task_woken);
    }
}

extern "C" fn receiver(_parameter: *mut c_void) {
    let queue = TEST_QUEUE.load(Ordering::Acquire);
    let start = xTaskGetTickCount();
    let mut value = 0u8;
    RECEIVER_STARTED.store(1, Ordering::Release);

    let result =
        unsafe { xQueueReceive(queue, core::ptr::addr_of_mut!(value).cast::<c_void>(), 1000) };

    RECEIVER_ELAPSED.store(
        xTaskGetTickCount().wrapping_sub(start) as u32,
        Ordering::Relaxed,
    );
    RECEIVER_VALUE.store(value as u32, Ordering::Relaxed);
    RECEIVER_RAN.store((result == pdPASS) as u32, Ordering::Release);

    loop {
        vTaskDelay(1000);
    }
}

fn pend_application_interrupt() {
    unsafe {
        // IRQ0 uses a priority from which FreeRTOS FromISR APIs are permitted.
        core::ptr::write_volatile(NVIC_IPR0, 0xC0);
        core::ptr::write_volatile(NVIC_ISER0, 1);
        core::ptr::write_volatile(NVIC_ISPR0, 1);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

extern "C" fn controller(_parameter: *mut c_void) {
    while RECEIVER_STARTED.load(Ordering::Acquire) == 0 {
        cortex_m::asm::nop();
    }

    let queue = TEST_QUEUE.load(Ordering::Acquire);

    vTaskSuspendAll();
    unsafe {
        // Reproduce the state immediately after prvLockQueue(). Interrupts stay
        // enabled so the application ISR can post while the scheduler is held.
        (*queue).cRxLock = QUEUE_LOCKED_UNMODIFIED;
        (*queue).cTxLock = QUEUE_LOCKED_UNMODIFIED;
    }

    pend_application_interrupt();

    let mut spins = 0;
    while ISR_RAN.load(Ordering::Acquire) == 0 && spins < 100_000 {
        cortex_m::asm::nop();
        spins += 1;
    }

    RECEIVER_BEFORE_UNLOCK.store(RECEIVER_RAN.load(Ordering::Acquire), Ordering::Relaxed);

    unsafe {
        // The queued byte means this call will not block the controller. It
        // does run prvLockQueue()/prvUnlockQueue(), which must consume cTxLock
        // and defer the blocked receiver to xPendingReadyList.
        vQueueWaitForMessageRestricted(queue, 0, pdFALSE);

        let locks_reset = (*queue).cRxLock == QUEUE_UNLOCKED && (*queue).cTxLock == QUEUE_UNLOCKED;
        LOCKS_AFTER_UNLOCK.store(locks_reset as u32, Ordering::Relaxed);
    }

    xTaskResumeAll();

    loop {
        vTaskDelay(1000);
    }
}

extern "C" fn reporter(_parameter: *mut c_void) {
    vTaskDelay(REPORT_TICKS);

    let isr_ran = ISR_RAN.load(Ordering::Acquire) != 0;
    let queue_result = ISR_QUEUE_RESULT.load(Ordering::Relaxed);
    let timer_result = ISR_TIMER_RESULT.load(Ordering::Relaxed);
    let higher_priority_woken = ISR_HIGHER_PRIORITY_WOKEN.load(Ordering::Relaxed);
    let receiver_ran = RECEIVER_RAN.load(Ordering::Acquire) != 0;
    let callback_ran = TIMER_CALLBACK_RAN.load(Ordering::Acquire) != 0;

    let mut output = tap();
    output.check(
        isr_ran
            && ISR_NUMBER.load(Ordering::Relaxed) == 0
            && ISR_CONTEXT.load(Ordering::Relaxed) != 0
            && queue_result == pdPASS as u32,
        "application_isr_sends_to_locked_queue",
    );
    output.check(
        ISR_TX_LOCK_AFTER_SEND.load(Ordering::Relaxed) == 1
            && higher_priority_woken == pdFALSE as u32
            && RECEIVER_BEFORE_UNLOCK.load(Ordering::Relaxed) == 0,
        "locked_queue_defers_receiver_wakeup",
    );
    output.check(
        LOCKS_AFTER_UNLOCK.load(Ordering::Relaxed) != 0
            && receiver_ran
            && RECEIVER_VALUE.load(Ordering::Relaxed) == QUEUED_VALUE as u32
            && RECEIVER_ELAPSED.load(Ordering::Relaxed) < REPORT_TICKS as u32,
        "queue_unlock_wakes_blocked_receiver",
    );
    output.check(
        timer_result == pdPASS as u32
            && callback_ran
            && TIMER_CALLBACK_VALID.load(Ordering::Relaxed) != 0,
        "timer_command_from_isr_runs_in_daemon",
    );
    output.diagnostic(format_args!(
        "irq={} context={} qsend={} timer={} woken={} tx_lock={} before={} locks={} receiver={}/0x{:02x}/{} callback={}/{}",
        ISR_NUMBER.load(Ordering::Relaxed),
        ISR_CONTEXT.load(Ordering::Relaxed),
        queue_result,
        timer_result,
        higher_priority_woken,
        ISR_TX_LOCK_AFTER_SEND.load(Ordering::Relaxed),
        RECEIVER_BEFORE_UNLOCK.load(Ordering::Relaxed),
        LOCKS_AFTER_UNLOCK.load(Ordering::Relaxed),
        RECEIVER_RAN.load(Ordering::Relaxed),
        RECEIVER_VALUE.load(Ordering::Relaxed),
        RECEIVER_ELAPSED.load(Ordering::Relaxed),
        TIMER_CALLBACK_RAN.load(Ordering::Relaxed),
        TIMER_CALLBACK_VALID.load(Ordering::Relaxed),
    ));

    finish(output, 4)
}

#[entry]
fn main() -> ! {
    let queue = unsafe { xQueueCreate(QUEUE_LENGTH, core::mem::size_of::<u8>() as UBaseType_t) };
    assert!(!queue.is_null());
    TEST_QUEUE.store(queue, Ordering::Release);

    unsafe {
        spawn_static(
            b"isr-rx\0",
            RECEIVER_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(RECEIVER_TCB),
            RECEIVER_PRIORITY,
            receiver,
        );
        spawn_static(
            b"isr-ctl\0",
            CONTROLLER_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(CONTROLLER_TCB),
            CONTROLLER_PRIORITY,
            controller,
        );
        spawn_static(
            b"report\0",
            REPORTER_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(REPORTER_TCB),
            REPORTER_PRIORITY,
            reporter,
        );
    }

    start_scheduler()
}
