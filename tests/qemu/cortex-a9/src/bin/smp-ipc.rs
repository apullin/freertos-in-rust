//! Two-core SPSC messages, deferred ISR events, and timer-daemon wakeups.
#![no_std]
#![no_main]

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};
use freertos_in_rust::kernel::{event_groups::*, stream_buffer::*, tasks::*, timers::*};
use freertos_in_rust::port::portYIELD_FROM_ISR;
use freertos_in_rust::types::*;
use freertos_qemu_cortex_a9::{
    arm_auxiliary_wake, auxiliary_interrupts, finish, init, set_auxiliary_hook, start_scheduler,
    start_secondary, tap,
};

const WORDS: usize = 1024;
const ITEMS: u32 = 1000;
static mut STACKS: [[StackType_t; WORDS]; 3] = [[0; WORDS]; 3];
static mut TCBS: [StaticTask_t; 3] = [const { StaticTask_t::new() }; 3];
static mut WORKER: TaskHandle_t = ptr::null_mut();
static mut MESSAGE_STORAGE: [u8; 65] = [0; 65];
static mut MESSAGE_CONTROL: StaticMessageBuffer_t = StaticMessageBuffer_t::new();
static mut EVENT_CONTROL: StaticEventGroup_t = StaticEventGroup_t::new();
static mut TIMER_CONTROL: StaticTimer_t = StaticTimer_t::new();
static mut MESSAGE: MessageBufferHandle_t = ptr::null_mut();
static mut EVENT: EventGroupHandle_t = ptr::null_mut();
static mut TIMER: TimerHandle_t = ptr::null_mut();
static STAGE: AtomicU32 = AtomicU32::new(0);
static ERRORS: AtomicU32 = AtomicU32::new(0);
static TIMER_CALLS: AtomicU32 = AtomicU32::new(0);
static BAD_INDEX: AtomicU32 = AtomicU32::new(u32::MAX);
static BAD_VALUE: AtomicU32 = AtomicU32::new(u32::MAX);
static BAD_LENGTH: AtomicU32 = AtomicU32::new(u32::MAX);
static BAD_TICKS: AtomicU32 = AtomicU32::new(u32::MAX);

extern "C" fn interrupt() {
    unsafe {
        let mut woken = pdFALSE;
        vTaskNotifyGiveFromISR(WORKER, &mut woken);
        if xEventGroupSetBitsFromISR(EVENT, 1, &mut woken) != pdPASS {
            ERRORS.fetch_or(2, Ordering::Relaxed);
        }
        portYIELD_FROM_ISR(woken);
    }
}

unsafe extern "C" fn timer_callback(_: TimerHandle_t) {
    TIMER_CALLS.fetch_add(1, Ordering::Release);
    xEventGroupSetBits(EVENT, 2);
}

extern "C" fn worker(_: *mut c_void) {
    unsafe {
        let mut i = 0;
        while i < ITEMS {
            let mut value = u32::MAX;
            let before = xTaskGetTickCount();
            let length = xMessageBufferReceive(MESSAGE, ptr::addr_of_mut!(value).cast(), 4, 500);
            if length != 4 || value != i {
                ERRORS.fetch_or(1, Ordering::Relaxed);
                if BAD_INDEX.load(Ordering::Relaxed) == u32::MAX {
                    BAD_INDEX.store(i, Ordering::Relaxed);
                    BAD_VALUE.store(value, Ordering::Relaxed);
                    BAD_LENGTH.store(length as u32, Ordering::Relaxed);
                    BAD_TICKS.store(xTaskGetTickCount().wrapping_sub(before), Ordering::Relaxed);
                }
            }
            i += 1;
        }
        xTaskNotifyStateClear(ptr::null_mut());
        ulTaskNotifyValueClear(ptr::null_mut(), u32::MAX);
        STAGE.store(1, Ordering::Release);
        if xEventGroupWaitBits(EVENT, 1, pdTRUE, pdTRUE, 500) & 1 == 0 {
            ERRORS.fetch_or(4, Ordering::Relaxed);
        }
        if ulTaskNotifyTake(pdTRUE, 500) != 1 {
            ERRORS.fetch_or(8, Ordering::Relaxed);
        }
        STAGE.store(2, Ordering::Release);
        if xEventGroupWaitBits(EVENT, 2, pdTRUE, pdTRUE, 500) & 2 == 0 {
            ERRORS.fetch_or(16, Ordering::Relaxed);
        }
        STAGE.store(3, Ordering::Release);
        loop {
            vTaskDelay(1000);
        }
    }
}

