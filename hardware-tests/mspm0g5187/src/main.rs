//! Dynamic four-task queue/event-group regression for the LP-MSPM0G5187.

#![no_main]
#![no_std]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU32, Ordering};

use cortex_m_rt::entry;
use freertos_in_rust::kernel::event_groups::EventBits_t;
use freertos_in_rust::memory::FreeRtosAllocator;
use freertos_in_rust::sync::{EventGroup, Queue, TaskContext, TaskHandle};
use freertos_in_rust::{delay_ms, start_scheduler};

mod hal;
mod pac;

use hal::gpio::{self, Port, PIN13, PIN17, PIN24};

#[global_allocator]
static ALLOCATOR: FreeRtosAllocator = FreeRtosAllocator;

/// The LP-MSPM0G5187 runs from its default 32 MHz SYSOSC configuration.
#[no_mangle]
pub static FREERTOS_CONFIG_CPU_CLOCK_HZ: u32 = 32_000_000;

const TASK_STACK_WORDS: usize = 128;
const RED_DONE: EventBits_t = 1 << 0;
const GREEN_DONE: EventBits_t = 1 << 1;
const BLUE_DONE: EventBits_t = 1 << 2;
const ALL_WORKERS_DONE: EventBits_t = RED_DONE | GREEN_DONE | BLUE_DONE;

static mut EVENTS: MaybeUninit<EventGroup> = MaybeUninit::uninit();
static mut WORK_QUEUE: MaybeUninit<Queue<u32>> = MaybeUninit::uninit();

static LFSR_STATE: AtomicU32 = AtomicU32::new(0xDEAD_BEEF);

// The MSPM0 flash programmer writes 64-bit units. Keep the initialized-data
// load segment a multiple of eight bytes so UniFlash can program the ELF
// directly; LFSR_STATE accounts for the other four bytes.
#[used]
#[link_section = ".data.flash_write_alignment"]
static mut FLASH_WRITE_ALIGNMENT_PAD: u32 = 0xFFFF_FFFF;

fn lfsr_next() -> u32 {
    let mut state = LFSR_STATE.load(Ordering::Relaxed);
    let feedback = (state ^ (state >> 2) ^ (state >> 22) ^ (state >> 31)) & 1;
    state = (state >> 1) | (feedback << 31);
    LFSR_STATE.store(state, Ordering::Relaxed);
    state
}

fn work_delay_ms() -> u32 {
    200 + lfsr_next() % 1001
}

fn shared() -> (&'static EventGroup, &'static Queue<u32>) {
    // Safety: main initializes both values exactly once before scheduler
    // startup. They are never moved or destroyed, and both wrappers support
    // the cross-task shared operations used by this application.
    unsafe { (EVENTS.assume_init_ref(), WORK_QUEUE.assume_init_ref()) }
}

extern "C" fn manager_task(_parameter: *mut c_void) {
    // Safety: FreeRTOS invokes this entry only as a live task, never as an ISR.
    let context = unsafe { TaskContext::assume() };
    let (events, queue) = shared();

    loop {
        for _ in 0..3 {
            assert!(queue.send(&context, &work_delay_ms()));
        }

        events.wait_all_clear(&context, ALL_WORKERS_DONE);
        delay_ms(&context, 50);
    }
}

fn worker(port: Port, pin: u32, completion_bit: EventBits_t) -> ! {
    // Safety: each caller is a FreeRTOS task entry and retains this capability
    // only within that task's execution context.
    let context = unsafe { TaskContext::assume() };
    let (events, queue) = shared();

    loop {
        let duration_ms = queue
            .receive(&context)
            .expect("the manager supplies one item per worker");
        gpio::set_high(port, pin);
        delay_ms(&context, duration_ms);
        gpio::set_low(port, pin);
        events.sync(&context, completion_bit, ALL_WORKERS_DONE);
    }
}

extern "C" fn red_worker(_parameter: *mut c_void) {
    worker(Port::A, PIN17, RED_DONE)
}

extern "C" fn green_worker(_parameter: *mut c_void) {
    worker(Port::A, PIN24, GREEN_DONE)
}

extern "C" fn blue_worker(_parameter: *mut c_void) {
    worker(Port::B, PIN13, BLUE_DONE)
}

#[entry]
fn main() -> ! {
    gpio::enable_power();
    gpio::init_pa17_output();
    gpio::init_pa24_output();
    gpio::init_pb13_output();
    gpio::set_low(Port::A, PIN17);
    gpio::set_low(Port::A, PIN24);
    gpio::set_low(Port::B, PIN13);

    // Safety: reset has not released any task or interrupt into the kernel, so
    // main exclusively owns pre-scheduler initialization until this capability
    // is consumed by start_scheduler below.
    let context = unsafe { TaskContext::assume() };

    // Safety: this is the sole initialization of stable program-lifetime
    // storage, before any task can read it.
    unsafe {
        EVENTS.write(EventGroup::new(&context).expect("create event group"));
        WORK_QUEUE.write(Queue::new(&context, 8).expect("create work queue"));
    }

    let _red = TaskHandle::spawn(&context, b"red", TASK_STACK_WORDS, 1, red_worker)
        .expect("create red worker");
    let _green = TaskHandle::spawn(&context, b"green", TASK_STACK_WORDS, 1, green_worker)
        .expect("create green worker");
    let _blue = TaskHandle::spawn(&context, b"blue", TASK_STACK_WORDS, 1, blue_worker)
        .expect("create blue worker");
    let _manager = TaskHandle::spawn(&context, b"manager", TASK_STACK_WORDS, 2, manager_task)
        .expect("create manager");

    start_scheduler(context)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    cortex_m::interrupt::disable();
    loop {
        cortex_m::asm::nop();
    }
}
