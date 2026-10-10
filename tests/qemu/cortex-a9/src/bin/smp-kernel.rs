//! Cross-core IPC, scheduler suspension, priority inheritance, and reclamation.
#![no_std]
#![no_main]

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use freertos_in_rust::kernel::{queue::*, tasks::*};
use freertos_in_rust::memory::xPortGetFreeHeapSize;
use freertos_in_rust::types::*;
use freertos_qemu_cortex_a9::{
    finish, init, start_scheduler, start_secondary, tap, timer_interrupts,
};

const WORDS: usize = 1024;
const ITEMS: u32 = 2000;
static mut STACKS: [[StackType_t; WORDS]; 3] = [[0; WORDS]; 3];
static mut TCBS: [StaticTask_t; 3] = [const { StaticTask_t::new() }; 3];
static mut HANDLES: [TaskHandle_t; 2] = [ptr::null_mut(); 2];
static mut QUEUE: QueueHandle_t = ptr::null_mut();
static mut MUTEX: QueueHandle_t = ptr::null_mut();
static COMMAND: AtomicU32 = AtomicU32::new(0);
static STAGE: AtomicU32 = AtomicU32::new(0);
static ERRORS: AtomicU32 = AtomicU32::new(0);
static COUNT: AtomicU32 = AtomicU32::new(0);
static STOP: AtomicBool = AtomicBool::new(false);
static INHERITED: AtomicBool = AtomicBool::new(false);
static SELF_DELETED: AtomicU32 = AtomicU32::new(0);
static LOW_RUNS: AtomicU32 = AtomicU32::new(0);

extern "C" fn lower_priority(_: *mut c_void) {
    loop {
        LOW_RUNS.fetch_add(1, Ordering::Relaxed);
    }
}

fn wait_stage(stage: u32) {
    let start = xTaskGetTickCount();
    while STAGE.load(Ordering::Acquire) != stage {
        assert!(xTaskGetTickCount().wrapping_sub(start) < 500);
        unsafe {
            vTaskDelay(1);
        }
    }
}

unsafe fn command(value: u32) {
    STAGE.store(0, Ordering::Release);
    COMMAND.store(value, Ordering::Release);
    assert_eq!(xTaskNotifyGive(HANDLES[1]), pdPASS);
}

extern "C" fn remote(_: *mut c_void) {
    unsafe {
        loop {
            assert!(ulTaskNotifyTake(pdTRUE, portMAX_DELAY) > 0);
            match COMMAND.load(Ordering::Acquire) {
                1 => {
                    let mut i = 0;
                    while i < ITEMS {
                        let mut value = u32::MAX;
                        if xQueueReceive(QUEUE, ptr::addr_of_mut!(value).cast(), 100) != pdPASS
                            || value != i
                        {
                            ERRORS.fetch_or(1, Ordering::Relaxed);
                        }
                        i += 1;
                    }
                }
                2 => {
                    assert_eq!(xQueueSemaphoreTake(MUTEX, 100), pdPASS);
                    STAGE.store(1, Ordering::Release);
                    let start = xTaskGetTickCount();
                    while uxTaskPriorityGet(ptr::null_mut()) != 4 {
                        assert!(xTaskGetTickCount().wrapping_sub(start) < 500);
                        core::hint::spin_loop();
                    }
                    INHERITED.store(true, Ordering::Release);
                    assert_eq!(xQueueSend(MUTEX, ptr::null(), 0), pdPASS);
                    if uxTaskPriorityGet(ptr::null_mut()) != 2 {
                        ERRORS.fetch_or(2, Ordering::Relaxed);
                    }
                }
                3 => {
                    STAGE.store(1, Ordering::Release);
                    let mut i = 0;
                    while i < ITEMS {
                        if ulTaskNotifyTake(pdFALSE, 100) == 0 {
                            ERRORS.fetch_or(4, Ordering::Relaxed);
                        }
                        i += 1;
                    }
                }
                4 => {
                    STAGE.store(1, Ordering::Release);
                    while !STOP.load(Ordering::Acquire) {
                        COUNT.fetch_add(1, Ordering::Relaxed);
                        // Race remote suspend against entry to a kernel API.
                        let _ = xTaskGetTickCount();
                    }
                }
                _ => panic!("bad test command"),
            }
            STAGE.store(2, Ordering::Release);
            xTaskNotifyGive(HANDLES[0]);
        }
    }
}

extern "C" fn delete_self(_: *mut c_void) {
    SELF_DELETED.fetch_add(1, Ordering::Release);
    unsafe {
        vTaskDelete(ptr::null_mut());
    }
    panic!("self-deleted task returned");
}

