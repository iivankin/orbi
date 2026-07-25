// Orbi's in-process Apple profiler runtime.
//
// This file is compiled into a dependent dynamic library only for `--trace`
// builds. It avoids Foundation and Objective-C so malloc interposition can stay
// small and predictable before Swift/Obj-C runtimes have fully initialized.

#define _DARWIN_C_SOURCE 1

#include <dlfcn.h>
#include <errno.h>
#include <execinfo.h>
#include <inttypes.h>
#include <limits.h>
#include <mach/mach.h>
#include <mach/mach_time.h>
#include <mach-o/dyld.h>
#include <mach-o/loader.h>
#include <malloc/malloc.h>
#include <pthread.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/time.h>
#include <sys/types.h>
#include <unistd.h>

#if defined(__arm__) || defined(__arm64__)
#include <mach/arm/thread_status.h>
#include <ptrauth.h>
#endif

#if defined(__x86_64__) || defined(__i386__)
#include <mach/i386/thread_status.h>
#endif

#ifndef ORBI_TRACE_DEFAULT_MODE
#define ORBI_TRACE_DEFAULT_MODE "off"
#endif

#define ORBI_TRACE_FORMAT_VERSION 1
#define ORBI_TRACE_MAX_FRAMES 64U
#define ORBI_TRACE_DEFAULT_CPU_SAMPLE_CAP 200000U
#define ORBI_TRACE_DEFAULT_MEMORY_STACK_CAP 262144U
#define ORBI_TRACE_DEFAULT_ALLOCATION_CAP 1048576U
#define ORBI_TRACE_DEFAULT_PROCESS_SAMPLE_CAP 4096U
#define ORBI_TRACE_DEFAULT_SAMPLE_INTERVAL_US 4500U
#define ORBI_TRACE_THREAD_CAP 512U
#define ORBI_TRACE_THREAD_NAME_CAP 64U
#define ORBI_TRACE_DEFAULT_RELATIVE_PATH "Documents/orbi-trace.json"

typedef enum {
  ORBI_TRACE_MODE_OFF = 0,
  ORBI_TRACE_MODE_CPU = 1,
  ORBI_TRACE_MODE_MEMORY = 2,
} orbi_trace_mode_t;

typedef struct {
  uint64_t time_ns;
  uint64_t phys_footprint;
  uint64_t resident_size;
  uint64_t resident_size_peak;
  uint64_t virtual_size;
} orbi_process_memory_sample_t;

typedef struct {
  uint64_t time_ns;
  uint32_t thread_index;
  uint32_t frame_count;
  uintptr_t frames[ORBI_TRACE_MAX_FRAMES];
} orbi_cpu_sample_t;

typedef struct {
  thread_t mach_thread;
  char name[ORBI_TRACE_THREAD_NAME_CAP];
} orbi_thread_entry_t;

typedef struct {
  uint64_t hash;
  uint32_t frame_count;
  uint32_t used;
  uintptr_t frames[ORBI_TRACE_MAX_FRAMES];
  uint64_t total_allocated_bytes;
  uint64_t allocation_count;
  uint64_t live_bytes;
  uint64_t live_allocation_count;
  uint64_t peak_live_bytes;
} orbi_allocation_stack_t;

typedef struct {
  void *ptr;
  size_t size;
  uint32_t stack_index;
  uint8_t state;
} orbi_allocation_entry_t;

typedef struct {
  orbi_trace_mode_t mode;
  atomic_bool initialized;
  atomic_bool recording;
  atomic_bool finalizing;
  char output_path[4096];
  uint64_t started_unix_ns;
  uint64_t started_mach_ns;
  uint32_t sample_interval_us;
  pthread_t sampler_thread;
  pthread_t flush_thread;
  thread_t sampler_mach_thread;
  thread_t flush_mach_thread;
  pthread_mutex_t lock;

  orbi_thread_entry_t threads[ORBI_TRACE_THREAD_CAP];
  uint32_t thread_count;

  orbi_cpu_sample_t *cpu_samples;
  uint32_t cpu_sample_count;
  uint32_t cpu_sample_cap;
  uint64_t dropped_cpu_samples;
  uint64_t failed_cpu_unwinds;

  orbi_process_memory_sample_t *process_samples;
  uint32_t process_sample_count;
  uint32_t process_sample_cap;
  uint64_t dropped_process_samples;

  orbi_allocation_stack_t *allocation_stacks;
  uint32_t allocation_stack_cap;
  uint32_t allocation_stack_count;
  uint64_t dropped_allocation_stacks;

  orbi_allocation_entry_t *allocations;
  uint32_t allocation_cap;
  uint32_t allocation_count;
  uint64_t total_allocated_bytes;
  uint64_t allocation_events;
  uint64_t live_bytes;
  uint64_t live_allocation_count;
  uint64_t peak_live_bytes;
  uint64_t dropped_allocations;
  uint64_t failed_allocation_frees;
  uint64_t failed_trace_opens;
  uint64_t failed_trace_writes;
  uint64_t failed_trace_renames;
} orbi_trace_state_t;

typedef struct {
  uintptr_t load_address;
  uint64_t size;
  char uuid[37];
} orbi_loaded_image_metadata_t;

static orbi_trace_state_t g_orbi_trace;
static volatile sig_atomic_t g_orbi_trace_signal;
static __thread uint32_t g_orbi_trace_reentry_guard;

static bool orbi_trace_is_initialized(void) {
  return atomic_load_explicit(&g_orbi_trace.initialized, memory_order_acquire);
}

static bool orbi_trace_is_recording(void) {
  return atomic_load_explicit(&g_orbi_trace.recording, memory_order_acquire);
}

static void orbi_trace_set_recording(bool recording) {
  atomic_store_explicit(&g_orbi_trace.recording, recording, memory_order_release);
}

static bool orbi_trace_begin_finalizing(void) {
  if (!orbi_trace_is_initialized()) {
    return false;
  }
  bool expected = false;
  return atomic_compare_exchange_strong_explicit(&g_orbi_trace.finalizing, &expected, true,
                                                 memory_order_acq_rel, memory_order_acquire);
}

static malloc_zone_t *orbi_default_malloc_zone(void) {
  malloc_zone_t *zone = malloc_default_zone();
  return zone;
}

static malloc_zone_t *orbi_malloc_zone_for_ptr(void *ptr) {
  malloc_zone_t *zone = ptr != NULL ? malloc_zone_from_ptr(ptr) : NULL;
  if (zone != NULL) {
    return zone;
  }
  return orbi_default_malloc_zone();
}

static void *orbi_fallback_malloc(size_t size) {
  malloc_zone_t *zone = orbi_default_malloc_zone();
  return zone != NULL ? malloc_zone_malloc(zone, size) : NULL;
}

static void *orbi_fallback_calloc(size_t count, size_t size) {
  malloc_zone_t *zone = orbi_default_malloc_zone();
  return zone != NULL ? malloc_zone_calloc(zone, count, size) : NULL;
}

