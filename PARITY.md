# FreeRTOS source-parity checklist

Audit baseline: FreeRTOS-Kernel commit
`3ace38969bd05558918f815ae75528cb656c64b0`, as recorded in the README.
The local `FreeRTOS-Kernel/` checkout was verified at that exact commit on
2026-07-09.

Status keys:

- **P** — control flow, state changes, locking, and active configuration
  branches match the named upstream symbol.
- **H** — upstream behavior plus a documented Rust hardening check that does
  not change valid-call behavior.
- **R** — Rust-only construction, type, or allocator glue; there is no C
  function to match.
- **D** — known deliberate limitation or inactive upstream branch.

Trace/coverage calls are considered separately from kernel state semantics.
All raw-pointer APIs retain the corresponding C preconditions even where the
Rust signature is `unsafe`.

## `src/kernel/list.rs`

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `listSET_FIRST_LIST_ITEM_INTEGRITY_CHECK_VALUE`, `listSET_SECOND_LIST_ITEM_INTEGRITY_CHECK_VALUE`, `listSET_LIST_INTEGRITY_CHECK_1_VALUE`, `listSET_LIST_INTEGRITY_CHECK_2_VALUE`, `listTEST_LIST_ITEM_INTEGRITY`, `listTEST_LIST_INTEGRITY` (both cfg modules) | same-named macros in `include/list.h` | **P**; enabled and disabled definitions checked. |
| `mtCOVERAGE_TEST_DELAY`, `mtCOVERAGE_TEST_MARKER` | same-named coverage macros | **P** for the normal no-op configuration. |
| `ListItem_t::new`, `List_t::new` | none | **R**; const zero-state constructors, followed by the upstream initializers before use. |
| `listSET_LIST_ITEM_OWNER`, `listGET_LIST_ITEM_OWNER`, `listSET_LIST_ITEM_VALUE`, `listGET_LIST_ITEM_VALUE` | same-named `include/list.h` macros | **P**. |
| `listGET_ITEM_VALUE_OF_HEAD_ENTRY`, `listGET_HEAD_ENTRY`, `listGET_NEXT`, `listGET_END_MARKER` | same-named `include/list.h` macros | **P**. |
| `listLIST_IS_EMPTY`, `listCURRENT_LIST_LENGTH`, `listGET_OWNER_OF_HEAD_ENTRY` | same-named `include/list.h` macros | **P**. |
| `listIS_CONTAINED_WITHIN`, `listLIST_ITEM_CONTAINER`, `listLIST_IS_INITIALISED` | same-named `include/list.h` macros | **P**. |
| `listGET_OWNER_OF_NEXT_ENTRY` | `listGET_OWNER_OF_NEXT_ENTRY` macro | **P, fixed in this audit**; removed an extra, overwritten `head->next` dereference before the correct marker-skip assignment. |
| `vListInitialise` | `vListInitialise` in `list.c` | **P** for `configUSE_MINI_LIST_ITEM == 0`. |
| `vListInitialiseItem` | `vListInitialiseItem` | **P**. |
| `vListInsertEnd` | `vListInsertEnd` | **P**; integrity tests and index-relative insertion order checked. |
| `vListInsert` | `vListInsert` | **P**; equal-value FIFO ordering and `portMAX_DELAY` special case checked. |
| `uxListRemove` | `uxListRemove` | **P**; index repair precedes container clearing and count decrement. |
| `listREMOVE_ITEM`, `listINSERT_END` | same-named `include/list.h` macros | **P**. |

Deliberate limitation: `MiniListItem_t` is the full `ListItem_t`, and
`configUSE_MINI_LIST_ITEM` is fixed to zero. This matches the crate's active
configuration but does not implement upstream's size-optimized `== 1` layout.

## `src/kernel/queue.rs`

### Queue algorithms and APIs

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `StaticQueue_t::new` | none | **R**; const backing storage constructor. |
| `mtCOVERAGE_TEST_DELAY`, `mtCOVERAGE_TEST_MARKER`, `queueYIELD_IF_USING_PREEMPTION` | same-named macros | **P** for single-core and the crate's preemption configuration. |
| `prvLockQueue` | `prvLockQueue` macro | **P**. |
| `prvIncrementQueueTxLock`, `prvIncrementQueueRxLock` | same-named macros | **P, fixed in this audit**; counts are now capped by `uxTaskGetNumberOfTasks`, not merely by `INT8_MAX`. |
| `xQueueGenericReset` | `xQueueGenericReset` | **P/H**; upstream ordering plus checked Rust arithmetic. |
| `prvInitialiseNewQueue` | `prvInitialiseNewQueue` | **P**. |
| `xQueueGenericCreateStatic`, `xQueueGenericGetStaticBuffers` | same-named functions | **P/H**; static/dynamic allocation branches checked. |
| `xQueueGenericCreate` | `xQueueGenericCreate` | **P/H**; multiplication and addition overflow checks retained. |
| `xQueueCreate`, `xQueueCreateStatic` | same-named `include/queue.h` macros | **P**. |
| `prvCopyDataToQueue`, `prvCopyDataFromQueue` | same-named functions | **P/H**; front insertion performs the equivalent wrap before subtraction so Rust never forms an out-of-allocation pointer. |
| `prvIsQueueEmpty`, `prvIsQueueFull` | same-named functions | **P**. |
| `xQueueGenericSend` | `xQueueGenericSend` | **P, fixed in this audit**; scheduler-suspended blocking assertion restored. Queue-set overwrite and mutex-disinherit paths checked. |
| `xQueueReceive`, `xQueuePeek` | same-named functions | **P, fixed in this audit**; scheduler-state assertions and post-timeout race rechecks match upstream. |
| `xQueueReceiveFromISR`, `xQueuePeekFromISR` | same-named functions | **P** for queue/event-list state and lock accounting. See the common ISR-validation limitation below. |
| `prvUnlockQueue` | `prvUnlockQueue` | **P**; TX processing precedes RX processing and missed-yield behavior is retained. |
| `uxQueueMessagesWaiting`, `uxQueueSpacesAvailable`, `uxQueueMessagesWaitingFromISR` | same-named functions | **P** for returned values and critical sections. |
| `vQueueDelete` | `vQueueDelete` | **P, fixed in this audit**; the symbol now remains available in static-only builds, where it unregisters/traces but does not free. |
| `uxQueueGetQueueItemSize`, `uxQueueGetQueueLength` | same-named functions | **P/H**; Rust also asserts a non-null handle. |
| `xQueueIsQueueEmptyFromISR`, `xQueueIsQueueFullFromISR` | same-named functions | **P**. |
| `uxQueueGetQueueNumber`, `vQueueSetQueueNumber`, `ucQueueGetQueueType` | same-named functions | **P** under `trace-facility`. |
| `xQueueSendToBack`, `xQueueSendToFront`, `xQueueSend`, `xQueueOverwrite` | same-named `include/queue.h` macros | **P**. |
| `xQueueSendToBackFromISR`, `xQueueSendToFrontFromISR`, `xQueueOverwriteFromISR`, `xQueueSendFromISR` | same-named `include/queue.h` macros | **P**. |
| `xQueueGenericSendFromISR` | `xQueueGenericSendFromISR` | **P** for queue/set state, wake flag, overwrite handling, and capped TX lock. |
| `xQueueGiveFromISR` | `xQueueGiveFromISR` | **P, fixed in this audit**; a locked queue-set member now defers notification through its TX lock instead of modifying the set immediately. |
| `vQueueWaitForMessageRestricted` | `vQueueWaitForMessageRestricted` | **P**; compiled only with `timers`, as upstream does. |
| `xQueueCreateMutex`, `xQueueCreateMutexStatic`, `prvInitialiseMutex` | same-named functions | **P, fixed in this audit**; dynamic and static creation share the upstream initializer and trace ordering. |
| `xQueueGetMutexHolder`, `xQueueGetMutexHolderFromISR` | same-named functions | **P**. |
| `xQueueGiveMutexRecursive`, `xQueueTakeMutexRecursive` | same-named functions | **P** for ownership, recursion count, and give/take state. |
| `xQueueSemaphoreTake` | `xQueueSemaphoreTake` | **P, fixed in this audit**; scheduler-state assertion restored; inheritance, timeout disinheritance, and race recheck were checked. |
| `prvGetHighestPriorityOfWaitToReceiveList` | same-named function | **P**. |
| `xQueueCreateCountingSemaphore`, `xQueueCreateCountingSemaphoreStatic` | same-named functions | **P** for valid inputs; crate assertions are always enabled for invalid inputs. |
| `prvNotifyQueueSetContainer` | same-named function | **P**; set TX locking now uses the upstream task-count cap. |
| `xQueueCreateSet`, `xQueueCreateSetStatic` | same-named functions | **P, fixed in this audit**; dynamic creation is exposed for `alloc`, `heap-4`, and `heap-5`, not only `alloc`. |
| `xQueueAddToSet`, `xQueueRemoveFromSet`, `xQueueSelectFromSet` | same-named functions | **P**. |
| `xQueueSelectFromSetFromISR` | `xQueueSelectFromSetFromISR` | **P, fixed in this audit**; now delegates to `xQueueReceiveFromISR`, preserving RX lock/event semantics. |
| `QueueRegistryItem::new` | none | **R**; const zero-state registry slot. |
| `vQueueAddToRegistry`, `pcQueueGetName`, `vQueueUnregisterQueue` | same-named functions | **P**. |