fn wait_stage(stage: u32) {
    let start = xTaskGetTickCount();
    while STAGE.load(Ordering::Acquire) != stage {
        assert!(xTaskGetTickCount().wrapping_sub(start) < 1000);
        unsafe {
            vTaskDelay(1);
        }
    }
}

extern "C" fn reporter(_: *mut c_void) {
    unsafe {
        let mut out = tap();
        let mut i = 0;
        while i < ITEMS {
            assert_eq!(
                xMessageBufferSend(MESSAGE, ptr::addr_of!(i).cast(), 4, 500),
                4
            );
            i += 1;
        }
        wait_stage(1);
        out.check(
            ERRORS.load(Ordering::Relaxed) & 1 == 0,
            "cross_core_message_buffer_preserves_boundaries_and_data",
        );
        // An ISR on the suspending CPU must enqueue daemon work without
        // deadlocking. The worker must not unblock until scheduling resumes.
        vTaskSuspendAll();
        let before = auxiliary_interrupts();
        arm_auxiliary_wake(2000);
        while auxiliary_interrupts() == before {
            core::hint::spin_loop();
        }
        let deferred = STAGE.load(Ordering::Acquire) == 1;
        xTaskResumeAll();
        wait_stage(2);
        out.check(
            deferred && ERRORS.load(Ordering::Relaxed) & 6 == 0,
            "isr_event_is_deferred_until_scheduler_resume_and_wakes_remote_task",
        );
        out.check(
            ERRORS.load(Ordering::Relaxed) & 8 == 0,
            "isr_notification_crosses_cores",
        );
        assert_eq!(xTimerStart(TIMER, 100), pdPASS);
        wait_stage(3);
        vTaskDelay(12);
        assert_eq!(xTimerStop(TIMER, 100), pdPASS);
        out.check(
            TIMER_CALLS.load(Ordering::Acquire) >= 2 && ERRORS.load(Ordering::Relaxed) & 16 == 0,
            "auto_reload_timer_daemon_wakes_remote_event_waiter",
        );
        out.diagnostic(format_args!(
            "errors={} callbacks={} auxiliary_irqs={} first_bad={}/{}/{} ticks={}",
            ERRORS.load(Ordering::Relaxed),
            TIMER_CALLS.load(Ordering::Acquire),
            auxiliary_interrupts(),
            BAD_INDEX.load(Ordering::Relaxed),
            BAD_VALUE.load(Ordering::Relaxed),
            BAD_LENGTH.load(Ordering::Relaxed),
            BAD_TICKS.load(Ordering::Relaxed)
        ));
        finish(out, 4)
    }
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    init();
    set_auxiliary_hook(interrupt);
    unsafe {
        MESSAGE = xMessageBufferCreateStatic(
            65,
            ptr::addr_of_mut!(MESSAGE_STORAGE).cast(),
            ptr::addr_of_mut!(MESSAGE_CONTROL),
        );
        EVENT = xEventGroupCreateStatic(ptr::addr_of_mut!(EVENT_CONTROL));
        TIMER = xTimerCreateStatic(
            b"periodic\0".as_ptr(),
            3,
            pdTRUE,
            ptr::null_mut(),
            timer_callback,
            ptr::addr_of_mut!(TIMER_CONTROL),
        );
        assert!(!MESSAGE.is_null() && !EVENT.is_null() && !TIMER.is_null());
        assert_eq!(
            vTimerSetTimerTaskMemory(
                ptr::addr_of_mut!(TCBS[2]),
                ptr::addr_of_mut!(STACKS[2]).cast(),
                WORDS
            ),
            pdPASS
        );
        let entries: [TaskFunction_t; 2] = [reporter, worker];
        let mut i = 0;
        while i < 2 {
            let task = xTaskCreateStatic(
                entries[i],
                b"smp-ipc\0".as_ptr(),
                WORDS,
                ptr::null_mut(),
                4 - i as u32,
                ptr::addr_of_mut!(STACKS[i]).cast(),
                ptr::addr_of_mut!(TCBS[i]),
            );
            assert!(!task.is_null());
            vTaskCoreAffinitySet(task, 1 << i);
            if i == 1 {
                WORKER = task;
            }
            i += 1;
        }
    }
    start_secondary();
    start_scheduler()
}
