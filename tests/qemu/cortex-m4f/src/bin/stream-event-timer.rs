//! Stream-buffer, event-group, and software-timer regressions using a real IRQ.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate panic_semihosting;

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicI32, AtomicPtr, AtomicU32, Ordering};

use cortex_m_rt::{entry, exception};
use freertos_in_rust::kernel::event_groups::{
    xEventGroupCreateStatic, xEventGroupGetBits, xEventGroupSetBitsFromISR, StaticEventGroup_t,
};
use freertos_in_rust::kernel::stream_buffer::{
    sbTYPE_STREAM_BUFFER, xStreamBufferGenericCreateStatic, xStreamBufferReceive,
    xStreamBufferReceiveFromISR, xStreamBufferReset, xStreamBufferSend,
    xStreamBufferSendCompletedFromISR, xStreamBufferSendFromISR, StaticStreamBuffer_t,
    StreamBufferHandle_t,
};
use freertos_in_rust::kernel::tasks::{vTaskDelay, xTaskGetTickCount, StaticTask_t};
use freertos_in_rust::kernel::timers::{
    vTimerSetTimerTaskMemory, xTimerChangePeriod, xTimerCreateStatic, xTimerStart, StaticTimer_t,
};
use freertos_in_rust::port::{portYIELD_FROM_ISR, xPortIsInsideInterrupt};
use freertos_in_rust::types::{
    pdFALSE, pdPASS, pdTRUE, BaseType_t, EventGroupHandle_t, StackType_t, TickType_t,
    TimerHandle_t, UBaseType_t,
};
use freertos_qemu_cortex_m4f::{finish, spawn_static, start_scheduler, tap, AlignedTaskBuffer};

const STACK_WORDS: usize = 256;
const RECEIVER_PRIORITY: UBaseType_t = 3;
const CONTROLLER_PRIORITY: UBaseType_t = 2;
const REPORTER_PRIORITY: UBaseType_t = 4;
const REPORT_TICKS: TickType_t = 30;
const STREAM_BYTE: u8 = 0xA7;
const EVENT_BIT: u32 = 0x0080_0000;

const NVIC_ISER0: *mut u32 = 0xE000_E100 as *mut u32;
const NVIC_ISPR0: *mut u32 = 0xE000_E200 as *mut u32;
const NVIC_IPR0: *mut u8 = 0xE000_E400 as *mut u8;

static STREAM: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static EVENT_GROUP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

static SEND_CALLBACKS: AtomicU32 = AtomicU32::new(0);
static RECEIVE_CALLBACKS: AtomicU32 = AtomicU32::new(0);
static ISR_CALLBACKS: AtomicU32 = AtomicU32::new(0);

static RECEIVER_STARTED: AtomicU32 = AtomicU32::new(0);
static RECEIVER_RESULT: AtomicU32 = AtomicU32::new(0);
static RECEIVER_BYTE: AtomicU32 = AtomicU32::new(0);
static RECEIVER_ELAPSED: AtomicU32 = AtomicU32::new(u32::MAX);

static ISR_NUMBER: AtomicI32 = AtomicI32::new(-1);
static ISR_CONTEXT: AtomicU32 = AtomicU32::new(0);
static ISR_STREAM_RESULT: AtomicU32 = AtomicU32::new(u32::MAX);
static ISR_EVENT_RESULT: AtomicU32 = AtomicU32::new(u32::MAX);

static TIMER_CALLBACKS: AtomicU32 = AtomicU32::new(0);
static TIMER_CALLBACK_TICK: AtomicU32 = AtomicU32::new(u32::MAX);
static ZERO_TIMER_REJECTED: AtomicU32 = AtomicU32::new(0);
static ZERO_CHANGE_REJECTED: AtomicU32 = AtomicU32::new(0);

static mut STREAM_CONTROL: StaticStreamBuffer_t = StaticStreamBuffer_t::new();
static mut STREAM_STORAGE: [u8; 33] = [0; 33];
static mut EVENT_CONTROL: StaticEventGroup_t = StaticEventGroup_t::new();
static mut TIMER_CONTROL: StaticTimer_t = StaticTimer_t::new();
static mut ZERO_TIMER_CONTROL: StaticTimer_t = StaticTimer_t::new();
static mut TIMER_TASK_TCB: StaticTask_t = StaticTask_t::new();
static mut TIMER_TASK_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];

static mut RECEIVER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut RECEIVER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut CONTROLLER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut CONTROLLER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();
static mut REPORTER_STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
static mut REPORTER_TCB: AlignedTaskBuffer = AlignedTaskBuffer::new();

extern "C" fn send_completed(
    stream: StreamBufferHandle_t,
    inside_isr: BaseType_t,
    higher_priority_task_woken: *mut BaseType_t,
) {
    SEND_CALLBACKS.fetch_add(1, Ordering::Relaxed);
    if inside_isr != pdFALSE {
        ISR_CALLBACKS.fetch_add(1, Ordering::Relaxed);
        unsafe {
            let _ = xStreamBufferSendCompletedFromISR(stream, higher_priority_task_woken);
        }
    }
}