### Local trace-hook functions

The following Rust functions map to same-named upstream trace macros and are
currently no-op defaults (**P** for upstream's default trace configuration):

`traceENTER_xQueueGenericReset`, `traceRETURN_xQueueGenericReset`,
`traceENTER_xQueueGenericCreateStatic`,
`traceRETURN_xQueueGenericCreateStatic`,
`traceENTER_xQueueGenericGetStaticBuffers`,
`traceRETURN_xQueueGenericGetStaticBuffers`,
`traceENTER_xQueueGenericCreate`, `traceRETURN_xQueueGenericCreate`,
`traceENTER_xQueueGenericSend`, `traceRETURN_xQueueGenericSend`,
`traceENTER_xQueueReceive`, `traceRETURN_xQueueReceive`,
`traceENTER_xQueueCreateMutex`, `traceRETURN_xQueueCreateMutex`,
`traceENTER_xQueueCreateMutexStatic`,
`traceRETURN_xQueueCreateMutexStatic`, `traceCREATE_MUTEX`,
`traceCREATE_MUTEX_FAILED`, `traceENTER_xQueueGetMutexHolder`,
`traceRETURN_xQueueGetMutexHolder`,
`traceENTER_xQueueGetMutexHolderFromISR`,
`traceRETURN_xQueueGetMutexHolderFromISR`,
`traceENTER_xQueueGiveMutexRecursive`,
`traceRETURN_xQueueGiveMutexRecursive`, `traceGIVE_MUTEX_RECURSIVE`,
`traceGIVE_MUTEX_RECURSIVE_FAILED`,
`traceENTER_xQueueTakeMutexRecursive`,
`traceRETURN_xQueueTakeMutexRecursive`, `traceTAKE_MUTEX_RECURSIVE`,
`traceTAKE_MUTEX_RECURSIVE_FAILED`,
`traceENTER_xQueueCreateCountingSemaphore`,
`traceRETURN_xQueueCreateCountingSemaphore`,
`traceENTER_xQueueCreateCountingSemaphoreStatic`,
`traceRETURN_xQueueCreateCountingSemaphoreStatic`,
`traceCREATE_COUNTING_SEMAPHORE`,
`traceCREATE_COUNTING_SEMAPHORE_FAILED`, `traceENTER_xQueueGiveFromISR`,
`traceRETURN_xQueueGiveFromISR`, `traceQUEUE_SEND_FROM_ISR`,
`traceQUEUE_SEND_FROM_ISR_FAILED`, `traceENTER_xQueuePeek`,
`traceRETURN_xQueuePeek`, `traceQUEUE_PEEK`, `traceQUEUE_PEEK_FAILED`,
`traceBLOCKING_ON_QUEUE_PEEK`, `traceENTER_xQueueReceiveFromISR`,
`traceRETURN_xQueueReceiveFromISR`, `traceQUEUE_RECEIVE_FROM_ISR`,
`traceQUEUE_RECEIVE_FROM_ISR_FAILED`, `traceENTER_xQueuePeekFromISR`,
`traceRETURN_xQueuePeekFromISR`, `traceQUEUE_PEEK_FROM_ISR`,
`traceQUEUE_PEEK_FROM_ISR_FAILED`,
`traceENTER_uxQueueMessagesWaitingFromISR`,
`traceRETURN_uxQueueMessagesWaitingFromISR`, `traceENTER_vQueueDelete`,
`traceRETURN_vQueueDelete`, `traceQUEUE_DELETE`,
`traceENTER_xQueueIsQueueEmptyFromISR`,
`traceRETURN_xQueueIsQueueEmptyFromISR`,
`traceENTER_xQueueIsQueueFullFromISR`,
`traceRETURN_xQueueIsQueueFullFromISR`,
`traceENTER_uxQueueGetQueueNumber`, `traceRETURN_uxQueueGetQueueNumber`,
`traceENTER_vQueueSetQueueNumber`, `traceRETURN_vQueueSetQueueNumber`,
`traceENTER_ucQueueGetQueueType`, `traceRETURN_ucQueueGetQueueType`, and
`traceQUEUE_SET_SEND`.

Registry-local no-op mappings are `traceENTER_vQueueAddToRegistry`,
`traceRETURN_vQueueAddToRegistry`, `traceQUEUE_REGISTRY_ADD`,
`traceENTER_pcQueueGetName`, `traceRETURN_pcQueueGetName`,
`traceENTER_vQueueUnregisterQueue`, and `traceRETURN_vQueueUnregisterQueue`.

Known queue limitations:

- **D:** Upstream co-routine-only functions `xQueueCRSend`,
  `xQueueCRReceive`, `xQueueCRSendFromISR`, and `xQueueCRReceiveFromISR` are not
  translated; the crate exposes no `configUSE_CO_ROUTINES` capability.
- **D:** The port abstraction has no equivalent of
  `portASSERT_IF_INTERRUPT_PRIORITY_INVALID()`. FromISR queue algorithms match,
  but ports cannot currently diagnose an ISR running above the permitted API
  priority.
- **D:** Local trace hooks are fixed no-ops. Kernel state matches the default
  upstream configuration, but complete user-replaceable trace instrumentation
  is not yet available, and several non-state-changing ENTER/RETURN call sites
  remain absent.

## `src/kernel/event_groups.rs`

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `eventCLEAR_EVENTS_ON_EXIT_BIT`, `eventUNBLOCKED_DUE_TO_BIT_SET`, `eventWAIT_FOR_ALL_BITS`, `eventEVENT_BITS_CONTROL_BYTES` | same-named constants in `include/event_groups.h` | **P** for 32- and 64-bit ticks; each control byte occupies the top eight bits. |
| `EventBits_t`, `EventGroup_t`, `StaticEventGroup_t`, `StaticList_t` | `EventBits_t`, `EventGroup_t`, `StaticEventGroup_t` | **P/R**; field cfgs match static/dynamic allocation and trace support, while const constructors and compile-time layout checks are Rust glue. |
| `mtCOVERAGE_TEST_MARKER` | same-named coverage macro | **P** for the normal no-op configuration. |
| `xEventGroupCreate` | `xEventGroupCreate` | **P/H**; allocation, initialization, trace, and failure paths match. The raw constructor is now `unsafe` because task context, allocator state, and eventual unique deletion are external invariants. |
| `xEventGroupCreateStatic` | `xEventGroupCreateStatic` | **P/H** for valid storage; Rust constructs the object with `ptr::write` and returns null for a null buffer before dereference. |
| `xEventGroupWaitBits` | `xEventGroupWaitBits` | **P, fixed in this audit**; restored the assertion that a blocking wait cannot begin while the scheduler is already suspended. Immediate-match, timeout, event-list value, clear-on-exit, and resume/yield paths were checked. |
| `xEventGroupSync` | `xEventGroupSync` | **P/H, fixed in this audit**; restored the scheduler-state assertion. Rust also rejects control bits in `uxBitsToSet` at the public boundary; upstream diagnoses them in the nested set operation. |
| `xEventGroupSetBits` | `xEventGroupSetBits` | **P**; waiter traversal, OR/AND matching, deferred clear mask, unordered-list removal, and scheduler suspension pairing were checked. |
| `xEventGroupClearBits` | `xEventGroupClearBits` | **P**; the critical section and pre-clear return value match. |
| `xEventGroupGetBitsFromISR` | `xEventGroupGetBitsFromISR` | **P/H**; interrupt-mask sampling matches and Rust additionally asserts a non-null handle. |
| `xEventGroupGetBits` | `xEventGroupGetBits` macro | **P**; delegates to `xEventGroupClearBits(handle, 0)`. |
| `vEventGroupDelete` | `vEventGroupDelete` | **P**; waiter unblocking, scheduler pairing, trace, and allocation-origin-dependent free were checked. |
| `xEventGroupGetStaticBuffer` | `xEventGroupGetStaticBuffer` | **P/H**; allocation-origin result and output pointer match, with explicit live-handle/output-pointer contracts. |
| `vEventGroupSetBitsCallback`, `vEventGroupClearBitsCallback` | same-named functions | **P/D**; callback dispatch matches. On 64-bit ticks the second parameter deliberately differs from upstream by widening with `PendedFunctionParameter_t` so all 56 advertised event bits survive deferral. |
| `xEventGroupSetBitsFromISR`, `xEventGroupClearBitsFromISR` | same-named functions | **P/H** for valid calls; both pend the matching daemon callback. Rust rejects null groups and reserved control bits before queueing. |
| `uxEventGroupGetNumber`, `vEventGroupSetNumber` | same-named functions | **P/H** under `trace-facility`; Rust adds live-handle assertions. |
| `prvTestWaitCondition` | `prvTestWaitCondition` | **P** for zero/nonzero OR waits and exact-mask AND waits. |

The local no-op ENTER/RETURN functions map one-for-one to these upstream trace
macros: `traceENTER_xEventGroupCreate`, `traceRETURN_xEventGroupCreate`,
`traceENTER_xEventGroupCreateStatic`,
`traceRETURN_xEventGroupCreateStatic`, `traceENTER_xEventGroupWaitBits`,
`traceRETURN_xEventGroupWaitBits`, `traceENTER_xEventGroupSync`,
`traceRETURN_xEventGroupSync`, `traceENTER_xEventGroupSetBits`,
`traceRETURN_xEventGroupSetBits`, `traceENTER_xEventGroupClearBits`,
`traceRETURN_xEventGroupClearBits`,
`traceENTER_xEventGroupGetBitsFromISR`,
`traceRETURN_xEventGroupGetBitsFromISR`, `traceENTER_vEventGroupDelete`,
`traceRETURN_vEventGroupDelete`, `traceENTER_xEventGroupGetStaticBuffer`,
`traceRETURN_xEventGroupGetStaticBuffer`,
`traceENTER_vEventGroupSetBitsCallback`,
`traceRETURN_vEventGroupSetBitsCallback`,
`traceENTER_vEventGroupClearBitsCallback`,
`traceRETURN_vEventGroupClearBitsCallback`,
`traceENTER_xEventGroupSetBitsFromISR`,
`traceRETURN_xEventGroupSetBitsFromISR`,
`traceENTER_xEventGroupClearBitsFromISR`,
`traceRETURN_xEventGroupClearBitsFromISR`,
`traceENTER_uxEventGroupGetNumber`,
`traceRETURN_uxEventGroupGetNumber`, `traceENTER_vEventGroupSetNumber`, and
`traceRETURN_vEventGroupSetNumber`. They are **P** for upstream's default
no-op trace configuration.

Known event-group limitations:

- **D:** The crate supports only 32- and 64-bit tick ports, so upstream's
  16-bit-tick event-control masks and eight-user-bit configuration are not
  exposed.
- **D:** FromISR algorithms cannot call upstream's
  `portASSERT_IF_INTERRUPT_PRIORITY_INVALID()` because the common port trait
  has no equivalent validation hook.
- **D:** Trace hooks are fixed defaults rather than fully user-replaceable
  macros.

## `src/kernel/timers.rs`

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| timer status bits, command IDs, `tmrNO_DELAY`, `tmrMAX_TIME_BEFORE_OVERFLOW` | same constants in `timers.c` and `include/timers.h` | **P**. Command zero remains defined and queueable but is intentionally not handled by the pinned daemon switch. |
| `TimerCallbackFunction_t`, `Timer_t`, `TimerParameter_t`, `DaemonTaskMessage_t` | same-named types | **P/R**; repr-C layout and active cfg fields match, with Rust unsafe callback types expressing the live-handle invariant. |
| `PendedFunctionParameter_t`, `PendedFunction_t`, `CallbackParameters_t` | `uint32_t`, `PendedFunction_t`, `CallbackParameters_t` | **P/D**; 32-bit ports match exactly. The parameter deliberately differs as `u64` on 64-bit-tick ports so deferred event-group operations do not truncate advertised event bits. |
| `DaemonTaskMessageUnion` | anonymous union in `DaemonTaskMessage_t` | **P, fixed in this audit**; `xCallbackParameters` now exists only with `pend-function-call`, matching `INCLUDE_xTimerPendFunctionCall`. |
| `StaticTimer_t::new` and layout assertions | `StaticTimer_t` | **R/P**; `MaybeUninit<Timer_t>` preserves exact size/alignment without inventing a live timer. |
| `vTimerSetTimerTaskMemory` | `vApplicationGetTimerTaskMemory` integration point | **R**; explicit pre-scheduler registration replaces the C application hook while preserving static task-memory ownership. |
| `xTimerCreateTimerTask` | `xTimerCreateTimerTask` | **P/R/H**; queue initialization and static/dynamic daemon creation match active crate configurations. The Rust registration veneer replaces the application hook, failure is returned instead of followed by upstream's final assert, and the raw single-initialization constructor is now `unsafe`. |
| `xTimerCreate`, `xTimerCreateStatic` | same-named functions | **P/H**; initialization, allocation flags, and trace order match for valid inputs. Rust rejects null callbacks/storage and zero periods before constructing state. |
| `xTimerGenericCommandFromTask`, `xTimerGenericCommandFromISR` | same-named functions | **P/H** for public command families; queue selection, scheduler-start delay behavior, and trace match. Rust rejects unknown IDs and zero change-period values instead of allowing a later daemon assertion or no-op. |
| `xTimerGenericCommand` | same-named `include/timers.h` macro | **P**; dispatch threshold is `tmrFIRST_FROM_ISR_COMMAND`. |
| `xTimerGetTimerDaemonTaskHandle` | same-named function | **P**. |
| `xTimerGetPeriod`, `vTimerSetReloadMode`, `xTimerGetReloadMode`, `uxTimerGetReloadMode`, `xTimerGetExpiryTime` | same-named functions | **P**; status-bit and list-item reads were checked. |
| `pcTimerGetName`, `xTimerIsTimerActive`, `pvTimerGetTimerID`, `vTimerSetTimerID` | same-named functions | **P**. |
| `xTimerGetStaticBuffer` | `xTimerGetStaticBuffer` | **P/H**; static-origin result matches and Rust adds a live timer assertion. |
| `vTimerResetState` | `vTimerResetState` | **P**; queue/task handles are reset without changing timer lists or last sampled time, matching the pinned source. |
| `uxTimerGetTimerNumber`, `vTimerSetTimerNumber` | same-named functions | **P** under `trace-facility`. |
| `xTimerPendFunctionCallFromISR`, `xTimerPendFunctionCall` | same-named functions | **P/H** for valid calls; message IDs, parameters, queue APIs, and delays match. Rust fails cleanly if the timer queue is not initialized and exposes callback/data lifetime through `unsafe`. |
| `prvInitialiseNewTimer` | `prvInitialiseNewTimer` | **P/R**; field/status initialization and list-owner setup match, using `ptr::write` to begin Rust object lifetime. |
| `prvCheckForValidListAndQueue` | `prvCheckForValidListAndQueue` | **P, fixed in this audit**; active/overflow lists, static command queue, and critical section match the crate's static-allocation configuration. `queue-registry` now registers the pinned `"TmrQ"` name. |
| `prvTimerTask` | `prvTimerTask` | **P**; hook, next-expiry processing, and command draining order match. |
| `prvGetNextExpireTime`, `prvSampleTimeNow`, `prvSwitchTimerLists` | same-named functions | **P**; empty-list reporting, tick-overflow detection, list swap, expired callback, and auto-reload behavior were checked. |
| `prvProcessTimerOrBlockTask` | `prvProcessTimerOrBlockTask` | **P/R**; scheduler suspend/resume ownership is now explicitly unsafe and paired on every branch. Passing `portMAX_DELAY` directly for two empty lists is equivalent to upstream's conversion inside `vTaskPlaceOnEventListRestricted`. |
| `prvInsertTimerInActiveList`, `prvReloadTimer`, `prvProcessExpiredTimer` | same-named functions | **P**; overflow/current-list selection, catch-up reload loop, active status, callback order, and next reload insertion were checked. |
| `prvProcessReceivedCommands` | `prvProcessReceivedCommands` | **P, fixed in this audit**; callback/timer split and all command cases match. Removed an added `tmrCOMMAND_START_DONT_TRACE` arm because the pinned switch intentionally ignores command zero. |
| `xTimerStart`, `xTimerStop`, `xTimerChangePeriod`, `xTimerDelete`, `xTimerReset` | same-named `include/timers.h` macros | **P**. |
| `xTimerStartFromISR`, `xTimerStopFromISR`, `xTimerChangePeriodFromISR`, `xTimerResetFromISR` | same-named macros | **P**. |

Timer ENTER/RETURN and event trace macros are imported from the crate trace
layer. Their argument values and state-changing call positions were checked;
they are **P** for the default no-op configuration. This includes create,
generic command, query, pended-call, `traceTIMER_CREATE`,
`traceTIMER_COMMAND_SEND`, `traceTIMER_COMMAND_RECEIVED`, and
`traceTIMER_EXPIRED` mappings.

Known timer limitations:

- **D:** SMP-only timer task core-affinity creation is not present on this
  single-core mainline.
- **D:** `configUSE_DAEMON_TASK_STARTUP_HOOK` has no crate hook/capability.
- **D:** The application static-memory callback is represented by the explicit
  Rust `vTimerSetTimerTaskMemory` registration API rather than a replaceable C
  hook.
- **D:** User-replaceable trace macros and the common FromISR interrupt-priority
  assertion remain unavailable.

## `src/kernel/stream_buffer.rs`

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `sbTYPE_STREAM_BUFFER`, `sbTYPE_MESSAGE_BUFFER`, `sbTYPE_STREAM_BATCHING_BUFFER`, flag values, `sbBYTES_TO_STORE_MESSAGE_LENGTH` | same constants/macros | **P** for the crate's `u32` message-length configuration. |
| `StreamBufferHandle_t`, `StreamBufferCallbackFunction_t`, `StreamBuffer_t` | same-named types | **P/R**; fields and callback configuration match `configUSE_SB_COMPLETED_CALLBACK == 1`. Atomic head/tail/waiter fields are Rust memory-model glue for the documented single-producer/single-consumer handoff. |
| `StaticStreamBuffer_t::new`, `StaticMessageBuffer_t` | `StaticStreamBuffer_t`, `StaticMessageBuffer_t` | **R/P, fixed in this audit**; aligned `MaybeUninit` storage and the message-buffer alias preserve upstream layout. |
| `xStreamBufferGenericCreate` | `xStreamBufferGenericCreate` | **P/H**; dynamic sentinel byte, type flags, callbacks, initialization, and trace-visible state match. Rust checked arithmetic and early invalid-size rejection prevent allocation overflow. |
| `xStreamBufferGenericCreateStatic` | `xStreamBufferGenericCreateStatic` | **P/H**; static capacity has no added sentinel, flags/callbacks match, and invalid pointers/sizes are rejected before construction. |
| `xStreamBufferCreate`, `xStreamBufferCreateWithCallback`, `xStreamBufferCreateStatic`, `xStreamBufferCreateStaticWithCallback` | same-named header macros | **P, completed in this audit**; missing callback/static veneers were added. |
| `xStreamBatchingBufferCreate`, `xStreamBatchingBufferCreateWithCallback`, `xStreamBatchingBufferCreateStatic`, `xStreamBatchingBufferCreateStaticWithCallback` | same-named header macros | **P, completed in this audit**. |
| `xMessageBufferCreate`, `xMessageBufferCreateWithCallback`, `xMessageBufferCreateStatic`, `xMessageBufferCreateStaticWithCallback` | same-named `message_buffer.h` macros | **P, completed in this audit**; trigger zero is passed literally and normalized by the generic initializer. |
| `vStreamBufferDelete` | `vStreamBufferDelete` | **P, fixed in this audit**; dynamic objects free and static objects are cleared. Static-only builds now take upstream's deliberately failing assertion if a corrupt object claims dynamic origin. |
| `xStreamBufferReset`, `xStreamBufferResetFromISR` | same-named functions | **P/R**; waiter exclusion, callback/trace preservation, interrupt/task critical sections, and reinitialization match. Atomic waiter loads express the same exclusion. |
| `xStreamBufferBytesAvailable`, `prvBytesInBuffer` | same-named functions | **P/R**; circular distance is unchanged and acquire loads publish copied bytes. |
| `xStreamBufferSpacesAvailable` | `xStreamBufferSpacesAvailable` | **D/R**; circular arithmetic matches, but Rust takes one acquire snapshot instead of upstream's retry if the consumer moves the tail during sampling. A stale tail can only conservatively under-report writer space. |
| `xStreamBufferIsEmpty`, `xStreamBufferIsFull`, `xStreamBufferSetTriggerLevel` | same-named functions | **P**. |
| `xStreamBufferNextMessageLengthBytes` | same-named function | **P, fixed in this audit**; restored upstream's assertion that a visible nonzero fragment cannot contain only a partial/equal-length prefix. |
| `xStreamBufferSend`, `xStreamBufferSendFromISR` | same-named functions | **P/H/R**; blocking, notification, message/stream capacity, partial stream writes, and callback paths match. Checked arithmetic and atomic publication harden the Rust boundary. |
| `xStreamBufferReceive`, `xStreamBufferReceiveFromISR` | same-named functions | **P/H/R**; batching wait rules, message truncation behavior, stream reads, notification, and callback paths match with atomic publication. |
| `uxStreamBufferGetStreamBufferNotificationIndex`, `vStreamBufferSetStreamBufferNotificationIndex` | same-named functions | **P/R**; waiter assertions and index validation match, using atomic storage. |
| `xStreamBufferSendCompletedFromISR`, `xStreamBufferReceiveCompletedFromISR` | same-named functions | **P/R**; interrupt masking, waiter exchange, indexed notification, and wake result match. |
| `prvInitialiseNewStreamBuffer` | `prvInitialiseNewStreamBuffer` | **P/R**; all fields, flags, callbacks, trace number, and notification index are initialized equivalently with atomics and `ptr::write`. |
| `prvTakeWaitingTask` | waiter load/clear sequences in completion macros | **R/P**; atomic swap implements the same single-consumer ownership transition. |
| `prvWriteBytesToBuffer`, `prvReadBytesFromBuffer` | same-named functions | **P/R**; two-part wrap copies and returned indices match without manufacturing out-of-allocation pointers. |
| `prvWriteMessageToBuffer`, `prvReadMessageFromBuffer` | same-named functions | **P/H**; prefix/payload order and too-small destination consumption rules match, with checked length conversion. |
| `sbSEND_COMPLETED`, `prvSEND_COMPLETED`, `sbSEND_COMPLETE_FROM_ISR`, `prvSEND_COMPLETE_FROM_ISR` | same-named macros | **P/R** for callback-enabled configuration; optional callback dispatch and default task notification match. |
| `sbRECEIVE_COMPLETED`, `prvRECEIVE_COMPLETED`, `sbRECEIVE_COMPLETED_FROM_ISR`, `prvRECEIVE_COMPLETED_FROM_ISR` | same-named macros | **P/R**. |
| `xStreamBufferGetStaticBuffers` | same-named function | **P, fixed in this audit**; both upstream-required output pointers are now asserted and written for a static object. |
| `xMessageBufferGetStaticBuffers`, `xMessageBufferSend`, `xMessageBufferSendFromISR`, `xMessageBufferReceive`, `xMessageBufferReceiveFromISR`, `vMessageBufferDelete` | same-named `message_buffer.h` macros | **P, completed in this audit**; previously missing raw header veneers now delegate exactly to stream-buffer APIs. |
| `xMessageBufferIsFull`, `xMessageBufferIsEmpty`, `xMessageBufferReset`, `xMessageBufferResetFromISR` | same-named macros | **P, completed in this audit**. |
| `xMessageBufferSpaceAvailable`, `xMessageBufferSpacesAvailable`, `xMessageBufferNextLengthBytes` | same-named macros | **P, completed in this audit**; both upstream space spellings are exposed. |
| `xMessageBufferSendCompletedFromISR`, `xMessageBufferReceiveCompletedFromISR` | same-named macros | **P, completed in this audit**. |
| `uxStreamBufferGetStreamBufferNumber`, `vStreamBufferSetStreamBufferNumber`, `ucStreamBufferGetStreamBufferType` | same-named functions | **P** under `trace-facility`. |

Known stream/message-buffer limitations:

- **D:** `configMESSAGE_BUFFER_LENGTH_TYPE` is fixed to `u32`; upstream permits
  an application-selected unsigned type.
- **D:** The crate fixes `configUSE_SB_COMPLETED_CALLBACK == 1`. Both the
  callback and null-callback/default-notification branches are implemented,
  but there is no smaller callback-fields-disabled layout.
- **D:** Upstream's debug-only `0x55` fill of caller storage during static
  creation is omitted. It is diagnostic only and does not change live buffer
  state.
- **D:** Stream-buffer trace macro call sites are not translated, except for
  stored trace number/type state. User-selectable tracing is therefore less
  complete than the default no-op upstream configuration.
- **D:** As with the other FromISR APIs, there is no common
  `portASSERT_IF_INTERRUPT_PRIORITY_INVALID()` hook.

## `src/memory/heap_4.rs`

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `BlockLink::new` | none | **R**; static zero-state construction. |
| `heap_block_size_is_valid`, `heap_block_is_allocated`, `heap_allocate_block`, `heap_free_block`, `heap_get_block_size` | `heapBLOCK_SIZE_IS_VALID`, `heapBLOCK_IS_ALLOCATED`, `heapALLOCATE_BLOCK`, `heapFREE_BLOCK`, size-mask expression | **P**. |
| `prv_heap_init` | `prvHeapInit` | **P**; start alignment, end marker, and initial free byte count checked. |
| `prv_insert_block_into_free_list` | `prvInsertBlockIntoFreeList` | **P**; previous/next/both-side coalescing checked. |
| `pvPortMalloc` | `pvPortMalloc` | **P/H**; checked arithmetic replaces the C overflow macros. First-fit removal, split threshold, counters, and ownership bit match. |
| `vPortFree` | `vPortFree` | **P/H** for valid pointers; Rust retains defensive allocation/link checks. |
| `xPortGetFreeHeapSize`, `xPortGetMinimumEverFreeHeapSize`, `xPortResetHeapMinimumEverFreeHeapSize` | same-named functions | **P**. |
| `pvPortCalloc` | `pvPortCalloc` | **P/H**; checked multiplication. |
| `vPortGetHeapStats` | `vPortGetHeapStats` | **P/H, fixed in this audit**; counter snapshot is again protected by a task critical section, and an empty free list reports upstream's `SIZE_MAX` smallest-block sentinel. Null output is rejected defensively. |
| `vPortInitialiseBlocks` | `vPortInitialiseBlocks` | **P**; intentional no-op. |
| `vPortHeapResetState` | `vPortHeapResetState` | **P**. |
| `prv_alloc_layout`, `prv_free_layout`, `FreeRtosAllocator::{alloc,dealloc,realloc}` | none | **R**; Rust `GlobalAlloc` veneer adds arbitrary-layout alignment metadata and allocate/copy/free reallocation. |

## `src/memory/heap_5.rs`

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `BlockLink::new`, `HeapRegion::new` | none | **R**; const Rust constructors. |
| `heap_block_size_is_valid`, `heap_block_is_allocated`, `heap_allocate_block`, `heap_free_block`, `heap_get_block_size` | same heap macros/size mask as `heap_5.c` | **P**. |
| `prv_region_bounds` | region-alignment/end calculations inside `vPortDefineHeapRegions` | **H**; validates null, tiny, wrapping, and alignment-loss cases before touching caller storage. |
| `vPortDefineHeapRegions` | `vPortDefineHeapRegions` | **P/H** for valid ordered regions. The Rust slice length replaces C's null sentinel; all regions are prevalidated and unusably tiny entries are skipped. |
| `prv_insert_block_into_free_list` | `prvInsertBlockIntoFreeList` | **P**; address ordering and the final-`pxEnd` coalescing exception match. |
| `pvPortMalloc` | `pvPortMalloc` | **P/H, fixed in this audit**; initialization assertion and checked size arithmetic retained, and split remainders are linked directly so intermediate zero-sized region markers remain exactly where upstream leaves them. |
| `vPortFree` | `vPortFree` | **P/H** for valid pointers. |
| `xPortGetFreeHeapSize`, `xPortGetMinimumEverFreeHeapSize`, `xPortResetHeapMinimumEverFreeHeapSize` | same-named functions | **P**. |
| `pvPortCalloc` | `pvPortCalloc` | **P/H**. |
| `vPortGetHeapStats` | `vPortGetHeapStats` | **P/H, fixed in this audit**; intermediate zero-sized region links are counted but excluded from the minimum, counter reads are protected, and the `SIZE_MAX` empty sentinel is preserved. |
| `vPortInitialiseBlocks` | none in `heap_5.c` | **R**; compatibility no-op directing callers to `vPortDefineHeapRegions`. |
| `vPortHeapResetState` | `vPortHeapResetState` | **P/H**; Rust additionally clears the start link before caller-owned regions may be reused. |
| `prv_alloc_layout`, `prv_free_layout`, `FreeRtosAllocator::{alloc,dealloc,realloc}` | none | **R**; same Rust `GlobalAlloc` veneer as heap_4. |

Allocator configuration notes:

- The audited active behavior corresponds to upstream defaults
  `configENABLE_HEAP_PROTECTOR == 0` and
  `configHEAP_CLEAR_MEMORY_ON_FREE == 0`; those optional branches are not
  exposed as crate features.
- Upstream `traceMALLOC`, `traceFREE`, and malloc-failed-hook behavior is a
  no-op in this crate's configured build. The allocation algorithms and
  counters are otherwise source-parity checked.

## `src/kernel/tasks.rs`

This audit covers the pinned `tasks.c`, `include/task.h`, and the task-related
list/port macros they invoke. It is a single-core audit: `configNUMBER_OF_CORES`
is fixed to one on this branch. Statement order was checked for ready/delayed/
overflow/pending/suspended/deleted list mutations, timeout snapshots, yield
latching, notification wakeups, priority inheritance, task construction,
diagnostic snapshots, and tickless-idle deadline maintenance.

### State, layout, and macro equivalents

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `taskSCHEDULER_SUSPENDED`, `taskSCHEDULER_NOT_STARTED`, `taskSCHEDULER_RUNNING` | same constants in `include/task.h` | **P, fixed in this audit**; restored the public numeric values `0`, `1`, and `2` respectively. |
| notification states, allocation-origin constants, task-state/report characters, `taskEVENT_LIST_ITEM_VALUE_IN_USE` | same constants/macros in `tasks.c` | **P** for 32- and 64-bit ticks. The crate has no 16-bit tick port. |
| `TimeOut_t`, `eTaskState`, `TCB_t` | `TimeOut_t`, `eTaskState`, `TCB_t` | **P/H/R**; active single-core fields and repr-C ordering match. Rust retains `uxStackDepth` to bound stack scans and uses volatile helper accesses for the upstream volatile notification arrays. |
| `TimeOut_t::new`, `TCB_t::new`, `StaticTask_t::new` | none | **R**; valid const construction replaces C zeroing/uninitialized static storage. |
| `taskRECORD_READY_PRIORITY`, `taskSELECT_HIGHEST_PRIORITY_TASK`, `taskRESET_READY_PRIORITY` | same-named macros | **P** for generic, non-port-optimized single-core selection. The reset macro is intentionally a no-op in this active upstream configuration. |
| `taskSWITCH_DELAYED_LISTS` | same-named macro | **P**; emptiness assertion, pointer swap, overflow count, and next-unblock reset order match. |
| `prvAddTaskToReadyList`, `prvGetTCBFromHandle` | same-named macro/helper | **P**. |
| `prvReadNotifiedValue`, `prvWriteNotifiedValue`, `prvReadNotifyState`, `prvWriteNotifyState` | volatile `TCB_t::ulNotifiedValue`/`ucNotifyState` accesses | **R/P, fixed in this audit**; explicit volatile operations preserve ISR-visible C field semantics without changing TCB layout. |
| `portYIELD_WITHIN_API`, `taskYIELD_ANY_CORE_IF_USING_PREEMPTION`, `vTaskYieldWithinAPI` | `taskYIELD_WITHIN_API`, `taskYIELD_ANY_CORE_IF_USING_PREEMPTION`, SMP `vTaskYieldWithinAPI` | **P/R**; active single-core yield points and strict higher-priority comparison match. The public `vTaskYieldWithinAPI` is retained as Rust compatibility glue. |
| `taskENTER_CRITICAL`, `taskEXIT_CRITICAL`, `taskENTER_CRITICAL_FROM_ISR`, `taskEXIT_CRITICAL_FROM_ISR` | same task/port macros | **P/R**; thin exported wrappers preserve the selected port's nesting/mask behavior. |

### Construction, scheduler core, delay, and event lists

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `prvInitialiseTaskLists`, `prvResetNextTaskUnblockTime` | same-named functions | **P**; all ready and state lists are initialized once and delayed-list pointers/reset values match. |
| `prvInitialiseNewTask` | same-named function | **P/H, fixed in this audit**; name/priority/list/notification/stack initialization order matches. Priority, alignment, and returned stack-pointer bounds are asserted; the aligned high address is recorded for downward stacks. |
| `prvAddNewTaskToReadyList` | same-named function | **P/H**; pre-start current-task selection, task generation, ready insertion, and post-start higher-priority yield match, with wrapping generation arithmetic. |
| `xTaskCreate` | `xTaskCreate`/`prvCreateTask` | **P/H, fixed in this audit**; downward-growing ports now allocate stack before TCB, preserving upstream's overflow-isolation property. Failure cleanup and checked size arithmetic match valid behavior. |
| `xTaskCreateStatic` | `xTaskCreateStatic`/`prvCreateStaticTask` | **P/H, fixed in this audit**; caller storage, aligned top/high address, construction, allocation origin, and ready insertion match, with null/zero/overflow hardening. |
| `xTaskGetStaticBuffers` | same-named function | **P/H, completed in this audit**; static-both, static-stack-only, and dynamic-origin results match with explicit output-pointer contracts. |
| `vTaskSetIdleTaskMemory`, `prvCreateIdleTasks` | `vApplicationGetIdleTaskMemory` integration and idle creation in `vTaskStartScheduler` | **R/P**; explicit pre-start registration replaces the C application hook; static, dynamic, and allocator-free fallback creation retain upstream ownership. |
| `prvIdleTask` | same-named function | **P/D, fixed in this audit**; deletion cleanup, conditional same-priority idle yield, and tickless ordering match. Cooperative scheduling and the application idle hook are inactive crate configurations. |
| `vTaskStartScheduler` | same-named function | **P/H**; idle/timer task creation, interrupt disable, tick/runtime initialization, and port start order match. Rust fail-stops on any system-task creation failure rather than returning after only the upstream allocation assertion. |
| `vTaskEndScheduler` | same-named function | **P, fixed in this audit**; timer and idle tasks are deleted and termination cleanup runs before interrupt/port shutdown. Actual return/restart remains port-dependent. |
| `vTaskResetState` | same-named function | **P/R, completed in this audit**; single-core task-module globals and Rust runtime-stat baseline are reset for a supported scheduler restart. |
| `vTaskSuspendAll`, `xTaskResumeAll` | same-named functions | **P, fixed in this audit**; nesting, barriers, pending-ready drain, strict priority comparison, next-unblock recalculation, pended-tick drain, and one-yield result match. Suspension no longer changes a pre-existing interrupt mask. |
| `prvAddCurrentTaskToDelayedList` | same-named function | **P, fixed in this audit**; ready removal, overflow/current delayed-list choice, and next-unblock update match. `portMAX_DELAY` becomes indefinite only with `task-suspend`, as upstream requires. |
| `vTaskDelay`, `xTaskDelayUntil` | same-named functions | **P, fixed in this audit**; wake-time arithmetic and scheduler pairing match, and `vTaskDelay` no longer yields a second time after `xTaskResumeAll` already yielded. |
| `xTaskGetSchedulerState`, `xTaskGetCurrentTaskHandle`, `xTaskGetCurrentTaskHandleForCore`, `uxTaskGetNumberOfTasks`, `pcTaskGetName` | same-named functions | **P/H, completed in this audit**; single-core core-ID validation and live-name-pointer behavior match. Scheduler-state numeric results are now ABI-correct. |
| `xTaskGetTickCount`, `xTaskGetTickCountFromISR` | same-named functions | **P** for sampled tick/masking semantics; the task form uses a conservative critical section on every host width. |
| `vTaskPlaceOnEventList`, `vTaskPlaceOnEventListRestricted`, `vTaskPlaceOnUnorderedEventList` | same-named functions | **P, fixed in this audit**; insertion type, timeout/indefinite behavior, and delayed-list mutation match. The unordered form again asserts scheduler suspension. |
| `xTaskRemoveFromEventList` | same-named function | **P, fixed in this audit**; event removal, direct/pending-ready path, tickless deadline reset, strict higher-priority result, and global pending-yield latch match. |
| `xTaskRemoveFromUnorderedEventList` | upstream `vTaskRemoveFromUnorderedEventList` | **P/R**; list/value/yield state matches. Rust retains an extra `BaseType_t` return used by earlier callers; upstream returns void. |
| `uxTaskResetEventItemValue` | same-named function | **P**; old value is returned before restoring the inverse-priority key. |
| `vTaskSetTimeOutState`, `vTaskInternalSetTimeOutState`, `xTaskCheckForTimeOut` | same-named functions | **P, fixed in this audit**; overflow snapshots, abort-delay handling, elapsed-time adjustment, and finite/indefinite `portMAX_DELAY` feature branch match. |
| `xTaskIncrementTick`, `vTaskMissedYield` | same-named functions | **P/H, fixed in this audit**; delayed wake order, strict higher-priority preemption, equal-priority time slicing, pending yield, and suspended tick accumulation match with wrapping tick counters. |
| `vTaskSwitchContext` | same-named function | **P/H/R**; suspended-switch deferral, runtime accounting, overflow hook, highest-ready selection, and trace ordering match. Runtime subtraction is explicitly wrapping. |

### Priority, deletion, suspension, and task-local facilities

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `pvTaskIncrementMutexHeldCount` | same-named function | **P/H**; null-before-first-task behavior and wrapping count match the unsigned C field. |
| `xTaskPriorityInherit`, `xTaskPriorityDisinherit`, `vTaskPriorityDisinheritAfterTimeout` | same-named functions | **P, fixed in this audit**; ready-list repositioning, event key updates, sole-mutex simplification, and return/yield semantics match. Ready-priority reset call sites are retained for future optimized selection. |
| `vTaskPriorityInherit` | none (compatibility wrapper over `xTaskPriorityInherit`) | **R**. |
| `uxTaskPriorityGet`, `uxTaskPriorityGetFromISR`, `uxTaskBasePriorityGet`, `uxTaskBasePriorityGetFromISR` | same-named functions | **P, fixed/completed in this audit**; task and ISR critical sections and inherited/base values match. |
| `vTaskPrioritySet` | same-named function | **P/H, fixed in this audit**; public priority assertion, inheritance-aware effective priority, event key, ready-list movement, and current-task yield match. |
| `eTaskGetState` | same-named function | **P, fixed in this audit**; now compiled whenever the crate's advertised `INCLUDE_eTaskGetState == 1`; pending-ready, delayed, indefinite-notification, suspended, deleted, and current states match using the critical-section snapshots. |
| `vTaskDelete`, `prvCheckTasksWaitingTermination`, `prvDeleteTCB` | same-named functions | **P, fixed in this audit**; state/event removal, immediate versus idle cleanup, counts, next-unblock reset, free origin, and self-delete yield match. Idle cleanup avoids entering a critical section when no work exists and frees outside each critical section. |
| `vTaskSuspend`, `prvTaskIsTaskSuspended`, `vTaskResume`, `xTaskResumeFromISR` | same-named functions | **P/H, fixed in this audit**; explicit suspend versus indefinite event/notification wait, pending-ready exclusion, list mutation, strict priority yield, and ISR return/latch behavior match. Null resume/suspend handles are asserted as upstream does. |
| `xTaskAbortDelay` | same-named function | **P**; scheduler suspension, state/event removal, abort flag, ready insertion, and deferred yield match. |
| `vTaskSetThreadLocalStoragePointer`, `pvTaskGetThreadLocalStoragePointer` | same-named functions | **P/H** under `thread-local-storage`; index/live-task checks are explicit. C delete callbacks are not configured. |
| `vTaskSetApplicationTaskTag`, `xTaskGetApplicationTaskTag`, `xTaskGetApplicationTaskTagFromISR`, `xTaskCallApplicationTaskHook` | same-named functions | **P/H** under `application-task-tag`; task/ISR exclusion and null-hook result match. |

### Direct notifications

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `eNotifyAction` | same-named enum | **P**. |
| `xTaskGenericNotify`, `xTaskGenericNotifyFromISR` | same-named functions | **P/H, fixed in this audit**; previous value, action semantics, unsigned increment wrap, notification state, blocked-list removal, event-list assertion, tickless deadline reset, and task/ISR yield outputs match. The integer action boundary now rejects non-enum values. |
| `xTaskNotify`, `xTaskNotifyIndexed`, `xTaskNotifyFromISR`, `xTaskNotifyIndexedFromISR` | same-named `include/task.h` macros | **P/R**; Rust functions provide the macro veneers over the generic implementations. |
| `xTaskNotifyGive`, `xTaskNotifyGiveIndexed`, `vTaskNotifyGiveFromISR`, `vTaskNotifyGiveIndexedFromISR` | same-named macros / `vTaskGenericNotifyGiveFromISR` | **P/R**; increment action and wake behavior share the audited generic path. |
| `xTaskGenericNotifyWait`, `xTaskNotifyWait`, `xTaskNotifyWaitIndexed` | same-named function/macros | **P/R, fixed in this audit**; outer volatile probe, race-closing critical recheck, bit clears, delayed insertion, resume/yield, result, and final state match. |
| `ulTaskNotifyTakeIndexed`, `ulTaskNotifyTake` | `ulTaskGenericNotifyTake` and macro veneer | **P/R, fixed in this audit**; volatile zero probe, recheck, block/yield, clear/decrement, and final state match. |
| `xTaskNotifyStateClearIndexed`, `xTaskNotifyStateClear` | `xTaskGenericNotifyStateClear` and macro | **P/R**. |
| `ulTaskNotifyValueClearIndexed`, `ulTaskNotifyValueClear` | `ulTaskGenericNotifyValueClear` and macro | **P/R**. |

### Diagnostics, stack checks, run time, and tickless idle

| Rust symbol(s) | Exact upstream symbol | Status and audit result |
| --- | --- | --- |
| `vTaskSetStackOverflowHook`, `prvCallApplicationStackOverflowHook`, `taskCHECK_FOR_STACK_OVERFLOW` | `vApplicationStackOverflowHook` and stack-overflow macros | **R/P/H**; explicit callback registration replaces the C link hook, preserves fail-stop behavior if absent, and stack probes are bounded by recorded allocation extent. |
| `prvTaskCheckFreeStackSpace`, `uxTaskGetStackHighWaterMark`, `uxTaskGetStackHighWaterMark2` | same-named function/APIs | **P/H**; fill-byte scan and word result match, bounded so a wholly untouched stack cannot cause an out-of-allocation read. |
| `uxTaskGetTaskNumber`, `vTaskSetTaskNumber` | same-named functions | **P, completed in this audit** under `trace-facility`; null-handle behavior matches. |
| `vTaskGetInfo`, `prvListTasksWithinSingleList`, `uxTaskGetSystemState` | same-named functions | **P/H/D, fixed in this audit**; current, indefinitely blocked, pending-ready, deleted-before-suspended enumeration, priorities, task number, stack mark, and runtime fields match. A null zero-task array no longer incurs Rust null-pointer arithmetic. See the deliberate `TaskStatus_t` shape limitation below. |
| `BufferWriter`, `prvGetTaskStateChar`, `prvGetTaskNameFromPtr`, `vTaskListTasks` | `vTaskListTasks` plus snprintf/name helpers | **R/H, fixed in this audit**; bounded no-std formatting deliberately differs textually, but snapshots/state characters match and every success, early-return, OOM, and truncation path retains a trailing NUL. Allocation uses fallible reserve. |
| `prvRunTimeDelta`, `prvGetTotalRunTime`, `ulTaskGetRunTimeCounter`, `ulTaskGetRunTimePercent`, `ulTaskGetIdleRunTimeCounter`, `ulTaskGetIdleRunTimePercent`, `ulTaskGetTotalRunTime` | runtime-stat accounting/query functions | **P/H/R**; switched-in intervals, current-task contribution, zero-total handling, and percentages match. Rust normalizes to the scheduler-start counter and handles hardware-counter wrap explicitly. |
| `vTaskGetRunTimeStatistics` | same-named function | **R/H, fixed in this audit**; task snapshot and percentages match semantically while bounded Rust formatting differs textually. Output is always NUL-terminated and allocation failure returns cleanly. |
| `prvGetExpectedIdleTime`, `eTaskConfirmSleepModeStatus`, `vTaskStepTick` | same-named functions | **P/H**; ready-priority checks, pending-ready/yield/tick aborts, all-indefinite-wait state, final-tick convention, critical section, and trace jump match with wrapping counters. |
| `xTaskCatchUpTicks` | same-named function | **P, completed in this audit**; unsuspended assertion, pended-count update, scheduler pairing, tick processing, and yield result match. |

Known task/scheduler limitations:

- **D:** SMP-only state and algorithms are intentionally absent from mainline:
  per-core current TCBs/yield counts, run state, core affinity, passive idle
  tasks, task locks, cross-core eviction, and SMP scheduler selection remain on
  the separate SMP line of work.
- **D:** MPU/restricted task creation and privilege-bit handling are not
  translated because no current port enables `portUSING_MPU_WRAPPERS`.
- **D:** Cooperative scheduling, time-slicing-off, port-optimized ready
  selection, task-level preemption disable, and positive-growth production
  ports are not exposed configurations. The active preemptive, time-sliced,
  generic-selection branches were audited.
- **D:** `INCLUDE_xTaskGetHandle` and `INCLUDE_xTaskGetIdleTaskHandle` are fixed
  to zero, so the corresponding name search and idle-handle API are omitted.
  Idle runtime queries use the internal idle handle.
- **D:** C runtime TLS delete callbacks, newlib reentrancy, POSIX errno,
  secure-context hooks, and port pre/post task-delete/setup-TCB hooks are not
  active crate capabilities.
- **D:** Idle/tick/daemon-startup application hooks and user-replaceable trace
  or coverage macros remain fixed no-ops. State-changing trace call positions
  used by the crate were checked separately.
- **D:** Rust `TaskStatus_t` omits upstream's conditional `pxTopOfStack` and
  `pxEndOfStack` diagnostic fields and currently stores
  `usStackHighWaterMark` as `u16` instead of `configSTACK_DEPTH_TYPE`.
  `vTaskGetInfo` deliberately saturates larger values rather than truncating.
  This affects diagnostic completeness, not scheduler state.
- **D:** As elsewhere, the common port layer has no equivalent of
  `portASSERT_IF_INTERRUPT_PRIORITY_INVALID()`. FromISR state/masking behavior
  matches, but an illegally high-priority ISR cannot be diagnosed centrally.
- **D:** Scheduler end/restart is source-parity complete in `tasks.rs`, but it
  is useful only on ports whose `vPortEndScheduler` actually restores a caller
  context. Most embedded ports deliberately never return from scheduler start.