static void orbi_fallback_free(void *ptr);

static void *orbi_fallback_realloc(void *ptr, size_t size) {
  if (ptr == NULL) {
    return orbi_fallback_malloc(size);
  }
  if (size == 0) {
    orbi_fallback_free(ptr);
    return NULL;
  }
  malloc_zone_t *zone = orbi_malloc_zone_for_ptr(ptr);
  return zone != NULL ? malloc_zone_realloc(zone, ptr, size) : NULL;
}

static void orbi_fallback_free(void *ptr) {
  if (ptr == NULL) {
    return;
  }
  malloc_zone_t *zone = orbi_malloc_zone_for_ptr(ptr);
  if (zone != NULL) {
    malloc_zone_free(zone, ptr);
  }
}

static uint64_t orbi_wall_time_ns(void) {
  struct timeval tv;
  if (gettimeofday(&tv, NULL) != 0) {
    return 0;
  }
  return ((uint64_t)tv.tv_sec * 1000000000ULL) + ((uint64_t)tv.tv_usec * 1000ULL);
}

static uint64_t orbi_mach_time_ns(void) {
  static mach_timebase_info_data_t timebase;
  if (timebase.denom == 0) {
    (void)mach_timebase_info(&timebase);
    if (timebase.denom == 0) {
      return 0;
    }
  }
  const uint64_t now = mach_absolute_time();
  return (now * (uint64_t)timebase.numer) / (uint64_t)timebase.denom;
}

static uint32_t orbi_env_u32(const char *key, uint32_t default_value, uint32_t min_value) {
  const char *value = getenv(key);
  if (value == NULL || value[0] == '\0') {
    return default_value;
  }
  char *end = NULL;
  errno = 0;
  unsigned long parsed = strtoul(value, &end, 10);
  if (errno != 0 || end == value || *end != '\0' || parsed > UINT32_MAX || parsed < min_value) {
    return default_value;
  }
  return (uint32_t)parsed;
}

static uint32_t orbi_next_power_of_two_u32(uint32_t value) {
  if (value <= 2U) {
    return 2U;
  }
  value -= 1U;
  value |= value >> 1U;
  value |= value >> 2U;
  value |= value >> 4U;
  value |= value >> 8U;
  value |= value >> 16U;
  return value + 1U;
}

static bool orbi_string_eq(const char *lhs, const char *rhs) {
  return lhs != NULL && rhs != NULL && strcmp(lhs, rhs) == 0;
}

static orbi_trace_mode_t orbi_resolve_mode(void) {
  const char *mode = getenv("ORBI_TRACE_MODE");
  if (mode == NULL || mode[0] == '\0') {
    mode = ORBI_TRACE_DEFAULT_MODE;
  }
  if (orbi_string_eq(mode, "cpu")) {
    return ORBI_TRACE_MODE_CPU;
  }
  if (orbi_string_eq(mode, "memory") || orbi_string_eq(mode, "allocations")) {
    return ORBI_TRACE_MODE_MEMORY;
  }
  return ORBI_TRACE_MODE_OFF;
}