extern "C" fn delete_remote(_: *mut c_void) {
    loop {
        COUNT.fetch_add(1, Ordering::Relaxed);
        let _ = xTaskGetTickCount();
    }
}

unsafe fn spawn_dynamic(entry: TaskFunction_t, core: u32) -> TaskHandle_t {
    let mut task = ptr::null_mut();
    vTaskSuspendAll();
    assert_eq!(
        xTaskCreate(
            entry,
            b"delete\0".as_ptr(),
            512,
            ptr::null_mut(),
            3,
            &mut task
        ),
        pdPASS
    );
    vTaskCoreAffinitySet(task, 1 << core);
    xTaskResumeAll();
    task
}

extern "C" fn reporter(_: *mut c_void) {
    unsafe {
        let mut out = tap();
        command(1);
        let mut i = 0;
        while i < ITEMS {
            assert_eq!(xQueueSend(QUEUE, ptr::addr_of!(i).cast(), 100), pdPASS);
            i += 1;
        }
        assert!(ulTaskNotifyTake(pdTRUE, 500) > 0);
        out.check(
            ERRORS.load(Ordering::Relaxed) & 1 == 0,
            "cross_core_queue_blocking_preserves_order",
        );

        command(2);
        wait_stage(1);
        assert_eq!(xQueueSemaphoreTake(MUTEX, 500), pdPASS);
        assert_eq!(xQueueGetMutexHolder(MUTEX), xTaskGetCurrentTaskHandle());
        assert_eq!(xQueueSend(MUTEX, ptr::null(), 0), pdPASS);
        assert!(ulTaskNotifyTake(pdTRUE, 500) > 0);
        out.check(
            INHERITED.load(Ordering::Acquire) && ERRORS.load(Ordering::Relaxed) & 2 == 0,
            "remote_mutex_owner_inherits_and_disinherits_priority",
        );

        command(3);
        wait_stage(1);
        i = 0;
        while i < ITEMS {
            xTaskNotifyGive(HANDLES[1]);
            i += 1;
        }
        assert!(ulTaskNotifyTake(pdTRUE, 500) > 0);
        out.check(
            ERRORS.load(Ordering::Relaxed) & 4 == 0,
            "cross_core_counting_notifications_are_not_lost",
        );

        command(4);
        wait_stage(1);
        out.check(
            eTaskGetState(HANDLES[1]) == eTaskState::eRunning,
            "state_query_recognizes_remote_running_task",
        );
        out.check(
            uxTaskGetStackHighWaterMark(HANDLES[1]) == 0,
            "remote_live_stack_is_not_scanned_concurrently",
        );
        vTaskSuspend(HANDLES[1]);
        // Return itself guarantees the remote context is no longer running;
        // do not give an asynchronous-only implementation a grace period.
        let before = COUNT.load(Ordering::Relaxed);
        let suspended_on_return = eTaskGetState(HANDLES[1]) == eTaskState::eSuspended;
        vTaskDelay(5);
        out.check(
            suspended_on_return && COUNT.load(Ordering::Relaxed) == before,
            "remote_suspend_stops_execution_and_kernel_reentry",
        );
        out.check(
            uxTaskGetStackHighWaterMark(HANDLES[1]) > 0,
            "stopped_remote_stack_can_be_scanned",
        );
        vTaskResume(HANDLES[1]);
        vTaskDelay(5);
        out.check(
            COUNT.load(Ordering::Relaxed) > before,
            "remote_resume_restarts_execution",
        );
        vTaskSuspendAll();
        let lower = xTaskCreateStatic(
            lower_priority,
            b"lower\0".as_ptr(),
            WORDS,
            ptr::null_mut(),
            1,
            ptr::addr_of_mut!(STACKS[2]).cast(),
            ptr::addr_of_mut!(TCBS[2]),
        );
        assert!(!lower.is_null());
        vTaskCoreAffinitySet(lower, 2);
        xTaskResumeAll();
        vTaskDelay(3);
        let initially_starved = LOW_RUNS.load(Ordering::Relaxed) == 0;
        vTaskPrioritySet(HANDLES[1], 0);
        vTaskDelay(3);
        out.check(
            initially_starved && LOW_RUNS.load(Ordering::Relaxed) > 0,
            "lowering_remote_running_priority_reschedules_its_core",
        );
        vTaskPrioritySet(HANDLES[1], 2);
        vTaskDelay(3);
        vTaskDelete(lower);
        out.check(
            ulTaskGetRunTimeCounter(HANDLES[1]) > 0,
            "runtime_statistics_include_remote_execution",
        );
        STOP.store(true, Ordering::Release);
        assert!(ulTaskNotifyTake(pdTRUE, 500) > 0);

        // The primary tick IRQ must remain serviceable while the owner core
        // suspends scheduling; nested suspension must defer exactly its ticks.
        vTaskSuspendAll();
        vTaskSuspendAll();
        let tick = xTaskGetTickCount();
        let irqs = timer_interrupts();
        while timer_interrupts().wrapping_sub(irqs) < 4 {
            core::hint::spin_loop();
        }
        let deferred = xTaskGetTickCount() == tick;
        let nested = xTaskResumeAll() == pdFALSE && xTaskGetTickCount() == tick;
        xTaskResumeAll();
        out.check(
            deferred && nested && xTaskGetTickCount().wrapping_sub(tick) >= 4,
            "nested_scheduler_suspension_replays_pending_ticks",
        );
        taskENTER_CRITICAL();
        let one_timebase = xTaskGetTickCount() == timer_interrupts();
        taskEXIT_CRITICAL();
        out.check(one_timebase, "two_cores_share_one_tick_timebase");

        // Allocate and reclaim task stacks on both cores, then delete a task
        // while it is actively entering kernel APIs on the other CPU.
        vTaskDelay(5);
        let free = xPortGetFreeHeapSize();
        let tasks = uxTaskGetNumberOfTasks();
        i = 0;
        while i < 24 {
            spawn_dynamic(delete_self, i % 2);
            vTaskDelay(3);
            i += 1;
        }
        vTaskDelay(5);
        out.check(
            SELF_DELETED.load(Ordering::Acquire) == 24
                && uxTaskGetNumberOfTasks() == tasks
                && xPortGetFreeHeapSize() == free,
            "self_delete_reclaims_stacks_on_both_cores",
        );

        COUNT.store(0, Ordering::Relaxed);
        let task = spawn_dynamic(delete_remote, 1);
        vTaskDelay(5);
        assert!(COUNT.load(Ordering::Relaxed) > 0);
        vTaskDelete(task);
        let stopped = COUNT.load(Ordering::Relaxed);
        vTaskDelay(5);
        out.check(
            COUNT.load(Ordering::Relaxed) == stopped
                && uxTaskGetNumberOfTasks() == tasks
                && xPortGetFreeHeapSize() == free,
            "remote_delete_waits_for_context_handoff_before_reclaiming",
        );
        out.diagnostic(format_args!(
            "errors={} free={}/{} tasks={}/{} self_deletes={}",
            ERRORS.load(Ordering::Relaxed),
            xPortGetFreeHeapSize(),
            free,
            uxTaskGetNumberOfTasks(),
            tasks,
            SELF_DELETED.load(Ordering::Relaxed)
        ));
        finish(out, 14)
    }
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    init();
    unsafe {
        // A pre-start deletion must not leave the next creation using a stale
        // selected TCB. Reuse this static slot later for the priority test.
        let temporary = xTaskCreateStatic(
            lower_priority,
            b"prestart\0".as_ptr(),
            WORDS,
            ptr::null_mut(),
            1,
            ptr::addr_of_mut!(STACKS[2]).cast(),
            ptr::addr_of_mut!(TCBS[2]),
        );
        assert!(!temporary.is_null());
        vTaskDelete(ptr::null_mut());
        assert_eq!(uxTaskGetNumberOfTasks(), 0);
        assert!(xTaskGetCurrentTaskHandle().is_null());
        QUEUE = xQueueCreate(2, 4);
        MUTEX = xQueueCreateMutex(queueQUEUE_TYPE_MUTEX);
        assert!(!QUEUE.is_null() && !MUTEX.is_null());
        let entries: [TaskFunction_t; 2] = [reporter, remote];
        let priorities = [4, 2];
        let mut i = 0;
        while i < 2 {
            HANDLES[i] = xTaskCreateStatic(
                entries[i],
                b"smp-kernel\0".as_ptr(),
                WORDS,
                ptr::null_mut(),
                priorities[i],
                ptr::addr_of_mut!(STACKS[i]).cast(),
                ptr::addr_of_mut!(TCBS[i]),
            );
            assert!(!HANDLES[i].is_null());
            vTaskCoreAffinitySet(HANDLES[i], 1 << i);
            i += 1;
        }
    }
    start_secondary();
    start_scheduler()
}