extern "C" fn receive_completed(
    _stream: StreamBufferHandle_t,
    inside_isr: BaseType_t,
    _higher_priority_task_woken: *mut BaseType_t,
) {
    RECEIVE_CALLBACKS.fetch_add(1, Ordering::Relaxed);
    if inside_isr != pdFALSE {
        ISR_CALLBACKS.fetch_add(1, Ordering::Relaxed);
    }
}

extern "C" fn timer_callback(_timer: TimerHandle_t) {
    TIMER_CALLBACK_TICK.store(xTaskGetTickCount() as u32, Ordering::Relaxed);
    TIMER_CALLBACKS.fetch_add(1, Ordering::Release);
}

#[exception]
unsafe fn DefaultHandler(interrupt_number: i16) {
    ISR_NUMBER.store(interrupt_number as i32, Ordering::Relaxed);
    ISR_CONTEXT.store(
        (xPortIsInsideInterrupt() != pdFALSE) as u32,
        Ordering::Relaxed,
    );

    if interrupt_number == 0 {
        let stream = STREAM.load(Ordering::Acquire);
        let event_group = EVENT_GROUP.load(Ordering::Acquire) as EventGroupHandle_t;
        let byte = STREAM_BYTE;
        let mut higher_priority_task_woken = pdFALSE;

        let stream_result = xStreamBufferSendFromISR(
            stream,
            core::ptr::addr_of!(byte).cast::<c_void>(),
            1,
            &mut higher_priority_task_woken,
        );
        let event_result =
            xEventGroupSetBitsFromISR(event_group, EVENT_BIT, &mut higher_priority_task_woken);

        ISR_STREAM_RESULT.store(stream_result as u32, Ordering::Release);
        ISR_EVENT_RESULT.store(event_result as u32, Ordering::Release);
        portYIELD_FROM_ISR(higher_priority_task_woken);
    }
}