static void *orbi_mmap_array(size_t count, size_t element_size) {
  if (count == 0 || element_size == 0 || count > SIZE_MAX / element_size) {
    return NULL;
  }
  const size_t byte_count = count * element_size;
  void *memory = mmap(NULL, byte_count, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
  if (memory == MAP_FAILED) {
    return NULL;
  }
  return memory;
}

static void orbi_resolve_output_path(char out[4096]) {
  const char *explicit_path = getenv("ORBI_TRACE_OUTPUT");
  const char *path = explicit_path != NULL && explicit_path[0] != '\0'
                         ? explicit_path
                         : ORBI_TRACE_DEFAULT_RELATIVE_PATH;
  if (path[0] == '/') {
    (void)snprintf(out, 4096, "%s", path);
    return;
  }

  const char *home = getenv("HOME");
  if (home == NULL || home[0] == '\0') {
    (void)snprintf(out, 4096, "%s", path);
    return;
  }
  (void)snprintf(out, 4096, "%s/%s", home, path);
}

static void orbi_make_parent_dirs(const char *path) {
  char buffer[4096];
  (void)snprintf(buffer, sizeof(buffer), "%s", path);
  for (char *cursor = buffer + 1; *cursor != '\0'; cursor++) {
    if (*cursor != '/') {
      continue;
    }
    *cursor = '\0';
    (void)mkdir(buffer, 0755);
    *cursor = '/';
  }
}

static uint64_t orbi_hash_frames(const uintptr_t *frames, uint32_t frame_count) {
  uint64_t hash = 1469598103934665603ULL;
  for (uint32_t i = 0; i < frame_count; i++) {
    uint64_t value = (uint64_t)frames[i];
    for (uint32_t byte = 0; byte < 8U; byte++) {
      hash ^= (value >> (byte * 8U)) & 0xffU;
      hash *= 1099511628211ULL;
    }
  }
  return hash == 0 ? 1U : hash;
}

static bool orbi_frames_equal(const orbi_allocation_stack_t *entry,
                              const uintptr_t *frames,
                              uint32_t frame_count) {
  if (entry->frame_count != frame_count) {
    return false;
  }
  for (uint32_t i = 0; i < frame_count; i++) {
    if (entry->frames[i] != frames[i]) {
      return false;
    }
  }
  return true;
}

static uint32_t orbi_thread_index_locked(thread_t thread) {
  for (uint32_t i = 0; i < g_orbi_trace.thread_count; i++) {
    if (g_orbi_trace.threads[i].mach_thread == thread) {
      return i;
    }
  }
  if (g_orbi_trace.thread_count >= ORBI_TRACE_THREAD_CAP) {
    return UINT32_MAX;
  }
  const uint32_t index = g_orbi_trace.thread_count;
  g_orbi_trace.thread_count += 1U;
  g_orbi_trace.threads[index].mach_thread = thread;
  (void)snprintf(g_orbi_trace.threads[index].name, ORBI_TRACE_THREAD_NAME_CAP, "Thread %u", thread);
  pthread_t pthread = pthread_from_mach_thread_np(thread);
  if (pthread != NULL) {
    char name[ORBI_TRACE_THREAD_NAME_CAP];
    if (pthread_getname_np(pthread, name, sizeof(name)) == 0 && name[0] != '\0') {
      (void)snprintf(g_orbi_trace.threads[index].name, ORBI_TRACE_THREAD_NAME_CAP, "%s", name);
    }
  }
  return index;
}

static bool orbi_read_task_memory(vm_address_t address, void *destination, size_t size) {
  if (address < 4096U || destination == NULL || size == 0) {
    return false;
  }
  vm_size_t copied = 0;
  return vm_read_overwrite(mach_task_self(), address, (vm_size_t)size, (vm_address_t)destination,
                           &copied) == KERN_SUCCESS &&
         copied == size;
}

static uintptr_t orbi_strip_instruction_pointer(uintptr_t value) {
#if defined(__arm64__)
  return (uintptr_t)ptrauth_strip((void *)value, ptrauth_key_return_address);
#else
  return value;
#endif
}

static bool orbi_thread_state(thread_t thread, uintptr_t *pc, uintptr_t *fp, uintptr_t *lr) {
  if (pc == NULL || fp == NULL || lr == NULL) {
    return false;
  }
  *pc = 0;
  *fp = 0;
  *lr = 0;
#if defined(__arm64__)
  arm_thread_state64_t state;
  mach_msg_type_number_t count = ARM_THREAD_STATE64_COUNT;
  kern_return_t result = thread_get_state(thread, ARM_THREAD_STATE64, (thread_state_t)&state, &count);
  if (result != KERN_SUCCESS && result != 268435459) {
    return false;
  }
  *pc = orbi_strip_instruction_pointer((uintptr_t)arm_thread_state64_get_pc(state));
  *fp = (uintptr_t)arm_thread_state64_get_fp(state);
  *lr = orbi_strip_instruction_pointer((uintptr_t)arm_thread_state64_get_lr(state));
  return true;
#elif defined(__x86_64__)
  x86_thread_state64_t state;
  mach_msg_type_number_t count = x86_THREAD_STATE64_COUNT;
  kern_return_t result = thread_get_state(thread, x86_THREAD_STATE64, (thread_state_t)&state, &count);
  if (result != KERN_SUCCESS && result != 268435459) {
    return false;
  }
  *pc = (uintptr_t)state.__rip;
  *fp = (uintptr_t)state.__rbp;
  return true;
#elif defined(__i386__)
  x86_thread_state32_t state;
  mach_msg_type_number_t count = x86_THREAD_STATE32_COUNT;
  kern_return_t result = thread_get_state(thread, x86_THREAD_STATE32, (thread_state_t)&state, &count);
  if (result != KERN_SUCCESS && result != 268435459) {
    return false;
  }
  *pc = (uintptr_t)state.__eip;
  *fp = (uintptr_t)state.__ebp;
  return true;
#else
  (void)thread;
  return false;
#endif
}

static uint32_t orbi_unwind_suspended_thread(thread_t thread,
                                             uintptr_t frames[ORBI_TRACE_MAX_FRAMES]) {
  uintptr_t pc = 0;
  uintptr_t fp = 0;
  uintptr_t lr = 0;
  if (!orbi_thread_state(thread, &pc, &fp, &lr) || pc < 4096U) {
    return 0;
  }

  uint32_t count = 0;
  frames[count++] = pc;
#if defined(__arm64__)
  if (lr >= 4096U && count < ORBI_TRACE_MAX_FRAMES) {
    frames[count++] = lr;
  }
#endif

  uintptr_t previous_fp = fp;
  for (uint32_t i = 0; i < ORBI_TRACE_MAX_FRAMES && count < ORBI_TRACE_MAX_FRAMES; i++) {
    uintptr_t pair[2] = {0, 0};
    if (previous_fp < 4096U ||
        !orbi_read_task_memory((vm_address_t)previous_fp, pair, sizeof(pair))) {
      break;
    }
    const uintptr_t next_fp = pair[0];
    const uintptr_t return_address = orbi_strip_instruction_pointer(pair[1]);
    if (return_address >= 4096U) {
      frames[count++] = return_address;
    }
    if (next_fp <= previous_fp || next_fp - previous_fp > (64U * 1024U * 1024U)) {
      break;
    }
    previous_fp = next_fp;
  }
  return count;
}

static void orbi_record_process_memory_locked(uint64_t time_ns) {
  if (g_orbi_trace.process_sample_count >= g_orbi_trace.process_sample_cap) {
    g_orbi_trace.dropped_process_samples += 1U;
    return;
  }
  task_vm_info_data_t info;
  mach_msg_type_number_t count = TASK_VM_INFO_COUNT;
  kern_return_t result =
      task_info(mach_task_self(), TASK_VM_INFO, (task_info_t)&info, &count);
  if (result != KERN_SUCCESS) {
    return;
  }
  orbi_process_memory_sample_t *sample =
      &g_orbi_trace.process_samples[g_orbi_trace.process_sample_count++];
  sample->time_ns = time_ns;
  sample->phys_footprint = info.phys_footprint;
  sample->resident_size = info.resident_size;
  sample->resident_size_peak = info.resident_size_peak;
  sample->virtual_size = info.virtual_size;
}

static void orbi_record_cpu_sample_locked(thread_t thread,
                                          const uintptr_t frames[ORBI_TRACE_MAX_FRAMES],
                                          uint32_t frame_count,
                                          uint64_t time_ns) {
  if (frame_count == 0) {
    g_orbi_trace.failed_cpu_unwinds += 1U;
    return;
  }
  if (g_orbi_trace.cpu_sample_count >= g_orbi_trace.cpu_sample_cap) {
    g_orbi_trace.dropped_cpu_samples += 1U;
    return;
  }
  const uint32_t thread_index = orbi_thread_index_locked(thread);
  if (thread_index == UINT32_MAX) {
    g_orbi_trace.dropped_cpu_samples += 1U;
    return;
  }
  orbi_cpu_sample_t *sample = &g_orbi_trace.cpu_samples[g_orbi_trace.cpu_sample_count++];
  sample->time_ns = time_ns;
  sample->thread_index = thread_index;
  sample->frame_count = frame_count;
  memcpy(sample->frames, frames, sizeof(uintptr_t) * frame_count);
}

static bool orbi_thread_is_cpu_active(thread_t thread) {
  thread_basic_info_data_t info;
  mach_msg_type_number_t count = THREAD_BASIC_INFO_COUNT;
  if (thread_info(thread, THREAD_BASIC_INFO, (thread_info_t)&info, &count) != KERN_SUCCESS) {
    return true;
  }
  if ((info.flags & TH_FLAGS_IDLE) != 0) {
    return false;
  }
  if (info.run_state == TH_STATE_RUNNING || info.run_state == TH_STATE_UNINTERRUPTIBLE) {
    return true;
  }
  return info.cpu_usage > 0;
}

static void *orbi_cpu_sampler_main(void *context) {
  (void)context;
  g_orbi_trace.sampler_mach_thread = mach_thread_self();
  while (orbi_trace_is_recording()) {
    thread_act_array_t threads = NULL;
    mach_msg_type_number_t thread_count = 0;
    const uint64_t time_ns = orbi_mach_time_ns() - g_orbi_trace.started_mach_ns;

    if (task_threads(mach_task_self(), &threads, &thread_count) == KERN_SUCCESS) {
      for (mach_msg_type_number_t i = 0; i < thread_count; i++) {
        thread_t thread = threads[i];
        if (thread == g_orbi_trace.sampler_mach_thread ||
            thread == g_orbi_trace.flush_mach_thread) {
          continue;
        }
        if (!orbi_thread_is_cpu_active(thread)) {
          continue;
        }
        uintptr_t frames[ORBI_TRACE_MAX_FRAMES];
        uint32_t frame_count = 0;
        if (thread_suspend(thread) == KERN_SUCCESS) {
          frame_count = orbi_unwind_suspended_thread(thread, frames);
          (void)thread_resume(thread);
        }
        (void)pthread_mutex_lock(&g_orbi_trace.lock);
        orbi_record_cpu_sample_locked(thread, frames, frame_count, time_ns);
        (void)pthread_mutex_unlock(&g_orbi_trace.lock);
      }
      (void)vm_deallocate(mach_task_self(), (vm_address_t)threads,
                          (vm_size_t)(sizeof(thread_t) * thread_count));
    }

    (void)pthread_mutex_lock(&g_orbi_trace.lock);
    orbi_record_process_memory_locked(time_ns);
    (void)pthread_mutex_unlock(&g_orbi_trace.lock);

    usleep(g_orbi_trace.sample_interval_us);
  }
  return NULL;
}

static uint32_t orbi_capture_current_stack(uintptr_t frames[ORBI_TRACE_MAX_FRAMES]) {
  void *raw_frames[ORBI_TRACE_MAX_FRAMES + 2U];
  int count = backtrace(raw_frames, (int)(ORBI_TRACE_MAX_FRAMES + 2U));
  if (count <= 2) {
    return 0;
  }
  uint32_t written = 0;
  for (int i = 2; i < count && written < ORBI_TRACE_MAX_FRAMES; i++) {
    frames[written++] = orbi_strip_instruction_pointer((uintptr_t)raw_frames[i]);
  }
  return written;
}

static uint32_t orbi_intern_allocation_stack_locked(const uintptr_t *frames,
                                                    uint32_t frame_count) {
  if (frame_count == 0 || g_orbi_trace.allocation_stack_cap == 0) {
    g_orbi_trace.dropped_allocation_stacks += 1U;
    return UINT32_MAX;
  }
  const uint64_t hash = orbi_hash_frames(frames, frame_count);
  const uint32_t mask = g_orbi_trace.allocation_stack_cap - 1U;
  for (uint32_t probe = 0; probe < g_orbi_trace.allocation_stack_cap; probe++) {
    const uint32_t index = (uint32_t)((hash + probe) & mask);
    orbi_allocation_stack_t *entry = &g_orbi_trace.allocation_stacks[index];
    if (entry->used == 0U) {
      entry->used = 1U;
      entry->hash = hash;
      entry->frame_count = frame_count;
      memcpy(entry->frames, frames, sizeof(uintptr_t) * frame_count);
      g_orbi_trace.allocation_stack_count += 1U;
      return index;
    }
    if (entry->hash == hash && orbi_frames_equal(entry, frames, frame_count)) {
      return index;
    }
  }
  g_orbi_trace.dropped_allocation_stacks += 1U;
  return UINT32_MAX;
}

static uint32_t orbi_allocation_slot(void *ptr) {
  uintptr_t value = (uintptr_t)ptr;
  value ^= value >> 33U;
  value *= 0xff51afd7ed558ccdULL;
  value ^= value >> 33U;
  return (uint32_t)value & (g_orbi_trace.allocation_cap - 1U);
}

static int32_t orbi_find_allocation_locked(void *ptr) {
  if (ptr == NULL || g_orbi_trace.allocation_cap == 0) {
    return -1;
  }
  const uint32_t start = orbi_allocation_slot(ptr);
  for (uint32_t probe = 0; probe < g_orbi_trace.allocation_cap; probe++) {
    const uint32_t index = (start + probe) & (g_orbi_trace.allocation_cap - 1U);
    orbi_allocation_entry_t *entry = &g_orbi_trace.allocations[index];
    if (entry->state == 0U) {
      return -1;
    }
    if (entry->state == 1U && entry->ptr == ptr) {
      return (int32_t)index;
    }
  }
  return -1;
}

static bool orbi_insert_allocation_locked(void *ptr, size_t size, uint32_t stack_index) {
  if (ptr == NULL || size == 0 || stack_index == UINT32_MAX || g_orbi_trace.allocation_cap == 0) {
    return false;
  }
  const uint32_t start = orbi_allocation_slot(ptr);
  uint32_t tombstone = UINT32_MAX;
  for (uint32_t probe = 0; probe < g_orbi_trace.allocation_cap; probe++) {
    const uint32_t index = (start + probe) & (g_orbi_trace.allocation_cap - 1U);
    orbi_allocation_entry_t *entry = &g_orbi_trace.allocations[index];
    if (entry->state == 1U && entry->ptr == ptr) {
      return false;
    }
    if (entry->state == 2U && tombstone == UINT32_MAX) {
      tombstone = index;
      continue;
    }
    if (entry->state == 0U) {
      const uint32_t target = tombstone == UINT32_MAX ? index : tombstone;
      orbi_allocation_entry_t *target_entry = &g_orbi_trace.allocations[target];
      target_entry->ptr = ptr;
      target_entry->size = size;
      target_entry->stack_index = stack_index;
      target_entry->state = 1U;
      g_orbi_trace.allocation_count += 1U;
      return true;
    }
  }
  return false;
}

static void orbi_apply_allocation_locked(void *ptr, size_t size, uint32_t stack_index) {
  if (!orbi_insert_allocation_locked(ptr, size, stack_index)) {
    g_orbi_trace.dropped_allocations += 1U;
    return;
  }
  orbi_allocation_stack_t *stack = &g_orbi_trace.allocation_stacks[stack_index];
  stack->total_allocated_bytes += (uint64_t)size;
  stack->allocation_count += 1U;
  stack->live_bytes += (uint64_t)size;
  stack->live_allocation_count += 1U;
  if (stack->live_bytes > stack->peak_live_bytes) {
    stack->peak_live_bytes = stack->live_bytes;
  }

  g_orbi_trace.total_allocated_bytes += (uint64_t)size;
  g_orbi_trace.allocation_events += 1U;
  g_orbi_trace.live_bytes += (uint64_t)size;
  g_orbi_trace.live_allocation_count += 1U;
  if (g_orbi_trace.live_bytes > g_orbi_trace.peak_live_bytes) {
    g_orbi_trace.peak_live_bytes = g_orbi_trace.live_bytes;
  }
}

static void orbi_remove_allocation_locked(void *ptr) {
  int32_t index = orbi_find_allocation_locked(ptr);
  if (index < 0) {
    g_orbi_trace.failed_allocation_frees += 1U;
    return;
  }
  orbi_allocation_entry_t *entry = &g_orbi_trace.allocations[index];
  if (entry->stack_index < g_orbi_trace.allocation_stack_cap) {
    orbi_allocation_stack_t *stack = &g_orbi_trace.allocation_stacks[entry->stack_index];
    if (stack->live_bytes >= entry->size) {
      stack->live_bytes -= (uint64_t)entry->size;
    } else {
      stack->live_bytes = 0;
    }
    if (stack->live_allocation_count > 0) {
      stack->live_allocation_count -= 1U;
    }
  }
  if (g_orbi_trace.live_bytes >= entry->size) {
    g_orbi_trace.live_bytes -= (uint64_t)entry->size;
  } else {
    g_orbi_trace.live_bytes = 0;
  }
  if (g_orbi_trace.live_allocation_count > 0) {
    g_orbi_trace.live_allocation_count -= 1U;
  }
  entry->ptr = NULL;
  entry->size = 0;
  entry->stack_index = UINT32_MAX;
  entry->state = 2U;
  if (g_orbi_trace.allocation_count > 0) {
    g_orbi_trace.allocation_count -= 1U;
  }
}

static void orbi_record_allocation(void *ptr, size_t size) {
  if (!orbi_trace_is_recording() || g_orbi_trace.mode != ORBI_TRACE_MODE_MEMORY || ptr == NULL ||
      size == 0 || g_orbi_trace_reentry_guard > 0U) {
    return;
  }
  g_orbi_trace_reentry_guard += 1U;
  uintptr_t frames[ORBI_TRACE_MAX_FRAMES];
  const uint32_t frame_count = orbi_capture_current_stack(frames);
  (void)pthread_mutex_lock(&g_orbi_trace.lock);
  const uint32_t stack_index = orbi_intern_allocation_stack_locked(frames, frame_count);
  orbi_apply_allocation_locked(ptr, size, stack_index);
  (void)pthread_mutex_unlock(&g_orbi_trace.lock);
  g_orbi_trace_reentry_guard -= 1U;
}

static void orbi_record_free(void *ptr) {
  if (!orbi_trace_is_recording() || g_orbi_trace.mode != ORBI_TRACE_MODE_MEMORY || ptr == NULL ||
      g_orbi_trace_reentry_guard > 0U) {
    return;
  }
  g_orbi_trace_reentry_guard += 1U;
  (void)pthread_mutex_lock(&g_orbi_trace.lock);
  orbi_remove_allocation_locked(ptr);
  (void)pthread_mutex_unlock(&g_orbi_trace.lock);
  g_orbi_trace_reentry_guard -= 1U;
}

static void orbi_json_frames(FILE *file, const uintptr_t *frames, uint32_t frame_count) {
  fputc('[', file);
  for (uint32_t i = 0; i < frame_count; i++) {
    if (i > 0) {
      fputc(',', file);
    }
    (void)fprintf(file, "\"0x%llx\"", (unsigned long long)frames[i]);
  }
  fputc(']', file);
}

static void orbi_json_string(FILE *file, const char *value) {
  fputc('"', file);
  for (const char *cursor = value; cursor != NULL && *cursor != '\0'; cursor++) {
    switch (*cursor) {
      case '\\':
        fputs("\\\\", file);
        break;
      case '"':
        fputs("\\\"", file);
        break;
      case '\n':
        fputs("\\n", file);
        break;
      case '\r':
        fputs("\\r", file);
        break;
      case '\t':
        fputs("\\t", file);
        break;
      default:
        if ((unsigned char)*cursor < 0x20U) {
          (void)fprintf(file, "\\u%04x", (unsigned char)*cursor);
        } else {
          fputc(*cursor, file);
        }
        break;
    }
  }
  fputc('"', file);
}

static const char *orbi_trace_arch(void) {
#if defined(__arm64e__)
  return "arm64e";
#elif defined(__arm64__) || defined(__aarch64__)
  return "arm64";
#elif defined(__x86_64__)
  return "x86_64";
#elif defined(__i386__)
  return "i386";
#elif defined(__arm__)
  return "arm";
#else
  return "unknown";
#endif
}

static void orbi_uuid_to_string(const uint8_t uuid[16], char out[37]) {
  (void)snprintf(out, 37,
                 "%02x%02x%02x%02x-%02x%02x-%02x%02x-%02x%02x-"
                 "%02x%02x%02x%02x%02x%02x",
                 uuid[0], uuid[1], uuid[2], uuid[3], uuid[4], uuid[5], uuid[6], uuid[7],
                 uuid[8], uuid[9], uuid[10], uuid[11], uuid[12], uuid[13], uuid[14],
                 uuid[15]);
}

static const struct load_command *orbi_first_load_command(const struct mach_header *header) {
  if (header == NULL) {
    return NULL;
  }
#if defined(__LP64__)
  if (header->magic == MH_MAGIC_64) {
    return (const struct load_command *)((const uint8_t *)header + sizeof(struct mach_header_64));
  }
#endif
  if (header->magic == MH_MAGIC) {
    return (const struct load_command *)((const uint8_t *)header + sizeof(struct mach_header));
  }
  return NULL;
}

static bool orbi_update_loaded_image_range(orbi_loaded_image_metadata_t *metadata,
                                           uint64_t vmaddr,
                                           uint64_t vmsize,
                                           intptr_t slide,
                                           bool has_range) {
  if (metadata == NULL || vmsize == 0 || vmaddr > UINT64_MAX - vmsize) {
    return has_range;
  }
  uint64_t start = vmaddr;
  if (slide >= 0) {
    const uint64_t positive_slide = (uint64_t)slide;
    if (start > UINT64_MAX - positive_slide) {
      return has_range;
    }
    start += positive_slide;
  } else {
    const uint64_t negative_slide = (uint64_t)(-(slide + 1)) + 1U;
    if (start < negative_slide) {
      return has_range;
    }
    start -= negative_slide;
  }
  const uint64_t end = start + vmsize;
  if (start > (uint64_t)UINTPTR_MAX || end > (uint64_t)UINTPTR_MAX) {
    return has_range;
  }
  const uint64_t previous_start = (uint64_t)metadata->load_address;
  const uint64_t previous_end = has_range ? previous_start + metadata->size : 0;
  const uint64_t new_start = !has_range || start < previous_start ? start : previous_start;
  const uint64_t new_end = !has_range || end > previous_end ? end : previous_end;
  metadata->load_address = (uintptr_t)new_start;
  metadata->size = new_end - new_start;
  return true;
}

static bool orbi_loaded_image_metadata(const struct mach_header *header,
                                       intptr_t slide,
                                       orbi_loaded_image_metadata_t *metadata) {
  if (header == NULL || metadata == NULL) {
    return false;
  }
  memset(metadata, 0, sizeof(*metadata));
  const struct load_command *command = orbi_first_load_command(header);
  if (command == NULL) {
    return false;
  }
  orbi_loaded_image_metadata_t text_metadata;
  orbi_loaded_image_metadata_t fallback_metadata;
  memset(&text_metadata, 0, sizeof(text_metadata));
  memset(&fallback_metadata, 0, sizeof(fallback_metadata));
  char uuid[37] = {0};
  bool has_text_range = false;
  bool has_fallback_range = false;
  for (uint32_t index = 0; index < header->ncmds; index++) {
    if (command->cmdsize == 0) {
      break;
    }
    if (command->cmd == LC_UUID && command->cmdsize >= sizeof(struct uuid_command)) {
      const struct uuid_command *uuid_command = (const struct uuid_command *)command;
      orbi_uuid_to_string(uuid_command->uuid, uuid);
    }
#if defined(__LP64__)
    if (command->cmd == LC_SEGMENT_64 && command->cmdsize >= sizeof(struct segment_command_64)) {
      const struct segment_command_64 *segment = (const struct segment_command_64 *)command;
      if (segment->initprot != 0) {
        has_fallback_range = orbi_update_loaded_image_range(
            &fallback_metadata, segment->vmaddr, segment->vmsize, slide, has_fallback_range);
        if (strncmp(segment->segname, "__TEXT", sizeof(segment->segname)) == 0) {
          has_text_range = orbi_update_loaded_image_range(
              &text_metadata, segment->vmaddr, segment->vmsize, slide, has_text_range);
        }
      }
    }
#endif
    if (command->cmd == LC_SEGMENT && command->cmdsize >= sizeof(struct segment_command)) {
      const struct segment_command *segment = (const struct segment_command *)command;
      if (segment->initprot != 0) {
        has_fallback_range = orbi_update_loaded_image_range(
            &fallback_metadata, segment->vmaddr, segment->vmsize, slide, has_fallback_range);
        if (strncmp(segment->segname, "__TEXT", sizeof(segment->segname)) == 0) {
          has_text_range = orbi_update_loaded_image_range(
              &text_metadata, segment->vmaddr, segment->vmsize, slide, has_text_range);
        }
      }
    }
    command = (const struct load_command *)((const uint8_t *)command + command->cmdsize);
  }
  if (has_text_range) {
    *metadata = text_metadata;
  } else if (has_fallback_range) {
    *metadata = fallback_metadata;
  } else {
    return false;
  }
  memcpy(metadata->uuid, uuid, sizeof(metadata->uuid));
  return true;
}

static void orbi_write_loaded_libraries_json(FILE *file) {
  fputs("  \"loadedLibraries\":[", file);
  bool wrote_image = false;
  const uint32_t image_count = _dyld_image_count();
  for (uint32_t index = 0; index < image_count; index++) {
    const struct mach_header *header = _dyld_get_image_header(index);
    const char *name = _dyld_get_image_name(index);
    orbi_loaded_image_metadata_t metadata;
    if (name == NULL || name[0] == '\0' ||
        !orbi_loaded_image_metadata(header, _dyld_get_image_vmaddr_slide(index), &metadata)) {
      continue;
    }
    if (wrote_image) {
      fputc(',', file);
    }
    wrote_image = true;
    fputs("{\"path\":", file);
    orbi_json_string(file, name);
    if (metadata.uuid[0] != '\0') {
      fputs(",\"uuid\":", file);
      orbi_json_string(file, metadata.uuid);
    }
    (void)fprintf(file, ",\"loadAddress\":\"0x%llx\",\"size\":%" PRIu64 "}",
                  (unsigned long long)metadata.load_address, metadata.size);
  }
  fputs("],\n", file);
}

static void orbi_write_common_json(FILE *file) {
  char hostname[256] = {0};
  (void)gethostname(hostname, sizeof(hostname) - 1U);
  fputs("{\n", file);
  (void)fprintf(file, "  \"format\":\"orbi.trace.v1\",\n");
  (void)fprintf(file, "  \"formatVersion\":%u,\n", ORBI_TRACE_FORMAT_VERSION);
  fputs("  \"mode\":", file);
  orbi_json_string(file, g_orbi_trace.mode == ORBI_TRACE_MODE_CPU ? "cpu" : "memory");
  fputs(",\n", file);
  (void)fprintf(file, "  \"startedAtUnixNanos\":%" PRIu64 ",\n",
                g_orbi_trace.started_unix_ns);
  fputs("  \"host\":", file);
  orbi_json_string(file, hostname);
  fputs(",\n", file);
  fputs("  \"arch\":", file);
  orbi_json_string(file, orbi_trace_arch());
  fputs(",\n", file);
  orbi_write_loaded_libraries_json(file);
  (void)fprintf(file,
                "  \"runtime\":{\"failedFileOpens\":%" PRIu64
                ",\"failedFileWrites\":%" PRIu64 ",\"failedFileRenames\":%" PRIu64 "},\n",
                g_orbi_trace.failed_trace_opens, g_orbi_trace.failed_trace_writes,
                g_orbi_trace.failed_trace_renames);
}

static void orbi_write_process_samples_json(FILE *file) {
  fputs("    \"processMemorySamples\":[", file);
  for (uint32_t i = 0; i < g_orbi_trace.process_sample_count; i++) {
    const orbi_process_memory_sample_t *sample = &g_orbi_trace.process_samples[i];
    if (i > 0) {
      fputc(',', file);
    }
    (void)fprintf(file,
                  "{\"timeNanos\":%" PRIu64 ",\"physFootprint\":%" PRIu64
                  ",\"residentSize\":%" PRIu64 ",\"residentSizePeak\":%" PRIu64
                  ",\"virtualSize\":%" PRIu64 "}",
                  sample->time_ns, sample->phys_footprint, sample->resident_size,
                  sample->resident_size_peak, sample->virtual_size);
  }
  fputs("],\n", file);
}

static void orbi_write_cpu_json(FILE *file) {
  orbi_write_common_json(file);
  fputs("  \"cpu\":{\n", file);
  (void)fprintf(file, "    \"sampleIntervalMicros\":%u,\n", g_orbi_trace.sample_interval_us);
  (void)fprintf(file, "    \"droppedSamples\":%" PRIu64 ",\n",
                g_orbi_trace.dropped_cpu_samples);
  (void)fprintf(file, "    \"failedUnwinds\":%" PRIu64 ",\n",
                g_orbi_trace.failed_cpu_unwinds);
  orbi_write_process_samples_json(file);
  fputs("    \"threads\":[", file);
  for (uint32_t thread_index = 0; thread_index < g_orbi_trace.thread_count; thread_index++) {
    if (thread_index > 0) {
      fputc(',', file);
    }
    fputs("{\"id\":", file);
    (void)fprintf(file, "%u,\"name\":", g_orbi_trace.threads[thread_index].mach_thread);
    orbi_json_string(file, g_orbi_trace.threads[thread_index].name);
    fputs(",\"samples\":[", file);
    bool wrote_sample = false;
    for (uint32_t i = 0; i < g_orbi_trace.cpu_sample_count; i++) {
      const orbi_cpu_sample_t *sample = &g_orbi_trace.cpu_samples[i];
      if (sample->thread_index != thread_index) {
        continue;
      }
      if (wrote_sample) {
        fputc(',', file);
      }
      wrote_sample = true;
      (void)fprintf(file, "{\"timeNanos\":%" PRIu64 ",\"stack\":", sample->time_ns);
      orbi_json_frames(file, sample->frames, sample->frame_count);
      fputc('}', file);
    }
    fputs("]}", file);
  }
  fputs("]\n  }\n}\n", file);
}

static void orbi_write_memory_json(FILE *file) {
  orbi_write_common_json(file);
  fputs("  \"memory\":{\n", file);
  (void)fprintf(file,
                "    \"summary\":{\"totalAllocatedBytes\":%" PRIu64
                ",\"allocationEvents\":%" PRIu64 ",\"liveBytes\":%" PRIu64
                ",\"liveAllocations\":%" PRIu64 ",\"peakLiveBytes\":%" PRIu64 "},\n",
                g_orbi_trace.total_allocated_bytes, g_orbi_trace.allocation_events,
                g_orbi_trace.live_bytes, g_orbi_trace.live_allocation_count,
                g_orbi_trace.peak_live_bytes);
  (void)fprintf(file,
                "    \"dropped\":{\"allocationRecords\":%" PRIu64
                ",\"allocationStacks\":%" PRIu64 ",\"processSamples\":%" PRIu64
                ",\"unknownFrees\":%" PRIu64 "},\n",
                g_orbi_trace.dropped_allocations, g_orbi_trace.dropped_allocation_stacks,
                g_orbi_trace.dropped_process_samples, g_orbi_trace.failed_allocation_frees);
  orbi_write_process_samples_json(file);
  fputs("    \"stacks\":[", file);
  bool wrote_stack = false;
  for (uint32_t i = 0; i < g_orbi_trace.allocation_stack_cap; i++) {
    const orbi_allocation_stack_t *stack = &g_orbi_trace.allocation_stacks[i];
    if (stack->used == 0U) {
      continue;
    }
    if (wrote_stack) {
      fputc(',', file);
    }
    wrote_stack = true;
    fputs("{\"stack\":", file);
    orbi_json_frames(file, stack->frames, stack->frame_count);
    (void)fprintf(file,
                  ",\"totalAllocatedBytes\":%" PRIu64 ",\"allocationCount\":%" PRIu64
                  ",\"liveBytes\":%" PRIu64 ",\"liveAllocationCount\":%" PRIu64
                  ",\"peakLiveBytes\":%" PRIu64 "}",
                  stack->total_allocated_bytes, stack->allocation_count, stack->live_bytes,
                  stack->live_allocation_count, stack->peak_live_bytes);
  }
  fputs("]\n  }\n}\n", file);
}

static bool orbi_trace_temp_path(char out[4096], const char *path) {
  if (out == NULL || path == NULL || path[0] == '\0') {
    return false;
  }
  const int written = snprintf(out, 4096, "%s.tmp", path);
  return written > 0 && written < 4096;
}

static void orbi_write_trace_payload(FILE *file) {
  if (g_orbi_trace.mode == ORBI_TRACE_MODE_CPU) {
    orbi_write_cpu_json(file);
  } else if (g_orbi_trace.mode == ORBI_TRACE_MODE_MEMORY) {
    orbi_write_memory_json(file);
  }
}

static void orbi_write_trace_file(void) {
  if (!orbi_trace_is_initialized() || g_orbi_trace.output_path[0] == '\0') {
    return;
  }
  orbi_make_parent_dirs(g_orbi_trace.output_path);
  char temp_path[4096];
  if (!orbi_trace_temp_path(temp_path, g_orbi_trace.output_path)) {
    g_orbi_trace.failed_trace_writes += 1U;
    return;
  }
  FILE *file = fopen(temp_path, "w");
  if (file == NULL) {
    g_orbi_trace.failed_trace_opens += 1U;
    return;
  }

  orbi_write_trace_payload(file);
  bool failed = ferror(file) != 0;
  if (fflush(file) != 0) {
    failed = true;
  }
  const int fd = fileno(file);
  if (fd >= 0 && fsync(fd) != 0) {
    failed = true;
  }
  if (fclose(file) != 0) {
    failed = true;
  }

  if (failed) {
    g_orbi_trace.failed_trace_writes += 1U;
    (void)unlink(temp_path);
    return;
  }
  if (rename(temp_path, g_orbi_trace.output_path) != 0) {
    g_orbi_trace.failed_trace_renames += 1U;
    (void)unlink(temp_path);
  }
}

static void orbi_flush_locked(void) {
  orbi_record_process_memory_locked(orbi_mach_time_ns() - g_orbi_trace.started_mach_ns);
  orbi_write_trace_file();
}

static void orbi_reraise_signal_if_needed(void) {
  const int signal_number = (int)g_orbi_trace_signal;
  if (signal_number == 0) {
    return;
  }
  signal(signal_number, SIG_DFL);
  raise(signal_number);
}

static void *orbi_flush_thread_main(void *context) {
  (void)context;
  g_orbi_trace.flush_mach_thread = mach_thread_self();
  while (orbi_trace_is_recording()) {
    usleep(1000000U);
    if (!orbi_trace_is_initialized()) {
      continue;
    }
    g_orbi_trace_reentry_guard += 1U;
    (void)pthread_mutex_lock(&g_orbi_trace.lock);
    orbi_flush_locked();
    (void)pthread_mutex_unlock(&g_orbi_trace.lock);
    g_orbi_trace_reentry_guard -= 1U;
  }

  g_orbi_trace_reentry_guard += 1U;
  (void)pthread_mutex_lock(&g_orbi_trace.lock);
  orbi_flush_locked();
  (void)pthread_mutex_unlock(&g_orbi_trace.lock);
  g_orbi_trace_reentry_guard -= 1U;
  orbi_reraise_signal_if_needed();
  return NULL;
}

static void orbi_signal_handler(int signal_number) {
  if (orbi_trace_is_initialized() && orbi_trace_is_recording()) {
    g_orbi_trace_signal = signal_number;
    orbi_trace_set_recording(false);
    return;
  }
  signal(signal_number, SIG_DFL);
  raise(signal_number);
}

static void orbi_install_signal_handlers(void) {
  struct sigaction action;
  memset(&action, 0, sizeof(action));
  action.sa_handler = orbi_signal_handler;
  sigemptyset(&action.sa_mask);
  (void)sigaction(SIGINT, &action, NULL);
  (void)sigaction(SIGTERM, &action, NULL);
  (void)sigaction(SIGHUP, &action, NULL);
}

static void orbi_stop_and_flush(void) {
  if (!orbi_trace_begin_finalizing()) {
    return;
  }
  orbi_trace_set_recording(false);
  if (g_orbi_trace.mode == ORBI_TRACE_MODE_CPU && g_orbi_trace.sampler_thread != (pthread_t)0) {
    (void)pthread_join(g_orbi_trace.sampler_thread, NULL);
  }
  if (g_orbi_trace.flush_thread != (pthread_t)0 &&
      !pthread_equal(g_orbi_trace.flush_thread, pthread_self())) {
    (void)pthread_join(g_orbi_trace.flush_thread, NULL);
  }
  g_orbi_trace_reentry_guard += 1U;
  (void)pthread_mutex_lock(&g_orbi_trace.lock);
  orbi_flush_locked();
  (void)pthread_mutex_unlock(&g_orbi_trace.lock);
  g_orbi_trace_reentry_guard -= 1U;
}

static bool orbi_allocate_runtime_buffers(orbi_trace_mode_t mode) {
  g_orbi_trace.process_sample_cap =
      orbi_env_u32("ORBI_TRACE_PROCESS_SAMPLE_CAP", ORBI_TRACE_DEFAULT_PROCESS_SAMPLE_CAP, 2U);
  g_orbi_trace.process_samples =
      (orbi_process_memory_sample_t *)orbi_mmap_array(g_orbi_trace.process_sample_cap,
                                                      sizeof(orbi_process_memory_sample_t));
  if (g_orbi_trace.process_samples == NULL) {
    return false;
  }

  if (mode == ORBI_TRACE_MODE_CPU) {
    g_orbi_trace.cpu_sample_cap =
        orbi_env_u32("ORBI_TRACE_CPU_SAMPLE_CAP", ORBI_TRACE_DEFAULT_CPU_SAMPLE_CAP, 16U);
    g_orbi_trace.cpu_samples =
        (orbi_cpu_sample_t *)orbi_mmap_array(g_orbi_trace.cpu_sample_cap,
                                             sizeof(orbi_cpu_sample_t));
    return g_orbi_trace.cpu_samples != NULL;
  }

  if (mode == ORBI_TRACE_MODE_MEMORY) {
    g_orbi_trace.allocation_stack_cap = orbi_next_power_of_two_u32(orbi_env_u32(
        "ORBI_TRACE_MEMORY_STACK_CAP", ORBI_TRACE_DEFAULT_MEMORY_STACK_CAP, 16U));
    g_orbi_trace.allocation_cap = orbi_next_power_of_two_u32(
        orbi_env_u32("ORBI_TRACE_ALLOCATION_CAP", ORBI_TRACE_DEFAULT_ALLOCATION_CAP, 16U));
    g_orbi_trace.allocation_stacks =
        (orbi_allocation_stack_t *)orbi_mmap_array(g_orbi_trace.allocation_stack_cap,
                                                   sizeof(orbi_allocation_stack_t));
    g_orbi_trace.allocations =
        (orbi_allocation_entry_t *)orbi_mmap_array(g_orbi_trace.allocation_cap,
                                                   sizeof(orbi_allocation_entry_t));
    return g_orbi_trace.allocation_stacks != NULL && g_orbi_trace.allocations != NULL;
  }

  return false;
}

__attribute__((constructor)) static void orbi_trace_constructor(void) {
  const orbi_trace_mode_t mode = orbi_resolve_mode();
  if (mode == ORBI_TRACE_MODE_OFF) {
    return;
  }
  memset(&g_orbi_trace, 0, sizeof(g_orbi_trace));
  atomic_init(&g_orbi_trace.initialized, false);
  atomic_init(&g_orbi_trace.recording, false);
  atomic_init(&g_orbi_trace.finalizing, false);
  g_orbi_trace.mode = mode;
  g_orbi_trace.started_unix_ns = orbi_wall_time_ns();
  g_orbi_trace.started_mach_ns = orbi_mach_time_ns();
  g_orbi_trace.sample_interval_us =
      orbi_env_u32("ORBI_TRACE_SAMPLE_INTERVAL_US", ORBI_TRACE_DEFAULT_SAMPLE_INTERVAL_US, 500U);
  orbi_resolve_output_path(g_orbi_trace.output_path);

  pthread_mutexattr_t attr;
  (void)pthread_mutexattr_init(&attr);
  (void)pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_NORMAL);
  (void)pthread_mutex_init(&g_orbi_trace.lock, &attr);
  (void)pthread_mutexattr_destroy(&attr);

  if (!orbi_allocate_runtime_buffers(mode)) {
    return;
  }
  atomic_store_explicit(&g_orbi_trace.initialized, true, memory_order_release);
  orbi_trace_set_recording(true);
  orbi_install_signal_handlers();
  (void)pthread_mutex_lock(&g_orbi_trace.lock);
  orbi_record_process_memory_locked(0);
  (void)pthread_mutex_unlock(&g_orbi_trace.lock);

  if (mode == ORBI_TRACE_MODE_CPU) {
    (void)pthread_create(&g_orbi_trace.sampler_thread, NULL, orbi_cpu_sampler_main, NULL);
  }
  (void)pthread_create(&g_orbi_trace.flush_thread, NULL, orbi_flush_thread_main, NULL);
}

