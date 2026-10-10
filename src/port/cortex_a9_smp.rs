// SPDX-License-Identifier: MIT
//! Two-core Cortex-A9 exclusion and reschedule interrupts.
//!
//! [AMENDMENT] One recursive, core-owned kernel lock protects both task and ISR
//! kernel state. Local IRQs are masked while changing ownership/depth. Scheduler
//! suspension retains this lock but permits local ISRs to pend wakeups/ticks;
//! other cores may execute application code but cannot enter the kernel.
//! The BSP must provide coherent, shareable RAM and reserve GIC SGI 0 for yields.

use super::*;
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const CORES: usize = configNUMBER_OF_CORES as usize;
const NO_OWNER: u32 = u32::MAX;
const GIC_DISTRIBUTOR_BASE: usize = configINTERRUPT_CONTROLLER_BASE_ADDRESS + 0x1000;
const GIC_CPU_INTERFACE_BASE: usize = GIC_CPU_INTERFACE_ADDRESS;
pub const portYIELD_SGI: u32 = 0;
static KERNEL_LOCK: AtomicU32 = AtomicU32::new(NO_OWNER);
static RESCHEDULE: AtomicU32 = AtomicU32::new(0);
static SWITCH_COUNT: [AtomicU32; CORES] = [const { AtomicU32::new(0) }; CORES];
static SCHEDULER_READY: AtomicBool = AtomicBool::new(false);
static mut LOCK_DEPTH: [u32; CORES] = [0; CORES];
static mut CRITICAL_MASK: [u32; CORES] = [0; CORES];

#[no_mangle]
pub static mut ulCriticalNesting: [u32; CORES] = [0; CORES];
#[no_mangle]
pub static mut ulPortTaskHasFPUContext: [u32; CORES] = [0; CORES];
#[no_mangle]
pub static mut ulPortYieldRequired: [u32; CORES] = [0; CORES];
#[no_mangle]
pub static mut ulPortInterruptNesting: [u32; CORES] = [0; CORES];

#[inline(always)]
pub fn portDISABLE_INTERRUPTS() -> u32 {
    let saved;
    unsafe {
        asm!("mrs {}, cpsr", "cpsid i", out(reg) saved, options(nostack));
    }
    saved
}

#[inline(always)]
pub fn portRESTORE_INTERRUPTS(saved: u32) {
    if saved & 0x80 == 0 {
        unsafe {
            asm!("cpsie i", options(nostack));
        }
    }
}

#[inline(always)]
pub fn portENABLE_INTERRUPTS() {
    unsafe {
        asm!("cpsie i", options(nostack));
    }
}

unsafe fn prvLock() -> usize {
    loop {
        let core = portGET_CORE_ID() as usize;
        if KERNEL_LOCK.load(Ordering::Acquire) == core as u32 {
            LOCK_DEPTH[core] += 1;
            return core;
        }
        let mode: u32;
        asm!("mrs {}, cpsr", out(reg) mode, options(nomem, nostack));
        if mode & 0x1f == 0x1f
            && ulPortInterruptNesting[core] == 0
            && RESCHEDULE.load(Ordering::Acquire) & (1 << core) != 0
        {
            // A remote suspend/delete may race this task masking IRQs to
            // enter the kernel. Service its reschedule before letting the
            // task mutate any lists, even if the SGI has not arrived yet.
            // Re-read MPIDR after SVC: this task may resume on another CPU.
            asm!("svc 0", options(nostack));
            continue;
        }
        if KERNEL_LOCK
            .compare_exchange(NO_OWNER, core as u32, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            // A request published by the previous owner must also be
            // checked after acquiring the lock, before entering the API.
            if mode & 0x1f == 0x1f
                && ulPortInterruptNesting[core] == 0
                && RESCHEDULE.load(Ordering::Acquire) & (1 << core) != 0
            {
                KERNEL_LOCK.store(NO_OWNER, Ordering::Release);
                continue;
            }
            LOCK_DEPTH[core] = 1;
            return core;
        }
        core::hint::spin_loop();
    }
}

pub fn portACKNOWLEDGE_YIELD() {
    // The context-switch path holds the kernel lock, so another core cannot
    // publish a new scheduler request until selection is complete.
    RESCHEDULE.fetch_and(!(1 << portGET_CORE_ID()), Ordering::AcqRel);
}

