//! 64-bit EventGroup FromISR regression through the real timer interrupt.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use freertos_in_rust::kernel::event_groups::{
    xEventGroupClearBitsFromISR, xEventGroupCreateStatic, xEventGroupGetBits,
    xEventGroupSetBitsFromISR, EventBits_t, StaticEventGroup_t,
};
use freertos_in_rust::kernel::tasks::vTaskDelay;
use freertos_in_rust::port::portYIELD_FROM_ISR;
use freertos_in_rust::types::{
    pdFALSE, pdPASS, BaseType_t, EventGroupHandle_t, StackType_t, TickType_t,
};
use freertos_qemu_cortex_a53::{
    finish, init, set_tick_isr_hook, spawn_static, start_scheduler, tap, AlignedTaskBuffer,
};

const STACK_WORDS: usize = 512;
const CONTROLLER_PRIORITY: u64 = 2;
const REPORTER_PRIORITY: u64 = 3;
const REPORT_TICKS: TickType_t = 40;

// Bits above 31 were truncated by the old uint32_t pended-call parameter.
// Bit 55 is the highest usable bit with 64-bit event groups.
const HIGH_EVENT_BITS: EventBits_t = (1_u64 << 55) | (1_u64 << 41);

const OPERATION_IDLE: u32 = 0;
const OPERATION_SET: u32 = 1;
const OPERATION_CLEAR: u32 = 2;

static OPERATION: AtomicU32 = AtomicU32::new(OPERATION_IDLE);
static SET_QUEUE_RESULT: AtomicU64 = AtomicU64::new(u64::MAX);
static CLEAR_QUEUE_RESULT: AtomicU64 = AtomicU64::new(u64::MAX);
static SET_OBSERVED: AtomicU32 = AtomicU32::new(0);
static CLEAR_OBSERVED: AtomicU32 = AtomicU32::new(0);
static ISR_HIGHER_PRIORITY_WOKEN: AtomicU64 = AtomicU64::new(u64::MAX);

static mut EVENT_CONTROL: StaticEventGroup_t = StaticEventGroup_t::new();
static mut EVENT_GROUP: EventGroupHandle_t = ptr::null_mut();

static mut CONTROLLER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut CONTROLLER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

extern "C" fn timer_isr_hook() {
    match OPERATION.swap(OPERATION_IDLE, Ordering::AcqRel) {
        OPERATION_SET => {
            let mut higher_priority_task_woken: BaseType_t = pdFALSE;
            // SAFETY: the static group remains live until the daemon consumes
            // this deferred operation; this hook runs at an allowed IRQ level.
            let result = unsafe {
                xEventGroupSetBitsFromISR(
                    EVENT_GROUP,
                    HIGH_EVENT_BITS,
                    &mut higher_priority_task_woken,
                )
            };
            SET_QUEUE_RESULT.store(result as u64, Ordering::Release);
            ISR_HIGHER_PRIORITY_WOKEN.store(higher_priority_task_woken as u64, Ordering::Release);
            portYIELD_FROM_ISR(higher_priority_task_woken);
        }
        OPERATION_CLEAR => {
            // SAFETY: the same static group and interrupt-context invariants as
            // the set operation above apply.
            let result = unsafe { xEventGroupClearBitsFromISR(EVENT_GROUP, HIGH_EVENT_BITS) };
            CLEAR_QUEUE_RESULT.store(result as u64, Ordering::Release);
            if result == pdPASS {
                // ClearBitsFromISR has no higher-priority output; FreeRTOS
                // specifies yielding when the command was queued.
                portYIELD_FROM_ISR(pdPASS);
            }
        }
        _ => {}
    }
}

extern "C" fn controller(_parameter: *mut c_void) {
    // Safety: setter is a live task and the scheduler is running.
    unsafe { vTaskDelay(1) };
    OPERATION.store(OPERATION_SET, Ordering::Release);

    let mut attempts = 0;
    while attempts < 20 {
        // Safety: setter remains a live task in ordinary task context.
        unsafe { vTaskDelay(1) };
        // SAFETY: EVENT_GROUP names the live static group for this firmware.
        if unsafe { xEventGroupGetBits(EVENT_GROUP) } & HIGH_EVENT_BITS == HIGH_EVENT_BITS {
            SET_OBSERVED.store(1, Ordering::Release);
            break;
        }
        attempts += 1;
    }

    OPERATION.store(OPERATION_CLEAR, Ordering::Release);
    attempts = 0;
    while attempts < 20 {
        // Safety: waiter is a live task and the scheduler is running.
        unsafe { vTaskDelay(1) };
        // SAFETY: EVENT_GROUP names the live static group for this firmware.
        if unsafe { xEventGroupGetBits(EVENT_GROUP) } & HIGH_EVENT_BITS == 0 {
            CLEAR_OBSERVED.store(1, Ordering::Release);
            break;
        }
        attempts += 1;
    }

    loop {
        // Safety: waiter remains a live task in ordinary task context.
        unsafe { vTaskDelay(1000) };
    }
}

extern "C" fn reporter(_parameter: *mut c_void) {
    // Safety: reporter is a live task and the scheduler is running.
    unsafe { vTaskDelay(REPORT_TICKS) };

    // SAFETY: EVENT_GROUP names the live static group for this firmware.
    let final_bits = unsafe { xEventGroupGetBits(EVENT_GROUP) };
    let mut output = tap();
    output.check(
        SET_QUEUE_RESULT.load(Ordering::Acquire) == pdPASS as u64
            && SET_OBSERVED.load(Ordering::Acquire) != 0,
        "physical_timer_isr_pends_64_bit_event_set",
    );
    output.check(
        CLEAR_QUEUE_RESULT.load(Ordering::Acquire) == pdPASS as u64
            && CLEAR_OBSERVED.load(Ordering::Acquire) != 0,
        "physical_timer_isr_pends_64_bit_event_clear",
    );
    output.check(
        final_bits & HIGH_EVENT_BITS == 0,
        "daemon_callbacks_preserve_bits_41_and_55",
    );
    output.diagnostic(format_args!(
        "set={}/{} clear={}/{} woken={} final=0x{:016x}",
        SET_QUEUE_RESULT.load(Ordering::Relaxed),
        SET_OBSERVED.load(Ordering::Relaxed),
        CLEAR_QUEUE_RESULT.load(Ordering::Relaxed),
        CLEAR_OBSERVED.load(Ordering::Relaxed),
        ISR_HIGHER_PRIORITY_WOKEN.load(Ordering::Relaxed),
        final_bits,
    ));
    finish(output, 3)
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    init();
    set_tick_isr_hook(timer_isr_hook);

    unsafe {
        // SAFETY: EVENT_CONTROL is initialized once at a stable address and
        // lives for the entire firmware execution.
        EVENT_GROUP = xEventGroupCreateStatic(core::ptr::addr_of_mut!(EVENT_CONTROL));
        assert!(!EVENT_GROUP.is_null());

        spawn_static(
            b"event-ctl\0",
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