__attribute__((destructor)) static void orbi_trace_destructor(void) {
  orbi_stop_and_flush();
}

static void *orbi_trace_malloc(size_t size) {
  void *ptr = orbi_fallback_malloc(size);
  if (orbi_trace_is_initialized() && orbi_trace_is_recording() &&
      g_orbi_trace.mode == ORBI_TRACE_MODE_MEMORY) {
    orbi_record_allocation(ptr, size);
  }
  return ptr;
}

static void *orbi_trace_calloc(size_t count, size_t size) {
  void *ptr = orbi_fallback_calloc(count, size);
  if (orbi_trace_is_initialized() && orbi_trace_is_recording() &&
      g_orbi_trace.mode == ORBI_TRACE_MODE_MEMORY && ptr != NULL && count != 0 &&
      size <= SIZE_MAX / count) {
    orbi_record_allocation(ptr, count * size);
  }
  return ptr;
}

static void *orbi_trace_realloc(void *ptr, size_t size) {
  const bool should_record = orbi_trace_is_initialized() && orbi_trace_is_recording() &&
                             g_orbi_trace.mode == ORBI_TRACE_MODE_MEMORY;
  if (ptr == NULL) {
    void *new_ptr = orbi_fallback_realloc(NULL, size);
    if (should_record) {
      orbi_record_allocation(new_ptr, size);
    }
    return new_ptr;
  }
  if (size == 0) {
    if (should_record) {
      orbi_record_free(ptr);
    }
    return orbi_fallback_realloc(ptr, 0);
  }
  void *new_ptr = orbi_fallback_realloc(ptr, size);
  if (new_ptr != NULL) {
    if (should_record) {
      orbi_record_free(ptr);
      orbi_record_allocation(new_ptr, size);
    }
  }
  return new_ptr;
}

static void orbi_trace_free(void *ptr) {
  if (orbi_trace_is_initialized() && orbi_trace_is_recording() &&
      g_orbi_trace.mode == ORBI_TRACE_MODE_MEMORY) {
    orbi_record_free(ptr);
  }
  orbi_fallback_free(ptr);
}

typedef struct {
  const void *replacement;
  const void *replacee;
} orbi_interpose_entry_t;

__attribute__((used)) static const orbi_interpose_entry_t
    orbi_trace_interposers[] __attribute__((section("__DATA,__interpose"))) = {
        {(const void *)orbi_trace_malloc, (const void *)malloc},
        {(const void *)orbi_trace_calloc, (const void *)calloc},
        {(const void *)orbi_trace_realloc, (const void *)realloc},
        {(const void *)orbi_trace_free, (const void *)free},
};