pub fn portRECORD_CONTEXT_SWITCH() {
    // Called after selection, while still holding the kernel lock.
    SWITCH_COUNT[portGET_CORE_ID() as usize].fetch_add(1, Ordering::Release);
}

pub fn portGET_CONTEXT_SWITCH_COUNT(core: usize) -> u32 {
    SWITCH_COUNT[core].load(Ordering::Acquire)
}

pub fn portASSERT_CAN_WAIT_FOR_CORE() {
    // The API's own critical section must be the only lock held. An outer
    // critical section/scheduler suspension would prevent the remote switch.
    unsafe {
        let core = portGET_CORE_ID() as usize;
        configASSERT(LOCK_DEPTH[core] == 1 && ulCriticalNesting[core] == 1);
    }
}

pub fn portWAIT_FOR_CONTEXT_SWITCH(core: usize, previous: u32) {
    // Wait without holding the kernel lock. In particular, allow a remote
    // ISR already waiting for that lock to finish before its task is evicted.
    while SWITCH_COUNT[core].load(Ordering::Acquire) == previous {
        core::hint::spin_loop();
    }
}

unsafe fn prvUnlock(core: usize) {
    configASSERT(LOCK_DEPTH[core] != 0);
    LOCK_DEPTH[core] -= 1;
    if LOCK_DEPTH[core] == 0 {
        KERNEL_LOCK.store(NO_OWNER, Ordering::Release);
    }
}

pub fn portGET_TASK_LOCK() {
    let saved = portDISABLE_INTERRUPTS();
    unsafe {
        prvLock();
    }
    portRESTORE_INTERRUPTS(saved);
}

pub fn portRELEASE_TASK_LOCK() {
    portRELEASE_ISR_LOCK();
    prvYieldIfPending();
}

pub fn portGET_ISR_LOCK() {
    portGET_TASK_LOCK();
}

pub fn portKERNEL_LOCK_HELD() -> bool {
    // Called only with local IRQs masked by the context-switch path.
    unsafe { LOCK_DEPTH[portGET_CORE_ID() as usize] != 0 }
}

pub fn portRELEASE_ISR_LOCK() {
    let saved = portDISABLE_INTERRUPTS();
    unsafe {
        prvUnlock(portGET_CORE_ID() as usize);
    }
    portRESTORE_INTERRUPTS(saved);
}

#[no_mangle]
pub extern "C" fn vPortEnterCritical() {
    let saved = portDISABLE_INTERRUPTS();
    unsafe {
        configASSERT(ulPortInterruptNesting[portGET_CORE_ID() as usize] == 0);
        let core = prvLock();
        if ulCriticalNesting[core] == 0 {
            CRITICAL_MASK[core] = saved;
        }
        ulCriticalNesting[core] += 1;
    }
}

#[no_mangle]
pub extern "C" fn vPortExitCritical() {
    unsafe {
        let core = portGET_CORE_ID() as usize;
        configASSERT(ulCriticalNesting[core] != 0);
        ulCriticalNesting[core] -= 1;
        prvUnlock(core);
        if ulCriticalNesting[core] == 0 {
            portRESTORE_INTERRUPTS(CRITICAL_MASK[core]);
            prvYieldIfPending();
        }
    }
}

pub fn portSET_INTERRUPT_MASK_FROM_ISR() -> UBaseType_t {
    let saved = portDISABLE_INTERRUPTS();
    unsafe {
        prvLock();
    }
    saved
}

pub fn portCLEAR_INTERRUPT_MASK_FROM_ISR(saved: UBaseType_t) {
    unsafe {
        prvUnlock(portGET_CORE_ID() as usize);
    }
    portRESTORE_INTERRUPTS(saved);
}

#[no_mangle]
pub extern "C" fn ulPortSetInterruptMask() -> u32 {
    portSET_INTERRUPT_MASK_FROM_ISR()
}
#[no_mangle]
pub extern "C" fn vPortClearInterruptMask(saved: u32) {
    portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
}

fn prvYieldIfPending() {
    let saved = portDISABLE_INTERRUPTS();
    unsafe {
        let core = portGET_CORE_ID() as usize;
        if LOCK_DEPTH[core] == 0
            && ulPortInterruptNesting[core] == 0
            && ulPortYieldRequired[core] != 0
            && SCHEDULER_READY.load(Ordering::Acquire)
        {
            ulPortYieldRequired[core] = 0;
            // Enter SVC masked. The saved task CPSR is restored on return.
            asm!("svc 0", options(nostack));
        }
    }
    portRESTORE_INTERRUPTS(saved);
}