fn pend_application_interrupt() {
    unsafe {
        core::ptr::write_volatile(NVIC_IPR0, 0xC0);
        core::ptr::write_volatile(NVIC_ISER0, 1);
        core::ptr::write_volatile(NVIC_ISPR0, 1);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

extern "C" fn receiver(_parameter: *mut c_void) {
    let stream = STREAM.load(Ordering::Acquire);
    let start = xTaskGetTickCount();
    let mut byte = 0u8;
    RECEIVER_STARTED.store(1, Ordering::Release);

    let received = unsafe {
        xStreamBufferReceive(
            stream,
            core::ptr::addr_of_mut!(byte).cast::<c_void>(),
            1,
            100,
        )
    };

    RECEIVER_ELAPSED.store(
        xTaskGetTickCount().wrapping_sub(start) as u32,
        Ordering::Relaxed,
    );
    RECEIVER_BYTE.store(byte as u32, Ordering::Relaxed);
    RECEIVER_RESULT.store(received as u32, Ordering::Release);

    loop {
        vTaskDelay(1000);
    }
}

extern "C" fn controller(_parameter: *mut c_void) {
    while RECEIVER_STARTED.load(Ordering::Acquire) == 0 {
        cortex_m::asm::nop();
    }

    vTaskDelay(2);
    pend_application_interrupt();

    loop {
        vTaskDelay(1000);
    }
}

extern "C" fn reporter(_parameter: *mut c_void) {
    vTaskDelay(REPORT_TICKS);

    let event_bits = xEventGroupGetBits(EVENT_GROUP.load(Ordering::Acquire));
    let mut output = tap();
    output.check(
        SEND_CALLBACKS.load(Ordering::Relaxed) == 2
            && RECEIVE_CALLBACKS.load(Ordering::Relaxed) == 2
            && ISR_CALLBACKS.load(Ordering::Relaxed) == 1,
        "stream_callbacks_survive_reset_and_report_context",
    );
    output.check(
        RECEIVER_RESULT.load(Ordering::Acquire) == 1
            && RECEIVER_BYTE.load(Ordering::Relaxed) == STREAM_BYTE as u32
            && RECEIVER_ELAPSED.load(Ordering::Relaxed) < REPORT_TICKS as u32,
        "stream_from_isr_wakes_blocked_receiver",
    );
    output.check(
        ISR_NUMBER.load(Ordering::Relaxed) == 0
            && ISR_CONTEXT.load(Ordering::Relaxed) != 0
            && ISR_STREAM_RESULT.load(Ordering::Acquire) == 1
            && ISR_EVENT_RESULT.load(Ordering::Acquire) == pdPASS as u32,
        "real_isr_posts_stream_and_event_commands",
    );
    output.check(
        event_bits & EVENT_BIT == EVENT_BIT,
        "deferred_event_group_set_preserves_high_usable_bit",
    );
    output.check(
        ZERO_TIMER_REJECTED.load(Ordering::Relaxed) != 0
            && ZERO_CHANGE_REJECTED.load(Ordering::Relaxed) != 0,
        "zero_period_timer_operations_are_rejected",
    );
    output.check(
        TIMER_CALLBACKS.load(Ordering::Acquire) == 1
            && TIMER_CALLBACK_TICK.load(Ordering::Relaxed) >= 5
            && TIMER_CALLBACK_TICK.load(Ordering::Relaxed) < REPORT_TICKS as u32,
        "static_one_shot_timer_fires_once",
    );
    output.diagnostic(format_args!(
        "callbacks={}/{}/{} rx={}/0x{:02x}/{} isr={}/{} stream={} event={}/0x{:08x} timer={}/{}",
        SEND_CALLBACKS.load(Ordering::Relaxed),
        RECEIVE_CALLBACKS.load(Ordering::Relaxed),
        ISR_CALLBACKS.load(Ordering::Relaxed),
        RECEIVER_RESULT.load(Ordering::Relaxed),
        RECEIVER_BYTE.load(Ordering::Relaxed),
        RECEIVER_ELAPSED.load(Ordering::Relaxed),
        ISR_NUMBER.load(Ordering::Relaxed),
        ISR_CONTEXT.load(Ordering::Relaxed),
        ISR_STREAM_RESULT.load(Ordering::Relaxed),
        ISR_EVENT_RESULT.load(Ordering::Relaxed),
        event_bits,
        TIMER_CALLBACKS.load(Ordering::Relaxed),
        TIMER_CALLBACK_TICK.load(Ordering::Relaxed),
    ));

    finish(output, 6)
}

#[entry]
fn main() -> ! {
    assert_eq!(
        unsafe {
            vTimerSetTimerTaskMemory(
                core::ptr::addr_of_mut!(TIMER_TASK_TCB),
                TIMER_TASK_STACK.as_mut_ptr(),
                TIMER_TASK_STACK.len(),
            )
        },
        pdPASS,
    );

    let stream = unsafe {
        xStreamBufferGenericCreateStatic(
            STREAM_STORAGE.len(),
            1,
            sbTYPE_STREAM_BUFFER,
            STREAM_STORAGE.as_mut_ptr(),
            core::ptr::addr_of_mut!(STREAM_CONTROL),
            Some(send_completed),
            Some(receive_completed),
        )
    };
    assert!(!stream.is_null());
    STREAM.store(stream, Ordering::Release);

    // Exercise callbacks once, reset, then let the ISR/task pair exercise the
    // retained callbacks again after scheduling starts.
    let initial = 0x31u8;
    let mut returned = 0u8;
    assert_eq!(
        unsafe { xStreamBufferSend(stream, core::ptr::addr_of!(initial).cast(), 1, 0) },
        1
    );
    assert_eq!(
        unsafe { xStreamBufferReceive(stream, core::ptr::addr_of_mut!(returned).cast(), 1, 0,) },
        1
    );
    assert_eq!(returned, initial);
    assert_eq!(unsafe { xStreamBufferReset(stream) }, pdPASS);

    // The ISR receive API is linked into this firmware as an additional
    // compile-time check of the callback-enabled representation.
    let _receive_from_isr: unsafe fn(_, _, _, _) -> _ = xStreamBufferReceiveFromISR;

    let event_group = xEventGroupCreateStatic(core::ptr::addr_of_mut!(EVENT_CONTROL));
    assert!(!event_group.is_null());
    EVENT_GROUP.store(event_group, Ordering::Release);

    let zero_timer = xTimerCreateStatic(
        b"zero\0".as_ptr(),
        0,
        pdTRUE,
        ptr::null_mut(),
        timer_callback,
        core::ptr::addr_of_mut!(ZERO_TIMER_CONTROL),
    );
    ZERO_TIMER_REJECTED.store(zero_timer.is_null() as u32, Ordering::Relaxed);

    let timer = xTimerCreateStatic(
        b"oneshot\0".as_ptr(),
        5,
        pdFALSE,
        ptr::null_mut(),
        timer_callback,
        core::ptr::addr_of_mut!(TIMER_CONTROL),
    );
    assert!(!timer.is_null());
    ZERO_CHANGE_REJECTED.store(
        (xTimerChangePeriod(timer, 0, 0) != pdPASS) as u32,
        Ordering::Relaxed,
    );
    assert_eq!(xTimerStart(timer, 0), pdPASS);

    unsafe {
        spawn_static(
            b"stream-rx\0",
            RECEIVER_STACK.as_mut_ptr(),
            STACK_WORDS,
            core::ptr::addr_of_mut!(RECEIVER_TCB),
            RECEIVER_PRIORITY,
            receiver,
        );
        spawn_static(
            b"controller\0",
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