#[inline(always)]
pub fn portYIELD() {
    portYIELD_FROM_ISR(pdTRUE);
    prvYieldIfPending();
}

pub fn portYIELD_FROM_ISR(required: BaseType_t) {
    if required != pdFALSE {
        let saved = portDISABLE_INTERRUPTS();
        unsafe {
            ulPortYieldRequired[portGET_CORE_ID() as usize] = 1;
        }
        portRESTORE_INTERRUPTS(saved);
    }
}

pub fn portYIELD_CORE(core: BaseType_t) {
    configASSERT(core >= 0 && (core as usize) < CORES);
    if core == portGET_CORE_ID() {
        portYIELD_FROM_ISR(pdTRUE);
    } else {
        RESCHEDULE.fetch_or(1 << core, Ordering::Release);
        // Publish all ready-list/context writes before sending the remote SGI.
        unsafe {
            asm!("dmb ish", options(nostack));
            core::ptr::write_volatile(
                (GIC_DISTRIBUTOR_BASE + 0xf00) as *mut u32,
                (1 << (16 + core as u32)) | portYIELD_SGI,
            );
            asm!("dsb ish", options(nostack));
        }
    }
}

#[no_mangle]
pub extern "C" fn vPortYieldInterruptHandler() {
    portYIELD_FROM_ISR(pdTRUE);
}

#[no_mangle]
pub extern "C" fn FreeRTOS_Tick_Handler() {
    // Only core zero owns the timebase; remote SGIs perform time slicing.
    configASSERT(portGET_CORE_ID() == 0);
    let saved = portSET_INTERRUPT_MASK_FROM_ISR();
    let required = unsafe { crate::kernel::tasks::xTaskIncrementTick() };
    portYIELD_FROM_ISR(required);
    portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
}

#[no_mangle]
pub extern "C" fn vPortTaskUsesFPU() {
    let saved = portDISABLE_INTERRUPTS();
    unsafe {
        ulPortTaskHasFPUContext[portGET_CORE_ID() as usize] = 1;
        asm!("vmsr fpscr, {}", in(reg) 0_u32, options(nostack));
    }
    portRESTORE_INTERRUPTS(saved);
}

pub fn xPortIsInsideInterrupt() -> BaseType_t {
    let saved = portDISABLE_INTERRUPTS();
    let inside = unsafe { ulPortInterruptNesting[portGET_CORE_ID() as usize] != 0 };
    portRESTORE_INTERRUPTS(saved);
    if inside {
        pdTRUE
    } else {
        pdFALSE
    }
}

unsafe fn prvStartCore() -> ! {
    // GIC CPU interfaces and SGI priorities are banked per CPU.
    core::ptr::write_volatile((GIC_CPU_INTERFACE_BASE + 0x00) as *mut u32, 1);
    core::ptr::write_volatile((GIC_CPU_INTERFACE_BASE + 0x04) as *mut u32, 0xff);
    core::ptr::write_volatile((GIC_CPU_INTERFACE_BASE + 0x08) as *mut u32, 0);
    core::ptr::write_volatile(
        (GIC_DISTRIBUTOR_BASE + 0x400 + portYIELD_SGI as usize) as *mut u8,
        0xa0,
    );
    vPortRestoreTaskContext();
    unreachable!()
}

#[no_mangle]
pub extern "C" fn xPortStartScheduler() -> BaseType_t {
    portDISABLE_INTERRUPTS();
    configASSERT(portGET_CORE_ID() == 0);
    SCHEDULER_READY.store(true, Ordering::Release);
    unsafe {
        asm!("sev", options(nostack));
        prvStartCore();
    }
}

/// Start core 1 after its BSP installed private exception stacks and vectors.
///
/// # Safety
/// Call once on core 1 with IRQs masked, coherent shared memory configured,
/// and FP hardware enabled. Core 0 alone must call vTaskStartScheduler.
pub unsafe fn vPortStartSecondaryCore() -> ! {
    portDISABLE_INTERRUPTS();
    configASSERT(portGET_CORE_ID() == 1);
    while !SCHEDULER_READY.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    prvStartCore()
}
